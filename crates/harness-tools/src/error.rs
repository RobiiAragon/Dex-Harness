use thiserror::Error;

#[derive(Error, Debug)]
pub enum NativeToolError {
    #[error("File system I/O error at '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Process execution timed out after {timeout_secs} seconds")]
    ProcessTimeout { timeout_secs: u64 },

    #[error("Failed to spawn process: {0}")]
    ProcessSpawn(#[source] std::io::Error),

    #[error("Invalid argument: {0}")]
    InvalidArgument(String),
}
