use thiserror::Error;

#[derive(Error, Debug)]
pub enum ModelError {
    #[error("I/O error at '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("HTTP request error to '{url}': {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("Failed to parse JSON response from '{url}': {source}")]
    Json {
        url: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("Model '{id}' was not found in local registry")]
    NotFound { id: String },

    #[error("Ollama API error at endpoint '{endpoint}' ({status}): {message}")]
    Ollama {
        endpoint: String,
        status: u16,
        message: String,
    },

    #[error("Download was cancelled for model '{model_id}': {reason}")]
    DownloadCancelled {
        model_id: String,
        reason: String,
    },

    #[error("Invalid model source or file path: {reason}")]
    InvalidInput { reason: String },

    #[error("Hugging Face API error from '{url}': {message}")]
    HuggingFace {
        url: String,
        message: String,
    },

    #[error("Local inference runtime '{runtime}' is not installed. {install_hint}")]
    RuntimeMissing {
        runtime: String,
        install_hint: String,
    },

    #[error("Local inference runtime '{runtime}' failed: {message}")]
    RuntimeFailed {
        runtime: String,
        message: String,
    },
}
