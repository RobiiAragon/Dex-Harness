//! Interactive agent session: REPL with slash commands, Ctrl-C interruption, and inline
//! permission prompts (`y` / `n` / `a` = always allow this tool for the session).

use std::collections::HashSet;
use std::io::{IsTerminal, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::{mpsc, Mutex, RwLock};

use harness_core::{
    Agent, AgentEvent, CoreError, EventBus, Hook, HookAction, HookRegistry, LlmProvider, ReActAgent,
    ToolRegistry,
};

use crate::endpoint::{list_chat_models, CliConfig, Endpoint};
use crate::ui::{strip_think, StreamPrinter, Style, ToolActivityHook};
use crate::ExecutionMode;

/// Line-oriented stdin shared by the REPL prompt and permission prompts. A dedicated thread
/// does the blocking reads so async code can `select!` on Ctrl-C while waiting for input.
pub struct Input {
    rx: Mutex<mpsc::UnboundedReceiver<String>>,
    pub interactive: bool,
}

impl Input {
    pub fn spawn() -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let interactive = std::io::stdin().is_terminal();
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            loop {
                let mut line = String::new();
                match stdin.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Arc::new(Self {
            rx: Mutex::new(rx),
            interactive,
        })
    }

    /// Next line from stdin (without the trailing newline); `None` on EOF.
    pub async fn next_line(&self) -> Option<String> {
        self.rx
            .lock()
            .await
            .recv()
            .await
            .map(|l| l.trim_end_matches(['\r', '\n']).to_string())
    }
}

/// Tools contributed by external MCP extensions. They can do anything, so Safe mode always
/// asks before running them. Filled in as background MCP connections complete.
fn external_tools() -> &'static std::sync::RwLock<HashSet<String>> {
    static EXTERNAL: std::sync::OnceLock<std::sync::RwLock<HashSet<String>>> = std::sync::OnceLock::new();
    EXTERNAL.get_or_init(Default::default)
}

pub fn mark_external_tools(names: &[String]) {
    let mut guard = external_tools().write().unwrap_or_else(|p| p.into_inner());
    guard.extend(names.iter().cloned());
}

fn is_external_tool(tool_name: &str) -> bool {
    external_tools()
        .read()
        .map(|g| g.contains(tool_name))
        .unwrap_or(true)
}

/// Tools that change files, run commands, control the desktop, or write media to disk.
pub fn is_sensitive_tool(tool_name: &str) -> bool {
    is_external_tool(tool_name) || is_builtin_sensitive_tool(tool_name)
}

fn is_builtin_sensitive_tool(tool_name: &str) -> bool {
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
            | "image_generate"
            | "image_upscale"
            | "image_process"
            | "video_generate"
            | "video_extract_frames"
            | "video_edit"
            | "mesh_generate_3d"
            | "blender_convert_mesh"
            | "blender_render_preview"
            | "unity_export_asset"
            | "audio_generate_music"
            | "audio_generate_sfx"
            | "audio_record"
            | "spawn_subagent"
    )
}

/// Enforces Autopilot / Safe / Plan. In Safe mode it asks inline; in Plan mode (or when nobody
/// can answer) it *denies* the call and tells the model why, instead of aborting the run.
pub struct CliSecurityHook {
    pub mode: Arc<RwLock<ExecutionMode>>,
    pub input: Option<Arc<Input>>,
    pub always_allowed: Arc<Mutex<HashSet<String>>>,
    pub style: Style,
}

