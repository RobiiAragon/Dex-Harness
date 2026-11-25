use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock, Weak};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

use crate::error::ToolError;
use crate::event::{DexEvent, EventBus};

/// Defines a tool's contract and parameters schema passed to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Controls whether a tool's JSON Schema is always exposed to the LLM
/// or deferred until dynamically discovered via `search_tools`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ToolVisibility {
    /// Core tool always visible in `active_definitions()`.
    #[default]
    Always,
    /// Extension / external tool hidden by default to protect local LLM context windows,
    /// discoverable and activatable on demand via `search_tools`.
    Deferred {
        category: String,
        tags: Vec<String>,
    },
}

/// Summary information for a deferred tool returned by catalog inspection or search.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeferredToolInfo {
    pub name: String,
    pub description: String,
    pub category: String,
    pub tags: Vec<String>,
    pub active: bool,
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
    pub fn success(
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
        output: impl Into<String>,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            tool_name: tool_name.into(),
            output: output.into(),
            is_error: false,
        }
    }

    pub fn error(
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
        error_message: impl Into<String>,
    ) -> Self {
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

#[derive(Clone)]
struct RegisteredTool {
    tool: Arc<dyn Tool>,
    visibility: ToolVisibility,
}

#[derive(Default)]
struct RegistryInner {
    tools: HashMap<String, RegisteredTool>,
    active_deferred: HashSet<String>,
}

/// Non-owning handle to a `ToolRegistry` used by `SearchToolsTool` to prevent `Arc` cycles.
#[derive(Clone)]
pub struct WeakToolRegistry {
    inner: Weak<RwLock<RegistryInner>>,
}

impl WeakToolRegistry {
    pub fn upgrade(&self) -> Option<ToolRegistry> {
        self.inner.upgrade().map(|inner| ToolRegistry { inner })
    }
}

/// Central catalog storing and dispatching tools available to an agent.
/// Supports `Always` (core) and `Deferred` (extension) tool visibility with dynamic activation.
#[derive(Clone)]
pub struct ToolRegistry {
    inner: Arc<RwLock<RegistryInner>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(RegistryInner::default())),
        }
    }

    /// Registers a core tool with `ToolVisibility::Always`.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.register_with_visibility(tool, ToolVisibility::Always);
    }

    /// Registers a tool with an explicit visibility mode (`Always` or `Deferred`).
    pub fn register_with_visibility(&mut self, tool: Arc<dyn Tool>, visibility: ToolVisibility) {
        let name = tool.name().to_string();
        info!(tool = %name, visibility = ?visibility, "Registered tool");
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.tools.insert(name, RegisteredTool { tool, visibility });
    }

    /// Convenience helper to register an external/extension tool as `ToolVisibility::Deferred`.
    pub fn register_deferred(
        &mut self,
        tool: Arc<dyn Tool>,
        category: impl Into<String>,
        tags: Vec<String>,
    ) {
        self.register_with_visibility(
            tool,
            ToolVisibility::Deferred {
                category: category.into(),
                tags,
            },
        );
    }

    /// Registers the native `search_tools` meta-tool wired to this registry via a weak handle.
    pub fn register_search_tools(&mut self, event_bus: Option<EventBus>) {
        let tool = SearchToolsTool::new(self.weak_handle(), event_bus);
        self.register(Arc::new(tool));
    }

    /// Creates a weak reference handle for meta-tools to avoid `Arc` cycles.
    pub fn weak_handle(&self) -> WeakToolRegistry {
        WeakToolRegistry {
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// Creates an independent snapshot copy of currently registered tools (without sharing locks).
    /// Useful when delegating tools to isolated subagents without creating `Arc` reference cycles.
    pub fn snapshot(&self) -> Self {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let inner = RegistryInner {
            tools: guard.tools.clone(),
            active_deferred: guard.active_deferred.clone(),
        };
        Self {
            inner: Arc::new(RwLock::new(inner)),
        }
    }

    /// Retrieves a tool by name regardless of visibility.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.tools.get(name).map(|entry| entry.tool.clone())
    }

    /// Returns the visibility metadata for a registered tool.
    pub fn visibility(&self, name: &str) -> Option<ToolVisibility> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.tools.get(name).map(|entry| entry.visibility.clone())
    }

    /// Activates a deferred tool so it appears in `active_definitions()`.
    /// Returns `true` if the tool exists and is a `Deferred` tool.
    pub fn activate_tool(&self, name: &str) -> bool {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let is_deferred = matches!(
            guard.tools.get(name).map(|e| &e.visibility),
            Some(ToolVisibility::Deferred { .. })
        );
        if is_deferred {
            guard.active_deferred.insert(name.to_string());
            true
        } else {
            false
        }
    }

    /// Deactivates a previously activated deferred tool.
    pub fn deactivate_tool(&self, name: &str) -> bool {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.active_deferred.remove(name)
    }

    /// Returns true if the tool is currently exposed to the LLM (`Always` or activated `Deferred`).
    pub fn is_active(&self, name: &str) -> bool {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match guard.tools.get(name) {
            Some(entry) => match entry.visibility {
                ToolVisibility::Always => true,
                ToolVisibility::Deferred { .. } => guard.active_deferred.contains(name),
            },
            None => false,
        }
    }

    /// Returns definitions for ALL registered tools (core + deferred), sorted by name.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut defs: Vec<ToolDefinition> = guard
            .tools
            .values()
            .map(|entry| entry.tool.definition())
            .collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }

    /// Returns definitions ONLY for `Always` tools plus `Deferred` tools activated in this session.
    pub fn active_definitions(&self) -> Vec<ToolDefinition> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut defs: Vec<ToolDefinition> = guard
            .tools
            .iter()
            .filter_map(|(name, entry)| match entry.visibility {
                ToolVisibility::Always => Some(entry.tool.definition()),
                ToolVisibility::Deferred { .. } => {
                    if guard.active_deferred.contains(name) {
                        Some(entry.tool.definition())
                    } else {
                        None
                    }
                }
            })
            .collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }

    /// Lists all deferred tools with their category, tags, and activation state.
    pub fn deferred_tools(&self) -> Vec<DeferredToolInfo> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut list: Vec<DeferredToolInfo> = guard
            .tools
            .iter()
            .filter_map(|(name, entry)| match &entry.visibility {
                ToolVisibility::Deferred { category, tags } => Some(DeferredToolInfo {
                    name: name.clone(),
                    description: entry.tool.description().to_string(),
                    category: category.clone(),
                    tags: tags.clone(),
                    active: guard.active_deferred.contains(name),
                }),
                ToolVisibility::Always => None,
            })
            .collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    /// Searches deferred tools by query keywords (matching name, category, tags, and description)
    /// and optional category filter.
    pub fn search_deferred(&self, query: &str, category: Option<&str>) -> Vec<DeferredToolInfo> {
        let trimmed = query.trim().to_lowercase();
        let cat_filter = category
            .map(|c| c.trim().to_lowercase())
            .filter(|c| !c.is_empty());

        let tokens: Vec<&str> = if trimmed.is_empty() || trimmed == "*" {
            Vec::new()
        } else {
            trimmed.split_whitespace().collect()
        };

        let mut scored: Vec<(usize, DeferredToolInfo)> = self
            .deferred_tools()
            .into_iter()
            .filter_map(|info| {
                if let Some(ref cat) = cat_filter {
                    if !info.category.to_lowercase().contains(cat) {
                        return None;
                    }
                }

                if tokens.is_empty() {
                    return Some((1, info));
                }

                let name_lc = info.name.to_lowercase();
                let desc_lc = info.description.to_lowercase();
                let cat_lc = info.category.to_lowercase();
                let tags_lc: Vec<String> = info.tags.iter().map(|t| t.to_lowercase()).collect();

                let mut score = 0usize;
                for token in &tokens {
                    if name_lc.contains(token) {
                        score += 4;
                    }
                    if tags_lc.iter().any(|t| t.contains(token)) {
                        score += 3;
                    }
                    if cat_lc.contains(token) {
                        score += 2;
                    }
                    if desc_lc.contains(token) {
                        score += 1;
                    }
                }

                if score > 0 {
                    Some((score, info))
                } else {
                    None
                }
            })
            .collect();

        // On equal scores, built-in Dex tools come before MCP extension tools: the latter only
        // work while their host app (Unity Editor, ComfyUI, Blender…) is running.
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| is_mcp_extension(&a.1).cmp(&is_mcp_extension(&b.1)))
                .then_with(|| a.1.name.cmp(&b.1.name))
        });
        scored.into_iter().map(|(_, info)| info).collect()
    }

    /// Returns total count of deferred tools currently registered.
    pub fn deferred_count(&self) -> usize {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard
            .tools
            .values()
            .filter(|e| matches!(e.visibility, ToolVisibility::Deferred { .. }))
            .count()
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

fn default_activate_true() -> bool {
    true
}

/// Input arguments for the `search_tools` meta-tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchToolsArgs {
    /// Keywords matching tool name, category, tags, or description. Use "*" to list all deferred tools.
    pub query: String,
    /// Optional extension category filter (e.g. "audio", "image_video", "three_d", "ocr_docs", "game_dev", "custom").
    #[serde(default)]
    pub category: Option<String>,
    /// When true (default), activates matched tools so their schemas are exposed on the next step.
    #[serde(default = "default_activate_true")]
    pub activate: bool,
}

