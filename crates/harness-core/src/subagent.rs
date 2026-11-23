use std::sync::Arc;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::{info, instrument};

use crate::agent::{Agent, ReActAgent};
use crate::error::ToolError;
use crate::event::{DexEvent, EventBus};
use crate::provider::LlmProvider;
use crate::tool::{Tool, ToolRegistry};

/// Arguments for spawning a concurrent subagent.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpawnSubagentArgs {
    /// Role of the subagent: "Explorer" (read-only search), "Coder" (edit/write/shell), or "Reviewer" (diff/git/verify).
    pub role: String,
    /// Detailed objective or question for the subagent to solve autonomously.
    pub task: String,
    /// Maximum ReAct steps allowed for this subagent. Defaults to 8.
    pub max_steps: Option<usize>,
}

/// Tool enabling the primary orchestrator to spawn isolated, concurrent subagents.
pub struct SpawnSubagentTool {
    provider: Arc<dyn LlmProvider>,
    master_tools: ToolRegistry,
    event_bus: Option<EventBus>,
}

impl SpawnSubagentTool {
    pub fn new(provider: Arc<dyn LlmProvider>, master_tools: ToolRegistry) -> Self {
        Self {
            provider,
            master_tools,
            event_bus: None,
        }
    }

    pub fn with_event_bus(mut self, bus: EventBus) -> Self {
        self.event_bus = Some(bus);
        self
    }

    /// Filters the master tool registry according to the subagent's specialization.
    fn filter_tools_for_role(&self, role: &str) -> (ToolRegistry, String) {
        let mut registry = ToolRegistry::new();

        match role.to_lowercase().as_str() {
            "explorer" => {
                for name in &["read_file", "find_files", "grep_search"] {
                    if let Some(tool) = self.master_tools.get(name) {
                        registry.register(tool);
                    }
                }
                let prompt = "You are an autonomous Research & Exploration Subagent.\n\
                              Your objective is to quickly search, find, and read project files to answer questions.\n\
                              Provide a concise, accurate summary of your findings to the orchestrator.".to_string();
                (registry, prompt)
            }
            "coder" => {
                for name in &["read_file", "write_file", "edit_file", "execute_shell", "pty_shell"] {
                    if let Some(tool) = self.master_tools.get(name) {
                        registry.register(tool);
                    }
                }
                let prompt = "You are an autonomous Software Engineering Subagent.\n\
                              Your objective is to implement, edit, and verify code modifications.\n\
                              Test your work and report the outcome to the orchestrator.".to_string();
                (registry, prompt)
            }
            "reviewer" => {
                for name in &["read_file", "grep_search", "git_worktree", "execute_shell"] {
                    if let Some(tool) = self.master_tools.get(name) {
                        registry.register(tool);
                    }
                }
                let prompt = "You are an autonomous Code Reviewer Subagent.\n\
                              Your objective is to verify correctness, style, test results, and catch regressions.\n\
                              Report a structured review to the orchestrator.".to_string();
                (registry, prompt)
            }
            _ => {
                // Default: allow all tools
                for def in self.master_tools.definitions() {
                    if let Some(tool) = self.master_tools.get(&def.name) {
                        registry.register(tool);
                    }
                }
                let prompt = "You are an autonomous Specialized Subagent working on a delegated task.\n\
                              Complete the objective and report your result concisely.".to_string();
                (registry, prompt)
            }
        }
    }
}

#[async_trait]
impl Tool for SpawnSubagentTool {
    fn name(&self) -> &str {
        "spawn_subagent"
    }

    fn description(&self) -> &str {
        "Delegates a subtask to an isolated, specialized subagent (Explorer, Coder, Reviewer). Runs with a clean context and restricted tools, returning its findings upon completion."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(SpawnSubagentArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "role": { "type": "string", "enum": ["Explorer", "Coder", "Reviewer"] },
                "task": { "type": "string", "description": "Subtask objective" },
                "max_steps": { "type": "integer", "description": "Max steps (default 8)" }
            },
            "required": ["role", "task"]
        }))
    }

    #[instrument(skip(self, arguments))]
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: SpawnSubagentArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        let role = args.role.clone();
        let task = args.task.clone();
        let max_steps = args.max_steps.unwrap_or(8);

        info!(role = %role, task = %task, "Spawning subagent");

        if let Some(ref bus) = self.event_bus {
            bus.publish(DexEvent::SubagentSpawned {
                role: role.clone(),
                task: task.clone(),
            });
        }

        let (sub_tools, system_prompt) = self.filter_tools_for_role(&role);
        let provider = self.provider.clone();
        let sub_bus = self.event_bus.clone();

        // Spawn subagent execution in a separate tokio async task
        let handle = tokio::spawn(async move {
            let mut subagent = ReActAgent::new(provider, sub_tools)
                .with_system_prompt(system_prompt)
                .with_max_steps(max_steps);

            if let Some(bus) = sub_bus {
                subagent = subagent.with_event_bus(bus);
            }

            subagent.run(&task).await
        });

        match handle.await {
            Ok(Ok(answer)) => {
                if let Some(ref bus) = self.event_bus {
                    bus.publish(DexEvent::SubagentCompleted {
                        role: role.clone(),
                        result: answer.clone(),
                    });
                }
                Ok(format!(
                    "=== 🤖 Subagent [{role}] Finished Successfully ===\n\n{answer}"
                ))
            }
            Ok(Err(e)) => Err(ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: format!("Subagent [{role}] failed: {e}"),
            }),
            Err(e) => Err(ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: format!("Subagent task panicked or failed to join: {e}"),
            }),
        }
    }
}
