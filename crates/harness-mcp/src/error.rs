use thiserror::Error;

#[derive(Error, Debug)]
pub enum McpError {
    #[error("Transport error: {0}")]
    Transport(String),

    #[error("JSON-RPC error code {code}: {message}")]
    JsonRpc {
        code: i64,
        message: String,
        data: Option<serde_json::Value>,
    },

    #[error("Protocol serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("I/O error during MCP transport operation: {0}")]
    Io(#[from] std::io::Error),
}
