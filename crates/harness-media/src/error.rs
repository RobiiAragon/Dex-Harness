use thiserror::Error;

#[derive(Error, Debug)]
pub enum MediaError {
    #[error("I/O error at '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Image processing error at '{path}': {message}")]
    Image { path: String, message: String },

    #[error("JSON error ({context}): {source}")]
    Json {
        context: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("HTTP request error to '{url}': {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("ComfyUI error: {message}")]
    ComfyUi { message: String },

    #[error("Subprocess '{command}' failed: {message}")]
    Subprocess { command: String, message: String },

    #[error("No visual generation backend available ({kind}): {reason}")]
    BackendUnavailable { kind: String, reason: String },

    #[error("Invalid media parameters: {reason}")]
    InvalidInput { reason: String },

    #[error("Media operation '{operation}' timed out after {timeout_secs}s")]
    Timeout { operation: String, timeout_secs: u64 },
}
