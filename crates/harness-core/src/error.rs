use thiserror::Error;

#[derive(Error, Debug)]
pub enum CoreError {
    #[error("LLM Provider error: {0}")]
    Provider(#[from] ProviderError),

    #[error("Tool execution error: {0}")]
    Tool(#[from] ToolError),

    #[error("Serialization / JSON error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Maximum ReAct iterations ({max_steps}) reached without achieving goal")]
    MaxIterationsReached { max_steps: usize },

    #[error("Agent halted: {reason}")]
    AgentHalted { reason: String },

    #[error("Internal error: {0}")]
    Internal(String),
}

#[derive(Error, Debug)]
pub enum ProviderError {
    #[error("HTTP request failed: {0}")]
    Http(String),

    #[error("Failed to parse response: {0}")]
    ParsingResponse(String),

    #[error("Requested model '{model}' not found or unavailable: {details}")]
    ModelNotFound { model: String, details: String },

    #[error("Request to LLM timed out")]
    Timeout,

    #[error("Provider '{provider}' error: {message}")]
    Custom {
        provider: String,
        message: String,
    },
}

#[derive(Error, Debug)]
pub enum ToolError {
    #[error("Tool '{name}' failed with error: {message}")]
    ExecutionFailed { name: String, message: String },

    #[error("Invalid arguments for tool '{name}': {reason}")]
    InvalidArguments { name: String, reason: String },

    #[error("Tool '{name}' is not registered in ToolRegistry")]
    NotFound { name: String },

    #[error("Tool '{name}' timed out after {timeout_secs} seconds")]
    Timeout { name: String, timeout_secs: u64 },

    #[error("I/O error in tool '{name}': {message}")]
    Io { name: String, message: String },
}
