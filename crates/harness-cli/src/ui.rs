//! Terminal rendering for the interactive `dex` CLI: one clean streamed answer, collapsed
//! `<think>` reasoning, and compact `⏺ tool` / `⎿ result` lines in the style of Claude Code.

use std::io::{IsTerminal, Write};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_core::{AgentEvent, CoreError, Hook, HookAction};

/// ANSI styling that degrades to plain text when stdout is not a TTY or `NO_COLOR` is set.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    color: bool,
}

impl Style {
    pub fn detect() -> Self {
        Self {
            color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    fn wrap(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn dim(&self, s: &str) -> String {
        self.wrap("2", s)
    }
    pub fn bold(&self, s: &str) -> String {
        self.wrap("1", s)
    }
    pub fn accent(&self, s: &str) -> String {
        self.wrap("38;5;208", s)
    }
    pub fn green(&self, s: &str) -> String {
        self.wrap("32", s)
    }
    pub fn red(&self, s: &str) -> String {
        self.wrap("31", s)
    }
    pub fn yellow(&self, s: &str) -> String {
        self.wrap("33", s)
    }
}

/// Truncates to at most `max` characters on a char boundary (never panics on UTF-8).
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Removes `<think>…</think>` blocks (and a dangling unterminated one) from a final answer.
pub fn strip_think(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        match rest.find("<think>") {
            Some(start) => {
                out.push_str(&rest[..start]);
                match rest[start..].find("</think>") {
                    Some(end) => rest = &rest[start + end + "</think>".len()..],
                    None => break,
                }
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out.trim().to_string()
}

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

#[derive(Default)]
struct StreamState {
    pending: String,
    in_think: bool,
    /// Nothing visible printed yet in this turn (used to trim leading blank lines).
    at_start: bool,
    printed_any: bool,
    think_label_shown: bool,
    /// Trailing whitespace held back until more text arrives (avoids blank-line runs
    /// before tool calls and at the end of answers).
    held_ws: String,
}

/// Streams model tokens to stdout, hiding reasoning behind a single "✻ Thinking…" line.
#[derive(Clone)]
pub struct StreamPrinter {
    state: Arc<Mutex<StreamState>>,
    style: Style,
}

impl StreamPrinter {
    pub fn new(style: Style) -> Self {
        Self {
            state: Arc::new(Mutex::new(StreamState {
                at_start: true,
                ..Default::default()
            })),
            style,
        }
    }

    /// Resets per-turn state before a new model call.
    pub fn begin_turn(&self) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        *st = StreamState {
            at_start: true,
            ..Default::default()
        };
    }

    /// Whether any visible answer text was printed since `begin_turn`.
    pub fn printed_any(&self) -> bool {
        self.state.lock().map(|s| s.printed_any).unwrap_or(false)
    }

    fn emit(&self, st: &mut StreamState, text: &str) {
        let text = if st.at_start { text.trim_start() } else { text };
        if text.is_empty() {
            return;
        }
        let body = text.trim_end();
        let trailing = &text[body.len()..];
        if body.is_empty() {
            st.held_ws.push_str(trailing);
            return;
        }
        if !st.printed_any {
            print!("{} ", self.style.accent("⏺"));
        }
        let held = std::mem::take(&mut st.held_ws);
        // Collapse runs of blank lines to a single paragraph break.
        let newlines = held.matches('\n').count();
        let held = if newlines > 2 { "\n\n".to_string() } else { held };
        st.at_start = false;
        st.printed_any = true;
        print!("{held}{body}");
        st.held_ws.push_str(trailing);
        let _ = std::io::stdout().flush();
    }

    /// Longest suffix of `s` that is a proper prefix of `tag` (a tag possibly split across tokens).
    fn partial_tag_suffix(s: &str, tag: &str) -> usize {
        (1..tag.len())
            .rev()
            .find(|&n| s.len() >= n && s.is_char_boundary(s.len() - n) && tag.starts_with(&s[s.len() - n..]))
            .unwrap_or(0)
    }

    pub fn push(&self, token: &str) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.pending.push_str(token);
        loop {
            if st.in_think {
                if let Some(end) = st.pending.find(CLOSE) {
                    st.pending.drain(..end + CLOSE.len());
                    st.in_think = false;
                    st.at_start = true;
                    if st.think_label_shown {
                        println!();
                    }
                    continue;
                }
                // Discard hidden reasoning, but keep a possible split closing tag.
                let keep = Self::partial_tag_suffix(&st.pending, CLOSE);
                let cut = st.pending.len() - keep;
                st.pending.drain(..cut);
                break;
            }
            if let Some(start) = st.pending.find(OPEN) {
                let before: String = st.pending[..start].to_string();
                self.emit(&mut st, &before);
                st.pending.drain(..start + OPEN.len());
                st.in_think = true;
                if !st.think_label_shown {
                    if st.printed_any {
                        println!();
                    }
                    print!("{}", self.style.dim("✻ Thinking…"));
                    let _ = std::io::stdout().flush();
                    st.think_label_shown = true;
                }
                continue;
            }
            let keep = Self::partial_tag_suffix(&st.pending, OPEN);
            let cut = st.pending.len() - keep;
            let out: String = st.pending[..cut].to_string();
            st.pending.drain(..cut);
            self.emit(&mut st, &out);
            break;
        }
    }

    /// Flushes buffered text and ends the line if anything was printed.
    pub fn finish(&self) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if !st.in_think && !st.pending.is_empty() {
            let rest = std::mem::take(&mut st.pending);
            self.emit(&mut st, &rest);
        }
        st.pending.clear();
        st.held_ws.clear();
        if st.printed_any || (st.think_label_shown && st.in_think) {
            println!();
        }
        st.in_think = false;
    }
}

/// One-line summary of tool arguments (the most meaningful field, else compact JSON).
pub fn summarize_args(args: &serde_json::Value) -> String {
    const KEYS: [&str; 12] = [
        "command", "path", "file_path", "pattern", "query", "url", "goal", "prompt", "text",
        "action", "name", "input",
    ];
    if let Some(obj) = args.as_object() {
        for key in KEYS {
            if let Some(v) = obj.get(key) {
                let s = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                return truncate_chars(&s.replace('\n', " ⏎ "), 90);
            }
        }
        if obj.is_empty() {
            return String::new();
        }
    }
    truncate_chars(&args.to_string(), 90)
}

/// Compact preview of a tool result: first few lines, then "… +N lines".
pub fn summarize_output(output: &str, max_lines: usize) -> Vec<String> {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut out: Vec<String> = lines
        .iter()
        .take(max_lines)
        .map(|l| truncate_chars(l.trim_end(), 140))
        .collect();
    if lines.len() > max_lines {
        out.push(format!("… +{} lines", lines.len() - max_lines));
    }
    if out.is_empty() {
        out.push("(no output)".to_string());
    }
    out
}

/// Prints tool activity synchronously from the hook pipeline, so lines appear in order with
/// streamed text and approval prompts. Register it **before** the security hook.
pub struct ToolActivityHook {
    pub printer: StreamPrinter,
    pub style: Style,
    pub verbose: bool,
    /// Collects `[dex-install:<id>]` markers from failed tools so the CLI can offer an install.
    pub missing_runtimes: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Hook for ToolActivityHook {
    async fn on_event(&self, event: &AgentEvent) -> Result<HookAction, CoreError> {
        match event {
            AgentEvent::PreToolUse { tool_name, arguments } => {
                self.printer.finish();
                self.printer.begin_turn();
                let args = summarize_args(arguments);
                println!(
                    "{} {} {}",
                    self.style.accent("⏺"),
                    self.style.bold(tool_name),
                    self.style.dim(&args)
                );
            }
            AgentEvent::PostToolUse { result, .. } => {
                if result.is_error {
                    let ids = harness_core::parse_install_markers(&result.output);
                    if !ids.is_empty() {
                        let mut missing = self.missing_runtimes.lock().unwrap_or_else(|p| p.into_inner());
                        for id in ids {
                            if !missing.contains(&id) {
                                missing.push(id);
                            }
                        }
                    }
                }
                let max = if self.verbose { 40 } else { 3 };
                let output = harness_core::strip_install_markers(&result.output);
                let lines = summarize_output(&output, max);
                for (i, line) in lines.iter().enumerate() {
                    let gutter = if i == 0 { "  ⎿ " } else { "    " };
                    let body = if result.is_error {
                        self.style.red(line)
                    } else {
                        self.style.dim(line)
                    };
                    println!("{}{}", self.style.dim(gutter), body);
                }
            }
            AgentEvent::OnError { .. } => {}
        }
        Ok(HookAction::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_is_utf8_safe() {
        let s = "ñandú ".repeat(400);
        let t = truncate_chars(&s, 10);
        assert_eq!(t.chars().count(), 10);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn strip_think_removes_reasoning() {
        assert_eq!(strip_think("<think>hmm</think>\n\nHola"), "Hola");
        assert_eq!(strip_think("A<think>x</think>B"), "AB");
        assert_eq!(strip_think("<think>never closed"), "");
        assert_eq!(strip_think("plain"), "plain");
    }

    #[test]
    fn partial_tag_detection() {
        assert_eq!(StreamPrinter::partial_tag_suffix("hello <thi", OPEN), 4);
        assert_eq!(StreamPrinter::partial_tag_suffix("hello", OPEN), 0);
        assert_eq!(StreamPrinter::partial_tag_suffix("abc</", CLOSE), 2);
        assert_eq!(StreamPrinter::partial_tag_suffix("ñ", OPEN), 0);
    }

    #[test]
    fn summaries() {
        let a = serde_json::json!({"command": "ls -la", "timeout": 3});
        assert_eq!(summarize_args(&a), "ls -la");
        let out = summarize_output("a\nb\nc\nd\ne", 3);
        assert_eq!(out, vec!["a", "b", "c", "… +2 lines"]);
    }
}