/// Tag carried by every tool bridged from an external MCP server.
pub const MCP_EXTENSION_TAG: &str = "mcp-extension";

fn is_mcp_extension(info: &DeferredToolInfo) -> bool {
    info.tags.iter().any(|t| t == MCP_EXTENSION_TAG)
}

/// Meta-tool that discovers deferred extension tools and dynamically activates them into the LLM context.
pub struct SearchToolsTool {
    registry: WeakToolRegistry,
    event_bus: Option<EventBus>,
}

impl SearchToolsTool {
    pub fn new(registry: WeakToolRegistry, event_bus: Option<EventBus>) -> Self {
        Self {
            registry,
            event_bus,
        }
    }
}

#[async_trait]
impl Tool for SearchToolsTool {
    fn name(&self) -> &str {
        "search_tools"
    }

    fn description(&self) -> &str {
        "Searches deferred external extension and MCP tools by name, category, tags, or description. Set `activate: true` to promote matching tools into your active toolset for subsequent steps."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(SearchToolsArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "category": { "type": "string" },
                "activate": { "type": "boolean" }
            },
            "required": ["query"]
        }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: SearchToolsArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: "ToolRegistry is no longer available".to_string(),
            })?;

        let matches = registry.search_deferred(&args.query, args.category.as_deref());
        if matches.is_empty() {
            let total_deferred = registry.deferred_count();
            return Ok(format!(
                "No deferred tools matched query '{}' (category: {:?}). Total deferred tools available: {}.",
                args.query, args.category, total_deferred
            ));
        }

        let mut newly_activated = Vec::new();
        let mut lines = Vec::with_capacity(matches.len() + 2);
        lines.push(format!(
            "Found {} matching deferred tool(s) (activate={}):",
            matches.len(),
            args.activate
        ));

        for m in &matches {
            let now_active = if args.activate {
                if !m.active && registry.activate_tool(&m.name) {
                    newly_activated.push(m.name.clone());
                }
                true
            } else {
                m.active
            };

            let status = if now_active { "ACTIVE" } else { "DEFERRED" };
            let source = if is_mcp_extension(m) {
                "MCP extension — needs its app/server running; prefer a built-in tool when one fits"
            } else {
                "built-in"
            };
            lines.push(format!(
                "- {} [{}] ({source}; category: {}, tags: [{}]): {}",
                m.name,
                status,
                m.category,
                m.tags.join(", "),
                m.description
            ));
        }

        if !newly_activated.is_empty() {
            if let Some(ref bus) = self.event_bus {
                bus.publish(DexEvent::ToolsActivated {
                    source: "search_tools".to_string(),
                    tool_names: newly_activated.clone(),
                });
            }
            lines.push(format!(
                "\nActivated {} tool(s) ({}). You may now invoke them directly in your next step.",
                newly_activated.len(),
                newly_activated.join(", ")
            ));
        }

        Ok(lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyTool {
        name: String,
        desc: String,
    }

    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            &self.desc
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object", "properties": {} })
        }

        async fn execute(&self, _args: serde_json::Value) -> Result<String, ToolError> {
            Ok(format!("ran {}", self.name))
        }
    }

    #[test]
    fn search_lists_builtin_tools_before_mcp_extension_tools() {
        let mut registry = ToolRegistry::new();
        registry.register_deferred(
            Arc::new(DummyTool {
                name: "generate_image".to_string(),
                desc: "Unity: generate an image asset".to_string(),
            }),
            "gamedev",
            vec!["unity".to_string(), MCP_EXTENSION_TAG.to_string()],
        );
        registry.register_deferred(
            Arc::new(DummyTool {
                name: "image_generate".to_string(),
                desc: "Generate an image locally".to_string(),
            }),
            "image",
            vec!["image".to_string()],
        );
        let names: Vec<String> = registry
            .search_deferred("*", None)
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, vec!["image_generate", "generate_image"]);
    }

    #[tokio::test]
    async fn test_core_vs_deferred_visibility_and_search_tools_activation() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        let mut registry = ToolRegistry::new();

        registry.register(Arc::new(DummyTool {
            name: "read_file".to_string(),
            desc: "Reads files from disk".to_string(),
        }));
        registry.register_search_tools(Some(bus.clone()));

        registry.register_deferred(
            Arc::new(DummyTool {
                name: "blender_render_scene".to_string(),
                desc: "Renders a 3D Blender scene to PNG".to_string(),
            }),
            "three_d",
            vec!["blender".to_string(), "3d".to_string(), "render".to_string()],
        );
        registry.register_deferred(
            Arc::new(DummyTool {
                name: "soundbox_generate_sfx".to_string(),
                desc: "Generates audio sound effects".to_string(),
            }),
            "audio",
            vec!["sfx".to_string(), "soundbox".to_string()],
        );

        // Initially only core tools (read_file + search_tools) are active
        let active_initial: Vec<String> = registry
            .active_definitions()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(active_initial, vec!["read_file", "search_tools"]);
        assert_eq!(registry.definitions().len(), 4);
        assert!(!registry.is_active("blender_render_scene"));

        // Search without activating
        let res_preview = registry
            .execute(&ToolCall {
                id: "call_1".to_string(),
                name: "search_tools".to_string(),
                arguments: serde_json::json!({
                    "query": "blender",
                    "activate": false
                }),
            })
            .await;
        assert!(!res_preview.is_error);
        assert!(res_preview.output.contains("blender_render_scene"));
        assert!(!registry.is_active("blender_render_scene"));

        // Search and activate
        let res_activate = registry
            .execute(&ToolCall {
                id: "call_2".to_string(),
                name: "search_tools".to_string(),
                arguments: serde_json::json!({
                    "query": "render",
                    "category": "three_d",
                    "activate": true
                }),
            })
            .await;
        assert!(!res_activate.is_error);
        assert!(registry.is_active("blender_render_scene"));
        assert!(!registry.is_active("soundbox_generate_sfx"));

        let active_after: Vec<String> = registry
            .active_definitions()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(
            active_after,
            vec!["blender_render_scene", "read_file", "search_tools"]
        );

        // Verify ToolsActivated event emitted on EventBus
        let event = rx.try_recv().expect("expected ToolsActivated event");
        match event {
            DexEvent::ToolsActivated { source, tool_names } => {
                assert_eq!(source, "search_tools");
                assert_eq!(tool_names, vec!["blender_render_scene"]);
            }
            other => panic!("unexpected event: {:?}", other),
        }
    }
}
