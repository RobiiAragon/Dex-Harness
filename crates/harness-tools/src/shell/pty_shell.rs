use async_trait::async_trait;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use schemars::JsonSchema;
use serde::Deserialize;
use std::io::Read;
use std::time::Duration;
use tracing::instrument;

use harness_core::{Tool, ToolError};

/// Input parameters for running a command inside a pseudo-terminal (PTY).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PtyShellArgs {
    /// Command string or program to execute inside the PTY (e.g., "top", "cargo build", "htop").
    pub command: String,
    /// Terminal rows. Defaults to 24.
    pub rows: Option<u16>,
    /// Terminal columns. Defaults to 80.
    pub cols: Option<u16>,
    /// Execution timeout in seconds. Defaults to 30.
    pub timeout_seconds: Option<u64>,
}

#[derive(Default, Clone)]
pub struct PtyShellTool;

impl PtyShellTool {
    pub const DEFAULT_TIMEOUT_SECS: u64 = 30;

    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for PtyShellTool {
    fn name(&self) -> &str {
        "pty_shell"
    }

    fn description(&self) -> &str {
        "Executes a command inside a native pseudo-terminal (PTY) with ANSI emulation and virtual terminal sizing. Ideal for tools that require TTY or interactive terminal features."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(PtyShellArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Command to run in PTY" },
                "rows": { "type": "integer", "description": "Terminal rows" },
                "cols": { "type": "integer", "description": "Terminal columns" },
                "timeout_seconds": { "type": "integer", "description": "Timeout in seconds" }
            },
            "required": ["command"]
        }))
    }

    #[instrument(skip(self, arguments))]
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: PtyShellArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        let timeout_secs = args.timeout_seconds.unwrap_or(Self::DEFAULT_TIMEOUT_SECS);
        let rows = args.rows.unwrap_or(24);
        let cols = args.cols.unwrap_or(80);
        let cmd_str = args.command.clone();

        let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let pty_system = native_pty_system();
            let pair = pty_system
                .openpty(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| format!("Failed to open PTY: {e}"))?;

            #[cfg(windows)]
            let mut cmd = {
                let mut c = CommandBuilder::new("cmd.exe");
                c.args(["/C", &cmd_str]);
                c
            };

            #[cfg(not(windows))]
            let cmd = {
                let mut c = CommandBuilder::new("sh");
                c.args(["-c", &cmd_str]);
                c
            };

            let mut child = pair
                .slave
                .spawn_command(cmd)
                .map_err(|e| format!("Failed to spawn command in PTY: {e}"))?;

            // Drop slave to allow EOF on master reader when child exits
            drop(pair.slave);

            let mut reader = pair
                .master
                .try_clone_reader()
                .map_err(|e| format!("Failed to clone PTY master reader: {e}"))?;

            let mut output = String::new();
            let mut buffer = [0u8; 1024];

            let start_time = std::time::Instant::now();
            let max_duration = Duration::from_secs(timeout_secs);

            loop {
                if start_time.elapsed() > max_duration {
                    let _ = child.kill();
                    return Err(format!("PTY execution timed out after {timeout_secs}s"));
                }

                match reader.read(&mut buffer) {
                    Ok(0) => break, // EOF reached
                    Ok(n) => {
                        let text = String::from_utf8_lossy(&buffer[..n]);
                        output.push_str(&text);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => break, // Process exited and closed descriptor
                }

                // Check if child has exited
                if let Ok(Some(_status)) = child.try_wait() {
                    // Read any trailing bytes
                    while let Ok(n) = reader.read(&mut buffer) {
                        if n == 0 {
                            break;
                        }
                        output.push_str(&String::from_utf8_lossy(&buffer[..n]));
                    }
                    break;
                }
            }

            Ok(output)
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            name: "pty_shell".to_string(),
            message: format!("PTY task panicked: {e}"),
        })?
        .map_err(|e| ToolError::ExecutionFailed {
            name: "pty_shell".to_string(),
            message: e,
        })?;

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pty_shell_simple() {
        let tool = PtyShellTool::new();
        let args = serde_json::json!({
            "command": "echo 'pty-hello'"
        });
        let result = tool.execute(args).await.expect("PTY execution should succeed");
        assert!(result.contains("pty-hello"));
    }
}
