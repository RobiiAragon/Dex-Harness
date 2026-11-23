use std::io::{self, Write};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand, ValueEnum};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use harness_computer::{
    BrowserClickTool, BrowserExtractDomTool, BrowserNavigateTool, BrowserScreenshotTool,
    BrowserSession, BrowserTypeTool, DesktopKeyboardTool, DesktopMouseTool, DesktopScreenshotTool,
};
use harness_core::{
    Agent, AgentEvent, AgentStep, CoreError, EventBus, HardwareProfile, Hook, HookAction,
    HookRegistry, LlmProvider, LoadSkillTool, ManageTasksTool, ProgressiveSkillLoader, ReActAgent,
    ScheduleRegistry, SpawnSubagentTool, ToolRegistry,
};
use harness_llm::{OllamaProvider, OpenAiProvider};
use harness_mcp::McpServer;
use harness_tools::{
    EditFileTool, ExecuteShellTool, FindFilesTool, GitWorktreeTool, GrepSearchTool, PtyShellTool,
    ReadFileTool, WriteFileTool,
};
use harness_ui::{run_tui, start_web_dashboard};

mod models_cli;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExecutionMode {
    /// Fully autonomous execution without interactive confirmation prompts
    Autopilot,
    /// Safe mode: Prompts for human confirmation before running modifying, shell, or desktop tools
    Safe,
    /// Plan mode: Strictly read-only, prohibiting modifying tools or shell commands
    Plan,
}

#[derive(Parser, Debug)]
#[command(name = "dex")]
#[command(author = "Dex-Harness Contributors")]
#[command(version = "0.3.0")]
#[command(about = "High-Performance Autonomous Agent Harness in Rust", long_about = None)]
struct Cli {
    /// LLM inference provider (ollama, openai)
    #[arg(short, long, default_value = "ollama", env = "DEX_PROVIDER")]
    provider: String,

    /// Model identifier to use (e.g. qwen2.5:7b, llama3.1:8b, gpt-4o-mini)
    #[arg(short, long, default_value = "qwen2.5:7b", env = "DEX_MODEL")]
    model: String,

    /// Execution security mode (autopilot, safe, plan)
    #[arg(long, value_enum, default_value_t = ExecutionMode::Autopilot, env = "DEX_MODE")]
    mode: ExecutionMode,

    /// Optional API base URL override (e.g. http://localhost:11434 for Ollama)
    #[arg(long, env = "DEX_API_BASE")]
    api_base: Option<String>,

    /// Enable live streaming of reasoning tokens
    #[arg(long, default_value_t = true)]
    stream: bool,

    /// Enable verbose debug output
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Execute a single autonomous objective
    Run {
        /// The objective or task for the agent to accomplish
        #[arg(required = true)]
        goal: String,
    },
    /// Launch the interactive agent REPL
    Repl,
    /// Start as a native Model Context Protocol (MCP) server over stdio
    McpServe,
    /// Launch the live interactive Terminal UI (TUI) dashboard
    Tui,
    /// Launch the real-time embedded Web dashboard
    Web {
        /// Web server port
        #[arg(short, long, default_value_t = 3000)]
        port: u16,
    },
    /// Analyze host hardware and provide local LLM model size recommendations
    Cookbook,
    /// Manage scheduled autonomous tasks (.dex/schedules.json)
    Schedule {
        #[command(subcommand)]
        action: ScheduleCommands,
    },
    /// Run background daemon executing scheduled tasks
    Daemon,
    /// Local Model Hub: Search, pull, import, and manage local models (Hugging Face GGUF / Ollama)
    Models {
        #[command(subcommand)]
        action: ModelCommands,
    },
}

