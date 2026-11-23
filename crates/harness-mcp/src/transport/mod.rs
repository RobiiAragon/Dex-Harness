pub mod stdio;

use async_trait::async_trait;
use crate::error::McpError;

pub use stdio::StdioTransport;

/// Trait defining communication channel with an MCP server (stdio or SSE).
#[async_trait]
pub trait McpTransport: Send + Sync {
    async fn send(&self, payload: &str) -> Result<(), McpError>;
    async fn receive(&self) -> Result<String, McpError>;
}
