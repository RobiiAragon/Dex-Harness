use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use async_trait::async_trait;
use harness_core::{Tool, ToolError, ToolRegistry};
use harness_mcp::{JsonRpcRequest, JsonRpcResponse, McpToolInfo, McpTransport, StdioTransport};
use serde_json::json;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::error::ExtensionError;
use crate::manifest::ExtensionKind;
use crate::supervisor::ExtensionSupervisor;

const MCP_HANDSHAKE_TIMEOUT_SECS: u64 = 8;
const MCP_TOOL_CALL_TIMEOUT_SECS: u64 = 60;

static REQUEST_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_request_id() -> u64 {
    REQUEST_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
}

async fn send_mcp_request(
    transport: &StdioTransport,
    extension_id: &str,
    method: &str,
    params: Option<serde_json::Value>,
    timeout_secs: u64,
) -> Result<JsonRpcResponse, ExtensionError> {
    let req = JsonRpcRequest::new(json!(next_request_id()), method, params);
    let payload = serde_json::to_string(&req).map_err(|e| ExtensionError::Json {
        context: format!("MCP request '{method}' for '{extension_id}'"),
        source: e,
    })?;

    let io_future = async {
        transport
            .send(&payload)
            .await
            .map_err(|e| ExtensionError::Mcp {
                id: extension_id.to_string(),
                message: e.to_string(),
            })?;

        let raw = transport.receive().await.map_err(|e| ExtensionError::Mcp {
            id: extension_id.to_string(),
            message: e.to_string(),
        })?;

        serde_json::from_str::<JsonRpcResponse>(&raw).map_err(|e| ExtensionError::Json {
            context: format!("MCP response '{method}' from '{extension_id}'"),
            source: e,
        })
    };

    tokio::time::timeout(Duration::from_secs(timeout_secs), io_future)
        .await
        .map_err(|_| ExtensionError::Timeout {
            id: extension_id.to_string(),
            timeout_secs,
        })?
}

async fn perform_initialize(
    transport: &StdioTransport,
    extension_id: &str,
) -> Result<(), ExtensionError> {
    let init_params = json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": {
            "name": "dex-harness",
            "version": "0.1.0"
        }
    });

    let resp = send_mcp_request(
        transport,
        extension_id,
        "initialize",
        Some(init_params),
        MCP_HANDSHAKE_TIMEOUT_SECS,
    )
    .await?;

    if let Some(err) = resp.error {
        return Err(ExtensionError::Mcp {
            id: extension_id.to_string(),
            message: format!("MCP initialize error {}: {}", err.code, err.message),
        });
    }

    // Send initialized notification (best-effort)
    let notif = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    if let Ok(payload) = serde_json::to_string(&notif) {
        let _ = tokio::time::timeout(Duration::from_secs(5), transport.send(&payload)).await;
    }

    Ok(())
}

/// Adapter exposing a tool discovered from a managed MCP extension as a `harness_core::Tool`.
/// Automatically updates `last_used_at` in `ExtensionSupervisor` and lazily wakes sleeping processes.
pub struct ExtensionMcpTool {
    extension_id: String,
    info: McpToolInfo,
    supervisor: Arc<ExtensionSupervisor>,
    last_initialized_ptr: Mutex<Option<usize>>,
}

impl ExtensionMcpTool {
    pub fn new(
        extension_id: impl Into<String>,
        info: McpToolInfo,
        supervisor: Arc<ExtensionSupervisor>,
        initial_transport_ptr: usize,
    ) -> Self {
        Self {
            extension_id: extension_id.into(),
            info,
            supervisor,
            last_initialized_ptr: Mutex::new(Some(initial_transport_ptr)),
        }
    }
}

#[async_trait]
impl Tool for ExtensionMcpTool {
    fn name(&self) -> &str {
        &self.info.name
    }

    fn description(&self) -> &str {
        self.info.description.as_deref().unwrap_or("External MCP extension tool")
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.info.input_schema.clone()
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        // Wake extension if it was Sleeping/Stopped and touch last_used_at
        let transport = self
            .supervisor
            .get_mcp_transport(&self.extension_id)
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: e.to_string(),
            })?;

        let ptr = Arc::as_ptr(&transport) as usize;
        {
            let mut last_ptr = self.last_initialized_ptr.lock().await;
            if *last_ptr != Some(ptr) {
                debug!(
                    extension = %self.extension_id,
                    tool = %self.info.name,
                    "Re-initializing lazily woken MCP extension"
                );
                perform_initialize(&transport, &self.extension_id)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed {
                        name: self.name().to_string(),
                        message: e.to_string(),
                    })?;
                *last_ptr = Some(ptr);
            }
        }

        let response = send_mcp_request(
            &transport,
            &self.extension_id,
            "tools/call",
            Some(json!({
                "name": self.info.name,
                "arguments": arguments,
            })),
            MCP_TOOL_CALL_TIMEOUT_SECS,
        )
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: e.to_string(),
        })?;

        self.supervisor.touch_last_used(&self.extension_id).await;

        if let Some(err) = response.error {
            return Err(ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: format!("MCP error {}: {}", err.code, err.message),
            });
        }

        let result = response.result.unwrap_or(serde_json::Value::Null);
        Ok(extract_mcp_text(&result))
    }
}

