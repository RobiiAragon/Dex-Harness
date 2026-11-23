use tracing::debug;
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
}
