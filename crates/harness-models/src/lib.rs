pub mod downloader;
pub mod error;
pub mod huggingface;
pub mod importer;
pub mod ollama_bridge;
pub mod registry;

pub use downloader::ModelDownloader;
pub use error::ModelError;
pub use huggingface::{
    evaluate_memory_fit, parse_quantization, HfClient, HfGgufFile, HfModelDetails, HfModelSummary,
    MemoryFit,
};
pub use importer::import_local_gguf;
pub use ollama_bridge::{OllamaBridge, OllamaModelDetails, OllamaModelTag};
pub use registry::{ModelMetadata, ModelRegistry, ModelSource};