fn extract_mcp_text(result: &serde_json::Value) -> String {
    if let Some(content_arr) = result.get("content").and_then(|v| v.as_array()) {
        let mut texts = Vec::new();
        for item in content_arr {
            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                texts.push(text.to_string());
            }
        }
        if !texts.is_empty() {
            return texts.join("\n");
        }
    }
    result.to_string()
}

/// Connects to a single `McpStdio` extension, performs `initialize` + `tools/list`,
/// and registers every discovered tool into `registry` as `ToolVisibility::Deferred`.
pub async fn connect_mcp_extension(
    supervisor: Arc<ExtensionSupervisor>,
    extension_id: &str,
    registry: &mut ToolRegistry,
) -> Result<Vec<String>, ExtensionError> {
    let reg = supervisor.load_registry().await;
    let manifest = reg
        .get(extension_id)
        .cloned()
        .ok_or_else(|| ExtensionError::NotFound {
            id: extension_id.to_string(),
        })?;

    if !matches!(manifest.kind, ExtensionKind::McpStdio { .. }) {
        return Err(ExtensionError::InvalidConfig {
            id: extension_id.to_string(),
            reason: "Only McpStdio extensions support direct stdio tool discovery".to_string(),
        });
    }

    let transport = supervisor.get_mcp_transport(extension_id).await?;
    perform_initialize(&transport, extension_id).await?;

    let list_resp = send_mcp_request(
        &transport,
        extension_id,
        "tools/list",
        Some(json!({})),
        MCP_HANDSHAKE_TIMEOUT_SECS,
    )
    .await?;

    if let Some(err) = list_resp.error {
        return Err(ExtensionError::Mcp {
            id: extension_id.to_string(),
            message: format!("MCP tools/list error {}: {}", err.code, err.message),
        });
    }

    let tools_value = list_resp
        .result
        .and_then(|r| r.get("tools").cloned())
        .unwrap_or_else(|| json!([]));

    let tool_infos: Vec<McpToolInfo> =
        serde_json::from_value(tools_value).map_err(|e| ExtensionError::Json {
            context: format!("tools/list items for '{extension_id}'"),
            source: e,
        })?;

    let transport_ptr = Arc::as_ptr(&transport) as usize;
    let mut registered_names = Vec::with_capacity(tool_infos.len());

    let mut base_tags = manifest.tags.clone();
    base_tags.push(manifest.id.clone());
    base_tags.push(manifest.name.to_lowercase());
    base_tags.push(harness_core::MCP_EXTENSION_TAG.to_string());

    for info in tool_infos {
        let tool_name = info.name.clone();
        let adapter = ExtensionMcpTool::new(
            extension_id,
            info,
            Arc::clone(&supervisor),
            transport_ptr,
        );
        registry.register_deferred(
            Arc::new(adapter),
            manifest.category.as_str(),
            base_tags.clone(),
        );
        registered_names.push(tool_name);
    }

    info!(
        extension = %extension_id,
        count = registered_names.len(),
        "Registered deferred MCP extension tools"
    );

    Ok(registered_names)
}

/// Outcome of connecting all enabled MCP extensions at agent startup.
#[derive(Debug, Default, Clone)]
pub struct McpStartupReport {
    /// Names of the deferred tools that were registered.
    pub registered_tools: Vec<String>,
    /// `(extension_id, error)` pairs for extensions that could not be reached.
    pub failed: Vec<(String, String)>,
}

/// Connects all enabled `McpStdio` extensions **concurrently** and registers their tools as
/// `Deferred`. A slow or broken server can no longer stall startup for every other one: each
/// handshake is bounded by `MCP_HANDSHAKE_TIMEOUT_SECS` and they all run in parallel.
/// Unreachable servers are reported (and logged at `debug`) without aborting agent startup.
pub async fn register_enabled_mcp_extensions(
    supervisor: Arc<ExtensionSupervisor>,
    registry: &mut ToolRegistry,
) -> Vec<String> {
    connect_enabled_mcp_extensions(supervisor, registry)
        .await
        .registered_tools
}

/// Same as [`register_enabled_mcp_extensions`] but also returns which extensions failed.
pub async fn connect_enabled_mcp_extensions(
    supervisor: Arc<ExtensionSupervisor>,
    registry: &mut ToolRegistry,
) -> McpStartupReport {
    let reg = supervisor.load_registry().await;
    let mut set = tokio::task::JoinSet::new();

    for manifest in reg.extensions {
        if !manifest.enabled || !matches!(manifest.kind, ExtensionKind::McpStdio { .. }) {
            continue;
        }
        let sup = Arc::clone(&supervisor);
        // `ToolRegistry` clones share the same inner registry, so tools land in `registry`.
        let mut shared = registry.clone();
        set.spawn(async move {
            let id = manifest.id.clone();
            let res = connect_mcp_extension(sup, &id, &mut shared).await;
            (id, res)
        });
    }

    let mut report = McpStartupReport::default();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((_, Ok(names))) => report.registered_tools.extend(names),
            Ok((id, Err(e))) => {
                debug!(extension = %id, error = %e, "Skipping enabled MCP extension that failed to connect");
                report.failed.push((id, e.to_string()));
            }
            Err(join_err) => {
                warn!(error = %join_err, "MCP extension connect task panicked or was cancelled");
            }
        }
    }
    report.failed.sort();
    report
}
