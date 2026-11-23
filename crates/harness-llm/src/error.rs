use thiserror::Error;

#[derive(Error, Debug)]
pub enum LlmError {
    #[error("HTTP client error: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("Failed to parse JSON payload: {0}")]
    Json(#[from] serde_json::Error),

    #[error("API returned error status {status}: {body}")]
    ApiError {
        status: reqwest::StatusCode,
        body: String,
    },

    #[error("Malformed response from provider: {0}")]
    MalformedResponse(String),
}