#[async_trait]
impl Hook for CliSecurityHook {
    async fn on_event(&self, event: &AgentEvent) -> Result<HookAction, CoreError> {
        let AgentEvent::PreToolUse { tool_name, .. } = event else {
            return Ok(HookAction::Continue);
        };
        if !is_sensitive_tool(tool_name) {
            return Ok(HookAction::Continue);
        }
        let mode = *self.mode.read().await;
        match mode {
            ExecutionMode::Autopilot => Ok(HookAction::Continue),
            ExecutionMode::Plan => Ok(HookAction::Deny(format!(
                "'{tool_name}' is blocked in Plan mode (read-only). Describe the change you would make instead."
            ))),
            ExecutionMode::Safe => {
                if self.always_allowed.lock().await.contains(tool_name) {
                    return Ok(HookAction::Continue);
                }
                let Some(input) = self.input.as_ref().filter(|i| i.interactive) else {
                    // Nobody can approve: stop cleanly instead of letting the model improvise.
                    return Ok(HookAction::Halt(format!(
                        "Stopped: '{tool_name}' needs approval but no interactive terminal is attached. Re-run with `--mode autopilot` (or run `dex` interactively)."
                    )));
                };
                print!(
                    "  {} {} {} ",
                    self.style.yellow("?"),
                    format_args!("Allow {}?", self.style.bold(tool_name)),
                    self.style.dim("[y]es / [n]o / [a]lways this session ›")
                );
                let _ = std::io::stdout().flush();
                let answer = input.next_line().await.unwrap_or_default();
                match answer.trim().to_lowercase().as_str() {
                    "y" | "yes" | "s" | "si" | "sí" => Ok(HookAction::Continue),
                    "a" | "always" | "siempre" => {
                        self.always_allowed.lock().await.insert(tool_name.clone());
                        Ok(HookAction::Continue)
                    }
                    _ => Ok(HookAction::Halt(format!(
                        "The user declined '{tool_name}'. Stop and wait for new instructions."
                    ))),
                }
            }
        }
    }
}

/// Everything needed to (re)build the agent, e.g. after `/model` switches endpoints.
pub struct SessionParts {
    pub endpoint: Endpoint,
    pub system_prompt: String,
    pub event_bus: EventBus,
    pub mode: Arc<RwLock<ExecutionMode>>,
    pub input: Arc<Input>,
    pub stream: bool,
    pub verbose: bool,
    pub max_steps: usize,
    pub style: Style,
    pub always_allowed: Arc<Mutex<HashSet<String>>>,
    pub printer: StreamPrinter,
    /// Builds the tool registry for a provider (also kicks off background MCP connections).
    pub make_tools: Arc<dyn Fn(Arc<dyn LlmProvider>) -> ToolRegistry + Send + Sync>,
}

impl SessionParts {
    pub fn build_agent(&self) -> ReActAgent {
        let provider = self.endpoint.build_provider();
        let tools = (self.make_tools)(provider.clone());

        let mut hooks = HookRegistry::new();
        hooks.register(Arc::new(ToolActivityHook {
            printer: self.printer.clone(),
            style: self.style,
            verbose: self.verbose,
        }));
        hooks.register(Arc::new(CliSecurityHook {
            mode: self.mode.clone(),
            input: Some(self.input.clone()),
            always_allowed: self.always_allowed.clone(),
            style: self.style,
        }));

        let mut agent = ReActAgent::new(provider, tools)
            .with_system_prompt(self.system_prompt.clone())
            .with_hooks(hooks)
            .with_max_steps(self.max_steps)
            .with_event_bus(self.event_bus.clone());
        if self.stream {
            let printer = self.printer.clone();
            agent = agent.with_token_callback(Arc::new(move |t: &str| printer.push(t)));
        }
        agent
    }
}

