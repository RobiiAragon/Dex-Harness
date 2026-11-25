use std::sync::Arc;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::CoreError;
use crate::tool::ToolResult;

/// Agent execution security mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Full autonomy: executes all tools without prompting
    Autopilot,
    /// Safe mode: prompts for user approval before modifying files, running shell, or controlling desktop
    Safe,
    /// Plan mode: strictly read-only, prohibiting modifying tools or shell commands
    Plan,
}

/// Lifecycle events emitted during agent execution.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Dispatched right before a tool is executed.
    PreToolUse {
        tool_name: String,
        arguments: serde_json::Value,
    },
    /// Dispatched immediately after a tool finishes execution.
    PostToolUse {
        tool_name: String,
        result: ToolResult,
    },
    /// Dispatched when an error occurs during reasoning or tool execution.
    OnError {
        error: String,
    },
}

/// Action instructed by a hook in response to an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookAction {
    /// Proceed with normal execution.
    Continue,
    /// Halt agent execution immediately with the given reason (e.g. the user rejected a tool
    /// and wants to give new instructions). Conversation history is kept valid.
    Halt(String),
    /// Skip this tool call and report the reason back to the model as an error tool result,
    /// letting the agent adapt (e.g. a write attempted in read-only Plan mode).
    Deny(String),
    /// Replace the tool arguments before execution proceeds.
    ModifyArguments(serde_json::Value),
}

/// Trait implemented by lifecycle interceptors and security guards.
#[async_trait]
pub trait Hook: Send + Sync {
    async fn on_event(&self, event: &AgentEvent) -> Result<HookAction, CoreError>;
}

/// Registry of active lifecycle hooks.
#[derive(Default, Clone)]
pub struct HookRegistry {
    hooks: Vec<Arc<dyn Hook>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    pub fn register(&mut self, hook: Arc<dyn Hook>) {
        self.hooks.push(hook);
    }

    pub async fn trigger(&self, event: &AgentEvent) -> Result<HookAction, CoreError> {
        for hook in &self.hooks {
            match hook.on_event(event).await? {
                HookAction::Continue => continue,
                HookAction::Halt(reason) => {
                    debug!(reason = %reason, "Hook instructed agent to halt");
                    return Ok(HookAction::Halt(reason));
                }
                HookAction::Deny(reason) => {
                    debug!(reason = %reason, "Hook denied tool call");
                    return Ok(HookAction::Deny(reason));
                }
                HookAction::ModifyArguments(new_args) => {
                    return Ok(HookAction::ModifyArguments(new_args));
                }
            }
        }
        Ok(HookAction::Continue)
    }
}
