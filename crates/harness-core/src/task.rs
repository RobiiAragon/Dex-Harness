use std::path::{Path, PathBuf};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::error::{CoreError, ToolError};
use crate::event::{DexEvent, EventBus};
use crate::tool::Tool;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    BlockedBy { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskItem {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: TaskStatus,
}

/// Persistent task board stored in JSON format (e.g. `.dex/tasks.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskBoard {
    pub tasks: Vec<TaskItem>,
    #[serde(skip)]
    file_path: Option<PathBuf>,
}

impl TaskBoard {
    pub const DEFAULT_PATH: &'static str = ".dex/tasks.json";

    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            file_path: Some(PathBuf::from(Self::DEFAULT_PATH)),
        }
    }

    pub fn with_path(mut self, path: impl AsRef<Path>) -> Self {
        self.file_path = Some(path.as_ref().to_path_buf());
        self
    }

    pub async fn load_or_default(path: impl AsRef<Path>) -> Self {
        let p = path.as_ref();
        if tokio::fs::try_exists(p).await.unwrap_or(false) {
            match tokio::fs::read_to_string(p).await {
                Ok(data) => match serde_json::from_str::<TaskBoard>(&data) {
                    Ok(mut board) => {
                        board.file_path = Some(p.to_path_buf());
                        return board;
                    }
                    Err(e) => {
                        debug!("Failed to parse tasks file '{:?}': {e}", p);
                    }
                },
                Err(e) => {
                    debug!("Failed to read tasks file '{:?}': {e}", p);
                }
            }
        }
        Self::new().with_path(p)
    }

    pub async fn save(&self) -> Result<(), CoreError> {
        if let Some(ref path) = self.file_path {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    tokio::fs::create_dir_all(parent).await.map_err(|e| {
                        CoreError::Internal(format!("Failed to create tasks directory: {e}"))
                    })?;
                }
            }
            let json = serde_json::to_string_pretty(self)?;
            tokio::fs::write(path, json).await.map_err(|e| {
                CoreError::Internal(format!("Failed to write tasks file: {e}"))
            })?;
        }
        Ok(())
    }

    pub fn add_task(&mut self, title: impl Into<String>, description: impl Into<String>) -> String {
        let id = format!("task_{}", self.tasks.len() + 1);
        self.tasks.push(TaskItem {
            id: id.clone(),
            title: title.into(),
            description: description.into(),
            status: TaskStatus::Pending,
        });
        id
    }

    pub fn update_status(&mut self, task_id: &str, status: TaskStatus) -> bool {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == task_id) {
            task.status = status;
            true
        } else {
            false
        }
    }

    pub fn export_markdown(&self) -> String {
        if self.tasks.is_empty() {
            return "No tasks currently recorded on the TaskBoard.".to_string();
        }

        let mut out = String::from("### 📋 Dex TaskBoard\n\n");
        for task in &self.tasks {
            let status_icon = match &task.status {
                TaskStatus::Pending => "⏳ [PENDING]",
                TaskStatus::InProgress => "🔄 [IN PROGRESS]",
                TaskStatus::Completed => "✅ [COMPLETED]",
                TaskStatus::BlockedBy { reason } => &format!("🚫 [BLOCKED: {reason}]"),
            };
            out.push_str(&format!(
                "- **{}** {}: {}\n  _{}_\n\n",
                task.id, status_icon, task.title, task.description
            ));
        }
        out
    }
}

/// Arguments for the TaskBoard agent tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskAction {
    /// List all tasks and their current statuses.
    List,
    /// Add a new task to the board.
    Add { title: String, description: String },
    /// Update status of an existing task.
    UpdateStatus {
        task_id: String,
        status: String,
        blocked_reason: Option<String>,
    },
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ManageTasksArgs {
    pub action: TaskAction,
}

/// Agent tool allowing inspection and update of the project task graph.
pub struct ManageTasksTool {
    board_path: PathBuf,
    event_bus: Option<EventBus>,
}

impl ManageTasksTool {
    pub fn new(board_path: impl AsRef<Path>) -> Self {
        Self {
            board_path: board_path.as_ref().to_path_buf(),
            event_bus: None,
        }
    }

    pub fn with_event_bus(mut self, bus: EventBus) -> Self {
        self.event_bus = Some(bus);
        self
    }
}

#[async_trait]
impl Tool for ManageTasksTool {
    fn name(&self) -> &str {
        "manage_tasks"
    }

    fn description(&self) -> &str {
        "Manages project tasks and objectives on the TaskBoard (.dex/tasks.json). Use this to track multi-step plans, mark progress, and avoid drifting on long tasks."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(ManageTasksArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "object",
                    "description": "Task action: list, add, or update_status"
                }
            },
            "required": ["action"]
        }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: ManageTasksArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        let mut board = TaskBoard::load_or_default(&self.board_path).await;

        match args.action {
            TaskAction::List => Ok(board.export_markdown()),
            TaskAction::Add { title, description } => {
                let id = board.add_task(&title, &description);
                let _ = board.save().await;
                info!(task_id = %id, "Added new task to TaskBoard");
                Ok(format!("Added task '{id}': {title}\n\n{}", board.export_markdown()))
            }
            TaskAction::UpdateStatus {
                task_id,
                status,
                blocked_reason,
            } => {
                let new_status = match status.to_lowercase().as_str() {
                    "pending" => TaskStatus::Pending,
                    "in_progress" => TaskStatus::InProgress,
                    "completed" => TaskStatus::Completed,
                    "blocked" => TaskStatus::BlockedBy {
                        reason: blocked_reason.unwrap_or_else(|| "Blocked by dependency".to_string()),
                    },
                    other => {
                        return Err(ToolError::InvalidArguments {
                            name: self.name().to_string(),
                            reason: format!(
                                "Unknown status '{other}'. Allowed: pending, in_progress, completed, blocked"
                            ),
                        });
                    }
                };

                if board.update_status(&task_id, new_status.clone()) {
                    let _ = board.save().await;
                    info!(task_id = %task_id, "Updated task status");
                    if let Some(ref bus) = self.event_bus {
                        bus.publish(DexEvent::TaskUpdated {
                            task_id: task_id.clone(),
                            status: format!("{:?}", new_status),
                        });
                    }
                    Ok(format!("Updated task '{task_id}'.\n\n{}", board.export_markdown()))
                } else {
                    Err(ToolError::NotFound {
                        name: format!("Task '{task_id}' not found on TaskBoard"),
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_taskboard_lifecycle() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("dex_test_tasks.json");

        let mut board = TaskBoard::new().with_path(&path);
        let id1 = board.add_task("Architecture Setup", "Design workspace layout");
        assert_eq!(id1, "task_1");

        let md1 = board.export_markdown();
        assert!(md1.contains("task_1"));
        assert!(md1.contains("⏳ [PENDING]"));

        assert!(board.update_status(&id1, TaskStatus::Completed));
        let md2 = board.export_markdown();
        assert!(md2.contains("✅ [COMPLETED]"));

        board.save().await.unwrap();
        assert!(tokio::fs::try_exists(&path).await.unwrap());

        let loaded = TaskBoard::load_or_default(&path).await;
        assert_eq!(loaded.tasks.len(), 1);
        assert_eq!(loaded.tasks[0].status, TaskStatus::Completed);

        let _ = tokio::fs::remove_file(path).await;
    }
}
