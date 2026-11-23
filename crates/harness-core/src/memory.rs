use serde::{Deserialize, Serialize};
use crate::provider::Message;

/// Manages short-term conversation history and working memory for an agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConversationContext {
    system_prompt: Option<String>,
    messages: Vec<Message>,
}

impl ConversationContext {
    pub fn new() -> Self {
        Self {
            system_prompt: None,
            messages: Vec::new(),
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn set_system_prompt(&mut self, prompt: impl Into<String>) {
        self.system_prompt = Some(prompt.into());
    }

    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty() && self.system_prompt.is_none()
    }

    pub fn len(&self) -> usize {
        self.messages.len() + usize::from(self.system_prompt.is_some())
    }

    /// Assembles the complete list of messages including the prepended system prompt.
    pub fn assemble_messages(&self) -> Vec<Message> {
        let mut assembled = Vec::with_capacity(self.messages.len() + 1);
        if let Some(ref sys) = self.system_prompt {
            assembled.push(Message::system(sys.clone()));
        }
        assembled.extend(self.messages.clone());
        assembled
    }

    /// Clears conversation history while keeping the system prompt intact.
    pub fn clear_history(&mut self) {
        self.messages.clear();
    }
}
