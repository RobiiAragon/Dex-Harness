use std::collections::HashMap;
use std::sync::Arc;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

use crate::error::ToolError;

/// Defines a tool's contract and parameters schema passed to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Represents an LLM's invocation of a specific tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Output returned by a tool after execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub tool_name: String,
    pub output: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(call_id: impl Into<String>, tool_name: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            tool_name: tool_name.into(),
            output: output.into(),
            is_error: false,
        }
    }

    pub fn error(call_id: impl Into<String>, tool_name: impl Into<String>, error_message: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            tool_name: tool_name.into(),
            output: error_message.into(),
            is_error: true,
        }
    }
}

/// Core interface that all tools in Dex-Harness must implement.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique identifier for the tool (e.g. "read_file", "execute_shell").
    fn name(&self) -> &str;

    /// Clear, concise instructions explaining what the tool does and when to call it.
    fn description(&self) -> &str;

    /// Dynamic JSON Schema describing the accepted parameters.
    fn parameters_schema(&self) -> serde_json::Value;

    /// Executes the tool with the given JSON arguments.
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError>;

    /// Converts this tool instance into an LLM-compatible ToolDefinition.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }
}

/// Central catalog storing and dispatching tools available to an agent.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Registers a tool into the catalog.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        info!(tool = tool.name(), "Registered tool");
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Retrieves a tool by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Returns list of tool definitions for sending to the LLM.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|t| t.definition()).collect()
    }

    /// Dispatches execution of a tool call and wraps the outcome into a ToolResult.
    pub async fn execute(&self, call: &ToolCall) -> ToolResult {
        debug!(tool = %call.name, call_id = %call.id, "Executing tool call");
        let tool = match self.get(&call.name) {
            Some(t) => t,
            None => {
                let err_msg = format!("Tool '{}' is not registered in this harness", call.name);
                error!(tool = %call.name, "{}", err_msg);
                return ToolResult::error(&call.id, &call.name, err_msg);
            }
        };

        match tool.execute(call.arguments.clone()).await {
            Ok(output) => {
                debug!(tool = %call.name, bytes = output.len(), "Tool execution succeeded");
                ToolResult::success(&call.id, &call.name, output)
            }
            Err(e) => {
                let err_msg = e.to_string();
                error!(tool = %call.name, error = %err_msg, "Tool execution failed");
                ToolResult::error(&call.id, &call.name, err_msg)
            }
        }
    }
}
