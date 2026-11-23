use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;
use tokio::process::Command;
use tracing::instrument;

use harness_core::{Tool, ToolError};

/// Allowed actions for the Git Worktree tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum GitWorktreeAction {
    /// List all currently active Git worktrees.
    List,
    /// Add a new worktree at the specified path for an existing or new branch.
    Add,
    /// Remove an existing worktree at the specified path.
    Remove,
}

/// Input parameters for Git worktree management.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GitWorktreeArgs {
    /// The action to perform: "list", "add", or "remove".
    pub action: GitWorktreeAction,
    /// Path for the worktree (required for "add" and "remove").
    pub path: Option<String>,
    /// Git branch name (optional for "add").
    pub branch: Option<String>,
    /// Create a new branch with this name when adding the worktree.
    pub create_branch: Option<bool>,
}

#[derive(Default, Clone)]
pub struct GitWorktreeTool {
    workspace_root: Option<PathBuf>,
}

impl GitWorktreeTool {
    pub fn new() -> Self {
        Self { workspace_root: None }
    }

    pub fn with_workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    fn git_command(&self) -> Command {
        let mut cmd = Command::new("git");
        harness_core::apply_enriched_path(&mut cmd);
        if let Some(ref root) = self.workspace_root {
            cmd.current_dir(root);
        }
        cmd
    }
}

#[async_trait]
impl Tool for GitWorktreeTool {
    fn name(&self) -> &str {
        "git_worktree"
    }

    fn description(&self) -> &str {
        "Manages isolated Git worktrees (list, add, remove) to allow concurrent branches, isolated testing, or subagent work without altering the primary working directory."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(GitWorktreeArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["list", "add", "remove"] },
                "path": { "type": "string", "description": "Target worktree path" },
                "branch": { "type": "string", "description": "Git branch name" },
                "create_branch": { "type": "boolean", "description": "Create new branch" }
            },
            "required": ["action"]
        }))
    }

    #[instrument(skip(self, arguments))]
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: GitWorktreeArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        match args.action {
            GitWorktreeAction::List => {
                let output = self
                    .git_command()
                    .args(["worktree", "list"])
                    .output()
                    .await
                    .map_err(|e| ToolError::Io {
                        name: self.name().to_string(),
                        message: format!("Failed to invoke 'git worktree list': {e}"),
                    })?;

                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.trim().is_empty() {
                    Ok("No active Git worktrees found or not in a Git repository.".to_string())
                } else {
                    Ok(format!("Active Git Worktrees:\n{}", stdout))
                }
            }
            GitWorktreeAction::Add => {
                let path = args.path.ok_or_else(|| ToolError::InvalidArguments {
                    name: self.name().to_string(),
                    reason: "Missing 'path' argument for 'add' action".to_string(),
                })?;

                let mut cmd = self.git_command();
                cmd.arg("worktree").arg("add");

                if let Some(true) = args.create_branch {
                    if let Some(ref branch) = args.branch {
                        cmd.arg("-b").arg(branch);
                    }
                }

                cmd.arg(&path);

                if let Some(false) | None = args.create_branch {
                    if let Some(ref branch) = args.branch {
                        cmd.arg(branch);
                    }
                }

                let output = cmd.output().await.map_err(|e| ToolError::Io {
                    name: self.name().to_string(),
                    message: format!("Failed to invoke 'git worktree add': {e}"),
                })?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(ToolError::ExecutionFailed {
                        name: self.name().to_string(),
                        message: format!("Failed to create worktree: {stderr}"),
                    });
                }

                let out = String::from_utf8_lossy(&output.stderr); // Git prints worktree creation info to stderr
                Ok(format!("Worktree added successfully at '{path}':\n{out}"))
            }
            GitWorktreeAction::Remove => {
                let path = args.path.ok_or_else(|| ToolError::InvalidArguments {
                    name: self.name().to_string(),
                    reason: "Missing 'path' argument for 'remove' action".to_string(),
                })?;

                let output = self
                    .git_command()
                    .args(["worktree", "remove", "--force", &path])
                    .output()
                    .await
                    .map_err(|e| ToolError::Io {
                        name: self.name().to_string(),
                        message: format!("Failed to invoke 'git worktree remove': {e}"),
                    })?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(ToolError::ExecutionFailed {
                        name: self.name().to_string(),
                        message: format!("Failed to remove worktree: {stderr}"),
                    });
                }

                Ok(format!("Successfully removed worktree at '{path}'"))
            }
        }
    }
}
