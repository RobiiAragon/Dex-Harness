use thiserror::Error;

#[derive(Error, Debug)]
pub enum DocsError {
    #[error("I/O error at '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Unsupported document format for '{path}': {reason}")]
    UnsupportedFormat { path: String, reason: String },

    #[error("OCR execution failed for '{path}': {reason}")]
    OcrFailed { path: String, reason: String },

    #[error("PDF extraction failed for '{path}': {reason}")]
    PdfExtractFailed { path: String, reason: String },

    #[error("HTTP request to Docling endpoint '{url}' failed: {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("Document operation '{operation}' timed out after {timeout_secs}s")]
    Timeout { operation: String, timeout_secs: u64 },

    #[error("Invalid document input: {reason}")]
    InvalidInput { reason: String },
}
