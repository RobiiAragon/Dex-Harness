use std::io::{self, Write};
use std::sync::Arc;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use harness_core::{Agent, AgentStep, LlmProvider, ReActAgent, ToolRegistry};
use harness_llm::{OllamaProvider, OpenAiProvider};
use harness_mcp as _;
use harness_tools::{ExecuteShellTool, ReadFileTool};

#[derive(Parser, Debug)]
#[command(name = "dex")]
#[command(author = "Dex-Harness Contributors")]
#[command(version = "0.1.0")]
#[command(about = "High-Performance Autonomous Agent Harness in Rust", long_about = None)]
struct Cli {
    /// LLM inference provider (ollama, openai)
    #[arg(short, long, default_value = "ollama", env = "DEX_PROVIDER")]
    provider: String,

    /// Model identifier to use (e.g. qwen2.5:7b, llama3.1:8b, gpt-4o-mini)
    #[arg(short, long, default_value = "qwen2.5:7b", env = "DEX_MODEL")]
    model: String,

    /// Optional API base URL override (e.g. http://localhost:11434 for Ollama)
    #[arg(long, env = "DEX_API_BASE")]
    api_base: Option<String>,

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

fn build_tools() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadFileTool::new()));
    registry.register(Arc::new(ExecuteShellTool::new()));
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

    let provider = build_provider(&cli.provider, &cli.model, cli.api_base.as_deref())
        .context("Failed to configure LLM provider")?;
    let tools = build_tools();

    match cli.command {
        Some(Commands::Run { goal }) => {
            println!("🚀 Dex-Harness (Provider: {}, Model: {})", cli.provider, cli.model);
            println!("🎯 Objective: {goal}");

            let observer = Arc::new(|step: &AgentStep| {
                print_step(step);
            });

            let mut agent = ReActAgent::new(provider, tools).with_observer(observer);

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
        Some(Commands::Repl) | None => {
            println!("========================================================");
            println!("       🤖 Dex-Harness Interactive REPL (dex)            ");
            println!("  Provider: {} | Model: {}", cli.provider, cli.model);
            println!("  Type 'exit' or 'quit' to leave, or enter your goal.   ");
            println!("========================================================\n");

            let observer = Arc::new(|step: &AgentStep| {
                print_step(step);
            });

            let mut agent = ReActAgent::new(provider, tools).with_observer(observer);

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
    }

    Ok(())
}