#[derive(Subcommand, Debug)]
enum ModelCommands {
    /// Search GGUF models on Hugging Face Hub
    Search {
        /// Search query (e.g. qwen, llama, deepseek)
        query: String,
        /// Maximum number of repositories to return
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// Download a model from Hugging Face or Ollama
    Pull {
        /// Model identifier (e.g. Qwen/Qwen2.5-Coder-14B-Instruct-GGUF or ollama:qwen2.5:7b)
        source: String,
        /// Preferred quantization (e.g. Q4_K_M, Q5_K_M, Q8_0)
        #[arg(short, long, default_value = "Q4_K_M")]
        quant: String,
    },
    /// Import a local .gguf file into Dex registry (and optionally register into Ollama)
    Import {
        /// Path to the .gguf file on disk
        path: String,
        /// Alias name for the model
        #[arg(short, long)]
        name: Option<String>,
        /// Also register the imported model in the local Ollama instance
        #[arg(long, default_value_t = false)]
        to_ollama: bool,
    },
    /// List all local models in Dex registry and Ollama
    List,
    /// Remove a model from the Dex registry and disk
    Rm {
        /// Model identifier to remove (e.g. local:my-model, hf:..., or ollama:...)
        model_id: String,
    },
}

#[derive(Subcommand, Debug)]
enum ScheduleCommands {
    /// List all scheduled tasks
    List,
    /// Add a new scheduled task
    Add {
        /// Short task name
        #[arg(short, long)]
        name: String,
        /// Autonomous goal for the agent
        #[arg(short, long)]
        goal: String,
        /// Execution interval in seconds
        #[arg(short, long, default_value_t = 3600)]
        interval: u64,
    },
    /// Remove a scheduled task by ID
    Remove {
        /// Task ID to remove
        #[arg(required = true)]
        id: String,
    },
}

/// Security guard hook enforcing Autopilot, Safe, or Plan restrictions across all sensitive tools.
struct SecurityHook {
    mode: ExecutionMode,
}

impl SecurityHook {
    fn is_modifying_or_sensitive(tool_name: &str) -> bool {
        matches!(
            tool_name,
            "write_file"
                | "edit_file"
                | "execute_shell"
                | "pty_shell"
                | "git_worktree"
                | "desktop_mouse"
                | "desktop_keyboard"
                | "browser_click"
                | "browser_type"
        )
    }
}

#[async_trait]
impl Hook for SecurityHook {
    async fn on_event(&self, event: &AgentEvent) -> Result<HookAction, CoreError> {
        if let AgentEvent::PreToolUse {
            ref tool_name,
            ref arguments,
        } = event
        {
            if Self::is_modifying_or_sensitive(tool_name) {
                match self.mode {
                    ExecutionMode::Autopilot => Ok(HookAction::Continue),
                    ExecutionMode::Plan => {
                        let reason = format!(
                            "Execution halted: tool '{tool_name}' is blocked in Plan mode (read-only mode active)"
                        );
                        Ok(HookAction::Halt(reason))
                    }
                    ExecutionMode::Safe => {
                        println!("\n⚠️  [SAFE MODE] Permission requested to execute tool '{}'", tool_name);
                        println!("   Arguments: {}", arguments);
                        print!("   Allow execution? [y/N]: ");
                        let _ = io::stdout().flush();

                        let mut answer = String::new();
                        if io::stdin().read_line(&mut answer).is_err() {
                            return Ok(HookAction::Halt("Failed to read user input".to_string()));
                        }

                        let trimmed = answer.trim();
                        if trimmed.eq_ignore_ascii_case("y") || trimmed.eq_ignore_ascii_case("yes") {
                            Ok(HookAction::Continue)
                        } else {
                            Ok(HookAction::Halt(format!(
                                "User denied authorization to execute tool '{tool_name}'"
                            )))
                        }
                    }
                }
            } else {
                Ok(HookAction::Continue)
            }
        } else {
            Ok(HookAction::Continue)
        }
    }
}

fn build_provider(provider_type: &str, model: &str, api_base: Option<&str>) -> Result<Arc<dyn LlmProvider>> {
    match provider_type.to_lowercase().as_str() {
        "ollama" => {
            let mut p = OllamaProvider::new(model);
            if let Some(base) = api_base {
                p = p.with_base_url(base);
            }
            Ok(Arc::new(p))
        }
        "openai" => {
            let mut p = OpenAiProvider::new(model);
            if let Some(base) = api_base {
                p = p.with_base_url(base);
            }
            Ok(Arc::new(p))
        }
        other => anyhow::bail!("Unsupported provider '{other}'. Supported: ollama, openai"),
    }
}

fn build_tools(
    provider: Arc<dyn LlmProvider>,
    skill_loader: ProgressiveSkillLoader,
    event_bus: EventBus,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    // File inspection & surgical manipulation
    registry.register(Arc::new(ReadFileTool::new()));
    registry.register(Arc::new(WriteFileTool::new()));
    registry.register(Arc::new(EditFileTool::new()));

    // Search & repository navigation
    registry.register(Arc::new(FindFilesTool::new()));
    registry.register(Arc::new(GrepSearchTool::new()));

    // Terminal execution
    registry.register(Arc::new(ExecuteShellTool::new()));
    registry.register(Arc::new(PtyShellTool::new()));

    // Git branch isolation
    registry.register(Arc::new(GitWorktreeTool::new()));

    // Project & Task graph management
    registry.register(Arc::new(
        ManageTasksTool::new(".dex/tasks.json").with_event_bus(event_bus.clone()),
    ));

    // Progressive skills
    registry.register(Arc::new(LoadSkillTool::new(skill_loader)));

    // Computer Use: Browser Automation
    let browser_session = BrowserSession::new();
    registry.register(Arc::new(BrowserNavigateTool::new(browser_session.clone())));
    registry.register(Arc::new(BrowserExtractDomTool::new(browser_session.clone())));
    registry.register(Arc::new(BrowserClickTool::new(browser_session.clone())));
    registry.register(Arc::new(BrowserTypeTool::new(browser_session.clone())));
    registry.register(Arc::new(BrowserScreenshotTool::new(browser_session)));

    // Computer Use: Desktop Automation
    registry.register(Arc::new(DesktopScreenshotTool::new()));
    registry.register(Arc::new(DesktopMouseTool::new()));
    registry.register(Arc::new(DesktopKeyboardTool::new()));

    // Subagents orchestrator (inheriting master tools and event bus)
    let master_copy = registry.clone();
    registry.register(Arc::new(
        SpawnSubagentTool::new(provider, master_copy).with_event_bus(event_bus),
    ));

    registry
}

fn print_step(step: &AgentStep) {
    println!("\n━━━ [Step {}] ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━", step.iteration);
    if let Some(ref thought) = step.thought {
        if !thought.trim().is_empty() {
            println!("\n🧠 Thought:\n{thought}");
        }
    }

    for (call, result) in step.tool_calls.iter().zip(step.tool_results.iter()) {
        println!("\n🛠️  Tool Call: {}", call.name);
        println!("   Arguments: {}", call.arguments);
        if result.is_error {
            println!("   Status: ❌ FAILED");
        } else {
            println!("   Status: ✅ SUCCESS");
        }
        let preview = if result.output.len() > 1000 {
            format!("{}...\n[Output truncated: {} bytes total]", &result.output[..1000], result.output.len())
        } else {
            result.output.clone()
        };
        println!("   Output:\n{preview}");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize structured tracing
    let default_filter = if cli.verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter)),
        )
        .init();

    let event_bus = EventBus::default();

    // Check standalone utility commands first
    match cli.command {
        Some(Commands::Cookbook) => {
            let profile = HardwareProfile::detect();
            println!("{}", profile.generate_report());
            return Ok(());
        }
        Some(Commands::Tui) => {
            let mode_str = format!("{:?}", cli.mode);
            run_tui(event_bus, &mode_str).await.context("Failed to run TUI")?;
            return Ok(());
        }
        Some(Commands::Web { port }) => {
            start_web_dashboard(event_bus, port).await.map_err(|e| anyhow::anyhow!(e))?;
            return Ok(());
        }
        Some(Commands::Schedule { action }) => {
            let mut reg = ScheduleRegistry::load_or_default(ScheduleRegistry::DEFAULT_PATH).await;
            match action {
                ScheduleCommands::List => {
                    println!("📋 Scheduled Autonomous Tasks (.dex/schedules.json):");
                    if reg.tasks.is_empty() {
                        println!("  No tasks scheduled.");
                    } else {
                        for t in &reg.tasks {
                            println!(
                                "  - [{}] '{}' (every {}s, enabled: {}) -> {}",
                                t.id, t.name, t.interval_seconds, t.enabled, t.goal
                            );
                        }
                    }
                }
                ScheduleCommands::Add { name, goal, interval } => {
                    let id = reg.add(name, goal, interval);
                    reg.save().await.context("Failed to save schedule")?;
                    println!("✅ Added schedule task '{id}'.");
                }
                ScheduleCommands::Remove { id } => {
                    if reg.remove(&id) {
                        reg.save().await.context("Failed to save schedule")?;
                        println!("✅ Removed schedule task '{id}'.");
                    } else {
                        println!("❌ Schedule task '{id}' not found.");
                    }
                }
            }
            return Ok(());
        }
        Some(Commands::Models { action }) => {
            match action {
                ModelCommands::Search { query, limit } => {
                    models_cli::search_models(&query, limit).await?;
                }
                ModelCommands::Pull { source, quant } => {
                    models_cli::pull_model(&source, &quant).await?;
                }
                ModelCommands::Import { path, name, to_ollama } => {
                    models_cli::import_model(&path, name.as_deref(), to_ollama).await?;
                }
                ModelCommands::List => {
                    models_cli::list_models().await?;
                }
                ModelCommands::Rm { model_id } => {
                    models_cli::remove_model(&model_id).await?;
                }
            }
            return Ok(());
        }
        Some(Commands::McpServe) => {
            let mut skill_loader = ProgressiveSkillLoader::new();
            skill_loader.scan_standard_locations().await;

            let provider = build_provider(&cli.provider, &cli.model, cli.api_base.as_deref())?;
            let tools = build_tools(provider, skill_loader, event_bus);

            let server = McpServer::new(tools);
            server.run_stdio().await.context("MCP stdio server terminated with error")?;
            return Ok(());
        }
        _ => {}
    }

    // Agent execution commands
    let mut skill_loader = ProgressiveSkillLoader::new();
    skill_loader.scan_standard_locations().await;

    let provider = build_provider(&cli.provider, &cli.model, cli.api_base.as_deref())
        .context("Failed to configure LLM provider")?;
    let tools = build_tools(provider.clone(), skill_loader.clone(), event_bus.clone());

    let mut hooks = HookRegistry::new();
    hooks.register(Arc::new(SecurityHook { mode: cli.mode }));

    let skills_index = skill_loader.generate_prompt_index();
    let base_system_prompt = format!(
        "You are Dex, a world-class autonomous software engineering and systems agent.\n\
         You operate with high precision, surgical file edits, computer vision perception, and safety awareness.\n\
         Mode: {:?}\n\
         {}\n\
         Track complex objectives using `manage_tasks` (.dex/tasks.json).\n\
         Delegate specialized tasks using `spawn_subagent` (Explorer, Coder, Reviewer).\n\
         You can inspect web pages with `browser_*` and take desktop screenshots with `desktop_*`.",
        cli.mode, skills_index
    );

    match cli.command {
        Some(Commands::Run { goal }) => {
            println!("🚀 Dex-Harness (Provider: {}, Model: {}, Mode: {:?})", cli.provider, cli.model, cli.mode);
            println!("🎯 Objective: {goal}");

            let observer = Arc::new(|step: &AgentStep| {
                print_step(step);
            });

            let mut agent = ReActAgent::new(provider, tools)
                .with_system_prompt(base_system_prompt)
                .with_observer(observer)
                .with_hooks(hooks)
                .with_event_bus(event_bus);

            if cli.stream {
                let stream_cb = Arc::new(|token: &str| {
                    print!("{token}");
                    let _ = io::stdout().flush();
                });
                agent = agent.with_token_callback(stream_cb);
            }

            match agent.run(&goal).await {
                Ok(answer) => {
                    println!("\n════════════════════════════════════════════════════════");
                    println!("🏁 Final Answer:\n\n{answer}");
                    println!("════════════════════════════════════════════════════════");
                }
                Err(e) => {
                    eprintln!("\n❌ Execution failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Daemon) => {
            println!("🤖 Dex-Harness Background Scheduler Daemon started...");
            let mut reg = ScheduleRegistry::load_or_default(ScheduleRegistry::DEFAULT_PATH).await;

            loop {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                let due_tasks: Vec<(String, String)> = reg
                    .tasks
                    .iter()
                    .filter(|t| t.should_run(now))
                    .map(|t| (t.id.clone(), t.goal.clone()))
                    .collect();

                for (id, goal) in due_tasks {
                    info!(task_id = %id, goal = %goal, "Executing scheduled autonomous task");
                    let mut agent = ReActAgent::new(provider.clone(), tools.clone())
                        .with_system_prompt(base_system_prompt.clone())
                        .with_hooks(hooks.clone())
                        .with_event_bus(event_bus.clone());

                    match agent.run(&goal).await {
                        Ok(ans) => {
                            info!(task_id = %id, "Scheduled task finished: {}", &ans[..ans.len().min(100)]);
                        }
                        Err(e) => {
                            error!(task_id = %id, "Scheduled task error: {e}");
                        }
                    }

                    reg.mark_executed(&id, now);
                    let _ = reg.save().await;
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            }
        }
        Some(Commands::Repl) | None => {
            println!("========================================================");
            println!("       🤖 Dex-Harness Interactive REPL (dex)            ");
            println!("  Provider: {} | Model: {} | Mode: {:?}", cli.provider, cli.model, cli.mode);
            println!("  Type 'exit' or 'quit' to leave, or enter your goal.   ");
            println!("========================================================\n");

            let observer = Arc::new(|step: &AgentStep| {
                print_step(step);
            });

            let mut agent = ReActAgent::new(provider, tools)
                .with_system_prompt(base_system_prompt)
                .with_observer(observer)
                .with_hooks(hooks)
                .with_event_bus(event_bus);

            if cli.stream {
                let stream_cb = Arc::new(|token: &str| {
                    print!("{token}");
                    let _ = io::stdout().flush();
                });
                agent = agent.with_token_callback(stream_cb);
            }

            loop {
                print!("\ndex> ");
                io::stdout().flush().context("Failed to flush stdout")?;

                let mut input = String::new();
                if io::stdin().read_line(&mut input)? == 0 {
                    break;
                }

                let trimmed = input.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed.eq_ignore_ascii_case("exit") || trimmed.eq_ignore_ascii_case("quit") {
                    println!("Goodbye!");
                    break;
                }

                match agent.run(trimmed).await {
                    Ok(answer) => {
                        println!("\n🏁 Answer:\n{answer}");
                    }
                    Err(e) => {
                        eprintln!("\n❌ Agent error: {e}");
                    }
                }
            }
        }
        _ => {}
    }

    Ok(())
}
