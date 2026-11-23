use std::collections::HashMap;
use std::process::Stdio;
use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tracing::debug;

use crate::error::McpError;
use crate::transport::McpTransport;

pub struct StdioTransport {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    reader: Mutex<BufReader<ChildStdout>>,
}

impl StdioTransport {
    pub fn spawn(program: &str, args: &[&str]) -> Result<Self, McpError> {
        let owned_args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        Self::spawn_with_env(program, &owned_args, &HashMap::new())
    }

    pub fn spawn_with_env(
        program: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self, McpError> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(McpError::Io)?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("Failed to capture MCP child stdin".to_string()))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("Failed to capture MCP child stdout".to_string()))?;

        Ok(Self {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            reader: Mutex::new(BufReader::new(stdout)),
        })
    }

    pub async fn pid(&self) -> Option<u32> {
        let child = self.child.lock().await;
        child.id()
    }

    pub async fn is_running(&self) -> bool {
        let mut child = self.child.lock().await;
        matches!(child.try_wait(), Ok(None))
    }

    pub async fn kill(&self) -> Result<(), McpError> {
        let mut child = self.child.lock().await;
        child.kill().await.map_err(McpError::Io)
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn send(&self, payload: &str) -> Result<(), McpError> {
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(payload.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        debug!(bytes = payload.len(), "Sent payload to MCP stdio");
        Ok(())
    }

    async fn receive(&self) -> Result<String, McpError> {
        let mut reader = self.reader.lock().await;
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            return Err(McpError::Transport(
                "MCP child closed stdout (EOF)".to_string(),
            ));
        }
        debug!(bytes = bytes_read, "Received payload from MCP stdio");
        Ok(line.trim_end().to_string())
    }
}
