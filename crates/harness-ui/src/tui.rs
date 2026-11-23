use std::io;
use std::time::Duration;
use crossterm::{
    event::{self, Event as CEvent, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, Paragraph},
    Terminal,
};

use harness_core::{DexEvent, EventBus, TaskBoard};

pub struct TuiState {
    pub mode: String,
    pub subagents: Vec<String>,
    pub logs: Vec<String>,
    pub taskboard_items: Vec<String>,
    pub estimated_tokens: usize,
}

impl Default for TuiState {
    fn default() -> Self {
        Self {
            mode: "Autopilot".to_string(),
            subagents: vec!["Orchestrator [Active]".to_string()],
            logs: vec!["System initialized. Listening for Dex-Harness events...".to_string()],
            taskboard_items: vec!["No tasks loaded yet.".to_string()],
            estimated_tokens: 0,
        }
    }
}

pub async fn run_tui(event_bus: EventBus, mode: &str) -> Result<(), io::Error> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = TuiState {
        mode: mode.to_string(),
        ..Default::default()
    };

    // Load initial tasks from .dex/tasks.json if present
    let board = TaskBoard::load_or_default(".dex/tasks.json").await;
    if !board.tasks.is_empty() {
        state.taskboard_items = board
            .tasks
            .into_iter()
            .map(|t| format!("{}: {:?} - {}", t.id, t.status, t.title))
            .collect();
    }

    let mut bus_rx = event_bus.subscribe();

    loop {
        // Poll event bus without blocking
        while let Ok(event) = bus_rx.try_recv() {
            match event {
                DexEvent::ThoughtToken { token } => {
                    if let Some(last) = state.logs.last_mut() {
                        if !last.ends_with('\n') {
                            last.push_str(&token);
                        } else {
                            state.logs.push(token);
                        }
                    } else {
                        state.logs.push(token);
                    }
                }
                DexEvent::ToolStarted { tool_name, arguments } => {
                    state.logs.push(format!("\n🛠️  [TOOL START] {tool_name}"));
                    state.logs.push(format!("   Args: {arguments}"));
                }
                DexEvent::ToolFinished { tool_name, output, is_error } => {
                    let status = if is_error { "❌ FAILED" } else { "✅ SUCCESS" };
                    state.logs.push(format!("   Result [{tool_name}]: {status}"));
                    if output.len() > 200 {
                        state.logs.push(format!("   Output: {}...", &output[..200]));
                    } else {
                        state.logs.push(format!("   Output: {output}"));
                    }
                }
                DexEvent::SubagentSpawned { role, task } => {
                    state.subagents.push(format!("Subagent [{role}]: {task}"));
                    state.logs.push(format!("\n🤖 [SUBAGENT SPAWNED] {role}"));
                }
                DexEvent::SubagentCompleted { role, .. } => {
                    state.logs.push(format!("🏁 [SUBAGENT FINISHED] {role}"));
                }
                DexEvent::TaskUpdated { task_id, status } => {
                    state.taskboard_items.push(format!("{task_id} -> {status}"));
                }
                DexEvent::ContextCompacted { estimated_tokens } => {
                    state.estimated_tokens = estimated_tokens;
                    state.logs.push(format!("🧹 [COMPACTION] Retained ~{estimated_tokens} tokens"));
                }
                DexEvent::SecurityBlocked { tool_name, reason } => {
                    state.logs.push(format!("⛔ [SECURITY BLOCKED] {tool_name}: {reason}"));
                }
                DexEvent::ModelDownloadProgress { file_name, downloaded_bytes, total_bytes, speed_mbps, .. } => {
                    let pct = if total_bytes > 0 { (downloaded_bytes as f64 / total_bytes as f64) * 100.0 } else { 0.0 };
                    state.logs.push(format!("📥 [DOWNLOAD] {file_name}: {pct:.1}% ({speed_mbps:.1} MB/s)"));
                }
                DexEvent::ModelDownloadCompleted { file_name, .. } => {
                    state.logs.push(format!("✅ [DOWNLOAD COMPLETE] {file_name}"));
                }
                DexEvent::ModelDownloadFailed { file_name, error, .. } => {
                    state.logs.push(format!("❌ [DOWNLOAD FAILED] {file_name}: {error}"));
                }
                DexEvent::ExtensionStatusChanged { extension_id, status, pid, estimated_ram_mb } => {
                    let pid_str = pid.map(|p| format!(" pid={p}")).unwrap_or_default();
                    state.logs.push(format!(
                        "🧩 [EXTENSION] {extension_id} -> {status}{pid_str} (~{estimated_ram_mb} MB)"
                    ));
                }
                DexEvent::ToolsActivated { source, tool_names } => {
                    state.logs.push(format!(
                        "🔎 [TOOLS ACTIVATED] ({source}): {}",
                        tool_names.join(", ")
                    ));
                }
                DexEvent::MediaGenerated { kind, path, prompt, duration_secs } => {
                    let dur = duration_secs.map(|d| format!(" ({d:.1}s)")).unwrap_or_default();
                    state.logs.push(format!(
                        "🎵 [MEDIA:{kind}]{dur} {path} — {prompt}"
                    ));
                }
                DexEvent::VoiceCaptureState { recording, level_db } => {
                    if recording {
                        state.logs.push(format!("🎙️ [MIC] Recording ({level_db:.1} dB)"));
                    }
                }
                DexEvent::MediaGenerationProgress { kind, step, total_steps, message, .. } => {
                    state.logs.push(format!(
                        "🎨 [MEDIA:{kind}] step {step}/{total_steps}: {message}"
                    ));
                }
            }
        }

        // Render dashboard
        terminal.draw(|f| {
            let size = f.area();

            // Main vertical split: Header/Panels (top) and Bottom Status
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(10), Constraint::Length(3)])
                .split(size);

            // Horizontal split for top: Left (Subagents/Tasks), Right (Log stream)
            let top_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
                .split(chunks[0]);

            // Left vertical split: Subagents (top-left) and TaskBoard (bottom-left)
            let left_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
                .split(top_chunks[0]);

            // 1. Subagents Panel
            let subagent_items: Vec<ListItem> = state
                .subagents
                .iter()
                .map(|s| ListItem::new(Line::from(vec![Span::raw(s)])))
                .collect();
            let subagents_widget = List::new(subagent_items).block(
                Block::default()
                    .title(format!(" 🤖 Subagents & Mode [{}] ", state.mode))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            );
            f.render_widget(subagents_widget, left_chunks[0]);

            // 2. TaskBoard Panel
            let task_items: Vec<ListItem> = state
                .taskboard_items
                .iter()
                .map(|t| ListItem::new(Line::from(vec![Span::raw(t)])))
                .collect();
            let tasks_widget = List::new(task_items).block(
                Block::default()
                    .title(" 📋 TaskBoard (.dex/tasks.json) ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Green)),
            );
            f.render_widget(tasks_widget, left_chunks[1]);

            // 3. Real-time Log & Thought Stream (Right)
            let log_lines: Vec<Line> = state
                .logs
                .iter()
                .rev()
                .take(30)
                .rev()
                .map(|l| Line::from(vec![Span::raw(l)]))
                .collect();
            let logs_widget = Paragraph::new(log_lines).block(
                Block::default()
                    .title(" ⚡ Live Agent Stream & Telemetry ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            );
            f.render_widget(logs_widget, top_chunks[1]);

            // 4. Token & Compaction Gauge (Bottom)
            let token_pct = ((state.estimated_tokens as f64 / 8000.0) * 100.0).min(100.0) as u16;
            let gauge = Gauge::default()
                .block(
                    Block::default()
                        .title(" 🧮 Context Window Usage (~8k target) ")
                        .borders(Borders::ALL),
                )
                .gauge_style(
                    Style::default()
                        .fg(if token_pct > 75 { Color::Red } else { Color::Green })
                        .add_modifier(Modifier::BOLD),
                )
                .percent(token_pct)
                .label(format!("{} / 8000 Tokens (Press 'q' or Esc to exit)", state.estimated_tokens));
            f.render_widget(gauge, chunks[1]);
        })?;

        // Check user input
        if event::poll(Duration::from_millis(100))? {
            if let CEvent::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') || key.code == KeyCode::Esc {
                    break;
                }
            }
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
