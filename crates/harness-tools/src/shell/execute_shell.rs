use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tracing::{debug, instrument, warn};

use harness_core::{Tool, ToolError};

/// Input parameters for executing a native shell command.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExecuteShellArgs {
    /// The shell command string to execute (e.g. "ls -la", "cargo test", "git status").
    pub command: String,
    /// Optional working directory to execute the command from. Defaults to the active workspace root.
    pub working_directory: Option<String>,
    /// Execution timeout in seconds. Defaults to 60 seconds.
    pub timeout_seconds: Option<u64>,
}

#[derive(Default, Clone)]
pub struct ExecuteShellTool {
    workspace_root: Option<PathBuf>,
}

impl ExecuteShellTool {
    pub const DEFAULT_TIMEOUT_SECS: u64 = 60;

    pub fn new() -> Self {
        Self { workspace_root: None }
    }

    pub fn with_workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    fn resolve_cwd(&self, raw: Option<&str>) -> Option<PathBuf> {
        match raw {
            Some(s) if !s.trim().is_empty() => {
                let p = Path::new(s);
                if p.is_absolute() {
                    Some(p.to_path_buf())
                } else if let Some(ref ws) = self.workspace_root {
                    Some(ws.join(p))
                } else {
                    Some(p.to_path_buf())
                }
            }
            _ => self.workspace_root.clone(),
        }
    }
}

#[async_trait]
impl Tool for ExecuteShellTool {
    fn name(&self) -> &str {
        "execute_shell"
    }

    fn description(&self) -> &str {
        "Executes a native shell command on the host system. Returns stdout, stderr, and the exit status. Use this tool to run build commands, inspect system status, list directories, or run tests."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(ExecuteShellArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Command string to run" },
                "working_directory": { "type": "string", "description": "Working directory" },
                "timeout_seconds": { "type": "integer", "description": "Timeout in seconds" }
            },
            "required": ["command"]
        }))
    }

    #[instrument(skip(self, arguments))]
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: ExecuteShellArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        let timeout_secs = args.timeout_seconds.unwrap_or(Self::DEFAULT_TIMEOUT_SECS);

        debug!(command = %args.command, timeout_secs = timeout_secs, "Spawning shell command");

        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("cmd.exe");
            c.arg("/C").arg(&args.command);
            c
        };

        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = Command::new("sh");
            c.arg("-c").arg(&args.command);
            c
        };

        if let Some(cwd) = self.resolve_cwd(args.working_directory.as_deref()) {
            cmd.current_dir(cwd);
        }
        harness_core::apply_enriched_path(&mut cmd);

        // Configure stdio pipes
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let execution = cmd.output();

        let output = match tokio::time::timeout(Duration::from_secs(timeout_secs), execution).await {
            Ok(result) => result.map_err(|e| ToolError::Io {
                name: self.name().to_string(),
                message: format!("Failed to spawn command '{}': {e}", args.command),
            })?,
            Err(_) => {
                warn!(command = %args.command, "Shell command timed out");
                return Err(ToolError::Timeout {
                    name: self.name().to_string(),
                    timeout_secs,
                });
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        let mut formatted = format!("Exit Code: {exit_code}\n");
        if !stdout.is_empty() {
            formatted.push_str("--- STDOUT ---\n");
            formatted.push_str(&stdout);
            if !stdout.ends_with('\n') {
                formatted.push('\n');
            }
        }
        if !stderr.is_empty() {
            formatted.push_str("--- STDERR ---\n");
            formatted.push_str(&stderr);
            if !stderr.ends_with('\n') {
                formatted.push('\n');
            }
        }

        if stdout.is_empty() && stderr.is_empty() {
            formatted.push_str("[Command produced no stdout or stderr]\n");
        }

        Ok(formatted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_execute_shell_echo() {
        let tool = ExecuteShellTool::new();
        let args = serde_json::json!({
            "command": "echo 'dex-test'"
        });
        let result = tool.execute(args).await.expect("Tool execution should succeed");
        assert!(result.contains("dex-test"));
        assert!(result.contains("Exit Code: 0"));
    }
}
