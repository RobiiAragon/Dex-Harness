use thiserror::Error;

#[derive(Error, Debug)]
pub enum AudioError {
    #[error("I/O error at '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Audio device error: {message}")]
    Device { message: String },

    #[error("Recording error: {message}")]
    Recording { message: String },

    #[error("WAV encoding/decoding error at '{path}': {message}")]
    Wav { path: String, message: String },

    #[error("HTTP request error to '{url}': {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("Subprocess '{command}' failed: {message}")]
    Subprocess { command: String, message: String },

    #[error("No STT backend available: {reason}")]
    SttUnavailable { reason: String },

    #[error("TTS synthesis failed: {reason}")]
    TtsFailed { reason: String },

    #[error("Audio generation failed ({kind}): {reason}")]
    GenerationFailed { kind: String, reason: String },

    #[error("Invalid audio input: {reason}")]
    InvalidInput { reason: String },

    #[error("Audio operation '{operation}' timed out after {timeout_secs}s")]
    Timeout { operation: String, timeout_secs: u64 },
}
