use std::sync::Arc;
use async_trait::async_trait;
use serde_json::json;
use harness_core::{Tool, ToolError};
use crate::protocol::{JsonRpcRequest, JsonRpcResponse, McpToolInfo};
use crate::transport::McpTransport;

/// Adapter that exposes an MCP server tool as a native `harness_core::Tool`.
pub struct McpTool {
    info: McpToolInfo,
    transport: Arc<dyn McpTransport>,
}

impl McpTool {
    pub fn new(info: McpToolInfo, transport: Arc<dyn McpTransport>) -> Self {
        Self { info, transport }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.info.name
    }

    fn description(&self) -> &str {
        self.info.description.as_deref().unwrap_or("MCP tool")
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.info.input_schema.clone()
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let request = JsonRpcRequest::new(
            json!(1),
            "tools/call",
            Some(json!({
                "name": self.info.name,
                "arguments": arguments,
            })),
        );

        let payload = serde_json::to_string(&request).map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: e.to_string(),
        })?;

        self.transport.send(&payload).await.map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: e.to_string(),
        })?;

        let response_str = self.transport.receive().await.map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: e.to_string(),
        })?;

        let response: JsonRpcResponse = serde_json::from_str(&response_str).map_err(|e| {
            ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: format!("Malformed MCP JSON-RPC response: {e}"),
            }
        })?;

        if let Some(err) = response.error {
            return Err(ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: format!("MCP error {}: {}", err.code, err.message),
            });
        }

        let result = response.result.unwrap_or(serde_json::Value::Null);
        Ok(result.to_string())
    }
}