/// Runs one goal, rendering the answer. Returns `false` if interrupted with Ctrl-C.
pub async fn run_turn(agent: &mut ReActAgent, parts: &SessionParts, goal: &str) -> bool {
    parts.printer.begin_turn();
    let started = Instant::now();
    let outcome = tokio::select! {
        r = agent.run(goal) => Some(r),
        _ = tokio::signal::ctrl_c() => None,
    };
    parts.printer.finish();
    let style = parts.style;
    match outcome {
        Some(Ok(answer)) => {
            if !parts.stream || !parts.printer.printed_any() {
                let clean = strip_think(&answer);
                if !clean.is_empty() {
                    println!("{} {clean}", style.accent("⏺"));
                }
            }
            if parts.verbose {
                println!("{}", style.dim(&format!("  ({:.1}s)", started.elapsed().as_secs_f32())));
            }
            true
        }
        Some(Err(CoreError::AgentHalted { reason })) => {
            println!("  {} {}", style.yellow("⎿"), style.dim(&reason));
            true
        }
        Some(Err(CoreError::MaxIterationsReached { max_steps })) => {
            println!(
                "  {} {}",
                style.yellow("⎿"),
                style.dim(&format!(
                    "Stopped after {max_steps} steps. Say \"continue\" to keep going (or raise --max-steps)."
                ))
            );
            true
        }
        Some(Err(e)) => {
            println!("  {} {}", style.red("✗"), e);
            true
        }
        None => {
            agent.repair_history();
            println!("\n  {} {}", style.yellow("⎿"), style.dim("Interrupted. What should Dex do instead?"));
            false
        }
    }
}

fn print_help(style: Style) {
    let rows = [
        ("/model [id]", "list chat models on the current server, or switch (saved)"),
        ("/mode [safe|plan|autopilot]", "show or change the permission mode (saved)"),
        ("/clear", "start a fresh conversation"),
        ("/tools", "list the tools the agent can call right now"),
        ("/doctor", "diagnose servers, models and runtimes"),
        ("/exit", "quit (or Ctrl-D, or Ctrl-C twice)"),
    ];
    for (cmd, desc) in rows {
        println!("  {}  {}", style.bold(&format!("{cmd:<28}")), style.dim(desc));
    }
    println!(
        "  {}",
        style.dim("Ctrl-C interrupts the current task. Modes: safe asks before edits/shell, plan is read-only.")
    );
}

pub fn mode_label(mode: ExecutionMode) -> &'static str {
    match mode {
        ExecutionMode::Autopilot => "autopilot",
        ExecutionMode::Safe => "safe",
        ExecutionMode::Plan => "plan",
    }
}

pub fn parse_mode(s: &str) -> Option<ExecutionMode> {
    match s.trim().to_lowercase().as_str() {
        "autopilot" | "auto" => Some(ExecutionMode::Autopilot),
        "safe" => Some(ExecutionMode::Safe),
        "plan" | "read-only" | "readonly" => Some(ExecutionMode::Plan),
        _ => None,
    }
}

pub fn print_banner(parts: &SessionParts, mode: ExecutionMode) {
    let style = parts.style;
    let cwd = std::env::current_dir()
        .map(|p| {
            let s = p.display().to_string();
            match std::env::var("HOME") {
                Ok(home) if s.starts_with(&home) => format!("~{}", &s[home.len()..]),
                _ => s,
            }
        })
        .unwrap_or_default();
    println!(
        "{} {}  {}",
        style.accent("✻"),
        style.bold("Dex"),
        style.dim(&format!("v{}", env!("CARGO_PKG_VERSION")))
    );
    println!(
        "  {} {}  {}",
        style.dim("model"),
        parts.endpoint.model,
        style.dim(&format!("({})", parts.endpoint.label))
    );
    println!("  {} {}  {} {}", style.dim("mode "), mode_label(mode), style.dim("cwd"), cwd);
    println!("  {}\n", style.dim("/help for commands · Ctrl-C to interrupt"));
}

