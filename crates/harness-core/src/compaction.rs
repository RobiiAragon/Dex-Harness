use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};
use crate::memory::ConversationContext;
use crate::provider::{Message, Role};

/// Compacts a conversation context if its estimated token footprint exceeds `max_tokens`.
pub struct ContextCompactor {
    pub max_tokens: usize,
    pub keep_last_messages: usize,
}

impl Default for ContextCompactor {
    fn default() -> Self {
        Self {
            max_tokens: 6000,
            keep_last_messages: 6,
        }
    }
}

impl ContextCompactor {
    pub fn new(max_tokens: usize, keep_last_messages: usize) -> Self {
        Self {
            max_tokens,
            keep_last_messages,
        }
    }

    /// Estimates total token count using the standard ~4 characters per token heuristic.
    pub fn estimate_tokens(messages: &[Message]) -> usize {
        let total_chars: usize = messages
            .iter()
            .map(|m| {
                m.content.as_deref().unwrap_or("").len()
                    + m.tool_calls
                        .as_ref()
                        .map(|calls| {
                            calls
                                .iter()
                                .map(|c| c.name.len() + c.arguments.to_string().len())
                                .sum::<usize>()
                        })
                        .unwrap_or(0)
            })
            .sum();

        total_chars / 4
    }

    /// Compresses historical tool outputs in place if tokens exceed `max_tokens`.
    pub fn compact(&self, context: &mut ConversationContext) -> bool {
        let messages = context.messages();
        let estimated = Self::estimate_tokens(messages);

        if estimated <= self.max_tokens || messages.len() <= self.keep_last_messages {
            return false;
        }

        debug!(
            estimated_tokens = estimated,
            threshold = self.max_tokens,
            "Compacting conversation history"
        );

        let total = messages.len();
        let cutoff = total.saturating_sub(self.keep_last_messages);

        // Build compacted messages
        let mut compacted = Vec::with_capacity(total);
        for (idx, msg) in messages.iter().enumerate() {
            if idx < cutoff && msg.role == Role::Tool {
                // Compress older tool outputs
                let original_len = msg.content.as_deref().unwrap_or("").len();
                if original_len > 300 {
                    let preview = msg
                        .content
                        .as_deref()
                        .unwrap_or("")
                        .chars()
                        .take(150)
                        .collect::<String>();
                    let compressed = format!(
                        "{preview}...\n[Output truncated from {original_len} chars to preserve context]"
                    );
                    let mut new_msg = msg.clone();
                    new_msg.content = Some(compressed);
                    compacted.push(new_msg);
                    continue;
                }
            }
            compacted.push(msg.clone());
        }

        context.clear_history();
        for msg in compacted {
            context.push(msg);
        }

        true
    }
}

/// Guardrail that detects excessively large tool outputs (e.g. from MCP servers),
/// persists the complete output under `.dex/tmp/tool_outputs/<id>.txt`, and returns
/// a clean truncated preview containing head, tail, and file path for `read_file`.
#[derive(Debug, Clone)]
pub struct ToolOutputCompactor {
    pub max_chars: usize,
    pub head_chars: usize,
    pub tail_chars: usize,
    pub output_dir: PathBuf,
}

impl Default for ToolOutputCompactor {
    fn default() -> Self {
        Self {
            max_chars: 8_000,
            head_chars: 2_500,
            tail_chars: 1_500,
            output_dir: PathBuf::from(".dex/tmp/tool_outputs"),
        }
    }
}

impl ToolOutputCompactor {
    pub fn new(max_chars: usize, head_chars: usize, tail_chars: usize) -> Self {
        Self {
            max_chars,
            head_chars,
            tail_chars,
            output_dir: PathBuf::from(".dex/tmp/tool_outputs"),
        }
    }

