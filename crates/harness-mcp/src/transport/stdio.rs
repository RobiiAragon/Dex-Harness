use async_trait::async_trait;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tracing::debug;

use crate::error::McpError;
use crate::transport::McpTransport;

pub struct StdioTransport {
    _child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    reader: Mutex<BufReader<ChildStdout>>,
}

impl StdioTransport {
    pub fn spawn(program: &str, args: &[&str]) -> Result<Self, McpError> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(McpError::Io)?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("Failed to capture MCP child stdin".to_string()))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("Failed to capture MCP child stdout".to_string()))?;

        Ok(Self {
            _child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            reader: Mutex::new(BufReader::new(stdout)),
        })
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
            return Err(McpError::Transport("MCP child closed stdout (EOF)".to_string()));
        }
        debug!(bytes = bytes_read, "Received payload from MCP stdio");
        Ok(line.trim_end().to_string())
    }
}