/// The interactive REPL.
pub async fn repl(mut parts: SessionParts, initial_goal: Option<String>) -> Result<()> {
    let style = parts.style;
    print_banner(&parts, *parts.mode.read().await);
    let mut agent = parts.build_agent();

    if let Some(goal) = initial_goal {
        println!("{} {}", style.dim("›"), goal);
        run_turn(&mut agent, &parts, &goal).await;
    }

    let mut last_ctrl_c: Option<Instant> = None;
    loop {
        print!("\n{} ", style.accent("›"));
        let _ = std::io::stdout().flush();

        let line = tokio::select! {
            l = parts.input.next_line() => l,
            _ = tokio::signal::ctrl_c() => {
                if last_ctrl_c.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
                    println!();
                    break;
                }
                last_ctrl_c = Some(Instant::now());
                println!("\n  {}", style.dim("Press Ctrl-C again to exit."));
                continue;
            }
        };
        let Some(line) = line else {
            println!();
            break;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(cmd) = trimmed.strip_prefix('/') {
            let mut it = cmd.splitn(2, char::is_whitespace);
            let name = it.next().unwrap_or_default();
            let arg = it.next().map(str::trim).filter(|a| !a.is_empty());
            match name {
                "exit" | "quit" | "q" => break,
                "help" | "h" | "?" => print_help(style),
                "clear" | "new" | "reset" => {
                    agent.reset_conversation();
                    println!("  {}", style.dim("Started a fresh conversation."));
                }
                "mode" => match arg {
                    None => println!("  mode: {}", mode_label(*parts.mode.read().await)),
                    Some(m) => match parse_mode(m) {
                        Some(new_mode) => {
                            *parts.mode.write().await = new_mode;
                            let mut cfg = CliConfig::load().await;
                            cfg.mode = Some(mode_label(new_mode).to_string());
                            let _ = cfg.save().await;
                            println!("  {} mode → {}", style.green("✓"), mode_label(new_mode));
                        }
                        None => println!("  {}", style.red("Unknown mode. Use safe, plan or autopilot.")),
                    },
                },
                "model" | "models" => match arg {
                    None => {
                        let models = list_chat_models(&parts.endpoint).await;
                        if models.is_empty() {
                            println!("  {}", style.dim("No model list available from this server."));
                        }
                        for m in models {
                            let mark = if m == parts.endpoint.model { style.green("●") } else { " ".to_string() };
                            println!("  {mark} {m}");
                        }
                        println!("  {}", style.dim("Switch with /model <id>"));
                    }
                    Some(id) => {
                        let available = list_chat_models(&parts.endpoint).await;
                        if !available.is_empty() && !available.iter().any(|m| m == id) {
                            println!("  {} '{id}' is not served by {}.", style.red("✗"), parts.endpoint.label);
                            continue;
                        }
                        let history: Vec<_> = agent.context().messages().to_vec();
                        parts.endpoint.model = id.to_string();
                        agent = parts.build_agent();
                        for m in history {
                            agent.context_mut().push(m);
                        }
                        let mut cfg = CliConfig::load().await;
                        cfg.model = Some(id.to_string());
                        let _ = cfg.save().await;
                        println!("  {} model → {id}", style.green("✓"));
                    }
                },
                "tools" => {
                    let mut names: Vec<String> = agent_tool_names(&agent);
                    names.sort();
                    println!("  {}", style.dim(&format!("{} tools: {}", names.len(), names.join(", "))));
                }
                "doctor" => {
                    if let Err(e) = crate::doctor::run(style).await {
                        println!("  {} {e}", style.red("✗"));
                    }
                }
                other => println!("  {} Unknown command /{other}. Try /help.", style.red("✗")),
            }
            continue;
        }

        last_ctrl_c = None;
        run_turn(&mut agent, &parts, trimmed).await;
    }
    Ok(())
}

fn agent_tool_names(agent: &ReActAgent) -> Vec<String> {
    agent.tool_definitions().into_iter().map(|d| d.name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parsing_roundtrip() {
        for m in [ExecutionMode::Autopilot, ExecutionMode::Safe, ExecutionMode::Plan] {
            assert_eq!(parse_mode(mode_label(m)), Some(m));
        }
        assert_eq!(parse_mode("nope"), None);
    }

    #[test]
    fn sensitive_tools_include_mcp_and_media() {
        assert!(is_sensitive_tool("execute_shell"));
        assert!(is_sensitive_tool("image_generate"));
        assert!(!is_sensitive_tool("execute_blender_code"));
        mark_external_tools(&["execute_blender_code".to_string()]);
        assert!(is_sensitive_tool("execute_blender_code"));
        assert!(!is_sensitive_tool("read_file"));
        assert!(!is_sensitive_tool("mesh_inspect"));
    }
}
