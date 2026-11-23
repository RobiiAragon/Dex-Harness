use std::io::{self, BufRead, Write};
use tracing::{debug, error, info};

use harness_core::{ToolCall, ToolRegistry};
use crate::error::McpError;
use crate::protocol::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, McpToolInfo};

/// An MCP Server that exposes a `ToolRegistry` over JSON-RPC 2.0 (stdio or SSE).
pub struct McpServer {
    tools: ToolRegistry,
    server_name: String,
    server_version: String,
}

impl McpServer {
    pub const PROTOCOL_VERSION: &'static str = "2024-11-05";

    pub fn new(tools: ToolRegistry) -> Self {
        Self {
            tools,
            server_name: "dex-harness".to_string(),
            server_version: "0.3.0".to_string(),
        }
    }

    pub fn with_info(mut self, name: impl Into<String>, version: impl Into<String>) -> Self {
        self.server_name = name.into();
        self.server_version = version.into();
        self
    }

    /// Handles an individual JSON-RPC 2.0 request or notification and produces an optional response.
    pub async fn handle_request(&self, request: JsonRpcRequest) -> Option<JsonRpcResponse> {
        debug!(method = %request.method, id = ?request.id, "Handling MCP request");

        let response = match request.method.as_str() {
            "initialize" => {
                let result = serde_json::json!({
                    "protocolVersion": Self::PROTOCOL_VERSION,
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": self.server_name,
                        "version": self.server_version,
                    }
                });
                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    result: Some(result),
                    error: None,
                }
            }
            "notifications/initialized" => {
                // Client initialized acknowledgement: notifications do not require a response
                return None;
            }
            "ping" => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: Some(serde_json::json!({})),
                error: None,
            },
            "tools/list" => {
                let tool_defs = self.tools.definitions();
                let mcp_tools: Vec<McpToolInfo> = tool_defs
                    .into_iter()
                    .map(|td| McpToolInfo {
                        name: td.name,
                        description: Some(td.description),
                        input_schema: td.parameters,
                    })
                    .collect();

                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    result: Some(serde_json::json!({ "tools": mcp_tools })),
                    error: None,
                }
            }
            "tools/call" => {
                let params = request.params.unwrap_or(serde_json::Value::Null);
                let tool_name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));

                let call_id = request.id.to_string();
                let call = ToolCall {
                    id: call_id,
                    name: tool_name.clone(),
                    arguments,
                };

                let tool_result = self.tools.execute(&call).await;

                let response_content = serde_json::json!([
                    {
                        "type": "text",
                        "text": tool_result.output,
                    }
                ]);

                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    result: Some(serde_json::json!({
                        "content": response_content,
                        "isError": tool_result.is_error,
                    })),
                    error: None,
                }
            }
            other => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("Method '{other}' not found"),
                    data: None,
                }),
            },
        };

        Some(response)
    }

    /// Runs the MCP server over standard I/O (stdio).
    /// Compatible with Claude Desktop, Cursor, Zed, and other MCP clients.
    pub async fn run_stdio(&self) -> Result<(), McpError> {
        info!("Starting Dex-Harness MCP Server on stdio");

        let stdin = io::stdin();
        let mut stdout = io::stdout();
        let reader = stdin.lock();

        for line_res in reader.lines() {
            let line = line_res.map_err(McpError::Io)?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            match serde_json::from_str::<JsonRpcRequest>(trimmed) {
                Ok(req) => {
                    if let Some(resp) = self.handle_request(req).await {
                        let payload = serde_json::to_string(&resp)?;
                        stdout.write_all(payload.as_bytes()).map_err(McpError::Io)?;
                        stdout.write_all(b"\n").map_err(McpError::Io)?;
                        stdout.flush().map_err(McpError::Io)?;
                    }
                }
                Err(e) => {
                    error!("Malformed JSON-RPC request: {e}");
                    let err_resp = JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: serde_json::Value::Null,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32700,
                            message: format!("Parse error: {e}"),
                            data: None,
                        }),
                    };
                    let payload = serde_json::to_string(&err_resp)?;
                    stdout.write_all(payload.as_bytes()).map_err(McpError::Io)?;
                    stdout.write_all(b"\n").map_err(McpError::Io)?;
                    stdout.flush().map_err(McpError::Io)?;
                }
            }
        }

        info!("MCP Server stdio loop closed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use async_trait::async_trait;
    use harness_core::{Tool, ToolError};

    struct DummyEchoTool;

    #[async_trait]
    impl Tool for DummyEchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes input"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": { "msg": { "type": "string" } }
            })
        }
        async fn execute(&self, args: serde_json::Value) -> Result<String, ToolError> {
            let msg = args.get("msg").and_then(|v| v.as_str()).unwrap_or("no-msg");
            Ok(format!("Echo: {msg}"))
        }
    }

    #[tokio::test]
    async fn test_mcp_server_initialize() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(DummyEchoTool));

        let server = McpServer::new(registry);
        let req = JsonRpcRequest::new(1, "initialize", None);

        let resp = server.handle_request(req).await.expect("Expected response");
        assert!(resp.error.is_none());
        let res = resp.result.expect("Expected result");
        assert_eq!(res["serverInfo"]["name"], "dex-harness");
        assert_eq!(res["protocolVersion"], "2024-11-05");
    }

    #[tokio::test]
    async fn test_mcp_server_tools_list_and_call() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(DummyEchoTool));

        let server = McpServer::new(registry);

        // Test tools/list
        let list_req = JsonRpcRequest::new(2, "tools/list", None);
        let list_resp = server.handle_request(list_req).await.unwrap();
        let list_res = list_resp.result.unwrap();
        let tools = list_res["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "echo");

        // Test tools/call
        let call_req = JsonRpcRequest::new(
            3,
            "tools/call",
            Some(serde_json::json!({
                "name": "echo",
                "arguments": { "msg": "hello from MCP" }
            })),
        );
        let call_resp = server.handle_request(call_req).await.unwrap();
        let call_res = call_resp.result.unwrap();
        assert_eq!(call_res["isError"], false);
        assert_eq!(call_res["content"][0]["text"], "Echo: hello from MCP");
    }
}