    pub fn with_output_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.output_dir = dir.into();
        self
    }

    /// Compacts `output` if it exceeds `max_chars`.
    /// Returns `(effective_output, Option<saved_file_path>)`.
    pub async fn compact_if_needed(
        &self,
        tool_name: &str,
        call_id: &str,
        output: &str,
    ) -> (String, Option<PathBuf>) {
        let total_chars = output.chars().count();
        if total_chars <= self.max_chars {
            return (output.to_string(), None);
        }

        let safe_tool = sanitize_filename_component(tool_name);
        let safe_id = sanitize_filename_component(call_id);
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let file_name = format!("{safe_tool}_{safe_id}_{ts_ms}.txt");
        let file_path = self.output_dir.join(file_name);

        let saved_path = match tokio::fs::create_dir_all(&self.output_dir).await {
            Ok(()) => match tokio::fs::write(&file_path, output).await {
                Ok(()) => {
                    debug!(
                        tool = %tool_name,
                        total_chars = total_chars,
                        path = ?file_path,
                        "Persisted oversized tool output to disk"
                    );
                    Some(file_path)
                }
                Err(e) => {
                    warn!(path = ?file_path, error = %e, "Failed to save full tool output");
                    None
                }
            },
            Err(e) => {
                warn!(dir = ?self.output_dir, error = %e, "Failed to create tool outputs directory");
                None
            }
        };

        let head: String = output.chars().take(self.head_chars).collect();
        let skip_for_tail = total_chars.saturating_sub(self.tail_chars);
        let tail: String = output.chars().skip(skip_for_tail).collect();

        let location_note = match &saved_path {
            Some(p) => format!(
                "Full output saved to '{}'. Use `read_file` with start_line/end_line to inspect specific sections.",
                p.display()
            ),
            None => "Full output could not be saved to disk.".to_string(),
        };

        let compacted = format!(
            "{head}\n\n... [TOOL OUTPUT TRUNCATED: {total_chars} chars from '{tool_name}' exceeded guardrail ({} chars). {location_note}] ...\n\n{tail}",
            self.max_chars
        );

        (compacted, saved_path)
    }
}

fn sanitize_filename_component(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    if cleaned.is_empty() {
        "out".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_compactor() {
        let mut ctx = ConversationContext::new();
        ctx.push(Message::user("Please analyze this codebase"));

        // Add 10 bulky tool messages
        for i in 0..10 {
            let large_output = "a".repeat(2000);
            ctx.push(Message::tool_result(format!("call_{i}"), "read_file", large_output));
        }

        let compactor = ContextCompactor::new(500, 4);
        let compacted = compactor.compact(&mut ctx);
        assert!(compacted);

        // Check that older tool messages were truncated
        let first_tool = &ctx.messages()[1];
        assert!(first_tool.content.as_ref().unwrap().contains("[Output truncated"));

        // Check that the last messages remained intact
        let last_tool = &ctx.messages()[ctx.messages().len() - 1];
        assert_eq!(last_tool.content.as_ref().unwrap().len(), 2000);
    }

    #[tokio::test]
    async fn test_tool_output_compactor_truncates_and_saves_to_disk() {
        let temp_dir = std::env::temp_dir().join(format!("dex_tool_out_test_{}", std::process::id()));
        let compactor = ToolOutputCompactor::new(500, 120, 80).with_output_dir(&temp_dir);

        // Short output stays untouched
        let (short_out, short_path) = compactor
            .compact_if_needed("docling_extract", "call_short", "Short text")
            .await;
        assert_eq!(short_out, "Short text");
        assert!(short_path.is_none());

        // Long output is spilled to disk and truncated with head + tail + path
        let head_marker = "START_MARKER_";
        let tail_marker = "_END_MARKER";
        let huge = format!("{}{}{}", head_marker, "X".repeat(1200), tail_marker);
        let (compacted, saved_path) = compactor
            .compact_if_needed("docling_extract", "call_huge", &huge)
            .await;

        let saved = saved_path.expect("huge output must be saved to disk");
        assert!(compacted.starts_with(head_marker));
        assert!(compacted.ends_with(tail_marker));
        assert!(compacted.contains("TOOL OUTPUT TRUNCATED"));
        assert!(compacted.contains(&saved.display().to_string()));
        assert!(compacted.len() < huge.len());

        let persisted = tokio::fs::read_to_string(&saved)
            .await
            .expect("should read persisted full output");
        assert_eq!(persisted, huge);

        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }
}
