use thiserror::Error;

#[derive(Error, Debug)]
pub enum ExtensionError {
    #[error("I/O error at '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("JSON serialization error for '{context}': {source}")]
    Json {
        context: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("Extension '{id}' not found in registry")]
    NotFound { id: String },

    #[error("Catalog recipe '{id}' does not exist")]
    CatalogNotFound { id: String },

    #[error("Failed to spawn process for extension '{id}' (command '{command}'): {source}")]
    ProcessSpawn {
        id: String,
        command: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Healthcheck failed for extension '{id}' at '{url}': {reason}")]
    HealthcheckFailed {
        id: String,
        url: String,
        reason: String,
    },

    #[error("MCP bridge error for extension '{id}': {message}")]
    Mcp { id: String, message: String },

    #[error("Invalid extension configuration for '{id}': {reason}")]
    InvalidConfig { id: String, reason: String },

    #[error("Operation timed out for extension '{id}' after {timeout_secs}s")]
    Timeout { id: String, timeout_secs: u64 },
}
