use thiserror::Error;

/// Strongly typed domain errors for 3D mesh inspection, generation, Blender, and Unity operations.
#[derive(Error, Debug)]
pub enum ThreeDError {
    #[error("I/O error at '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("3D geometry error at '{path}': {message}")]
    Gltf { path: String, message: String },

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

    #[error("Subprocess '{command}' failed: {message}")]
    Subprocess { command: String, message: String },

    #[error("3D backend unavailable ({kind}): {reason}")]
    BackendUnavailable { kind: String, reason: String },

    #[error("Invalid 3D operation parameters: {reason}")]
    InvalidInput { reason: String },

    #[error("Security check blocked path operation: {reason}")]
    SecurityViolation { reason: String },

    #[error("3D operation '{operation}' timed out after {timeout_secs}s")]
    Timeout { operation: String, timeout_secs: u64 },
}
