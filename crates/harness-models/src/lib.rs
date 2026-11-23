pub mod config;
pub mod downloader;
pub mod error;
pub mod huggingface;
pub mod importer;
pub mod local_servers;
pub mod ollama_bridge;
pub mod registry;
pub mod scanner;

pub use config::ModelDirsConfig;
pub use downloader::ModelDownloader;
pub use error::ModelError;
pub use huggingface::{
    evaluate_memory_fit, parse_quantization, strip_yaml_frontmatter, HfClient, HfGgufFile,
    HfModelDetails, HfModelSummary, MemoryFit,
};
pub use importer::{import_local_gguf, import_local_model};
pub use local_servers::{
    detect_local_servers, LocalServerInfo, LM_STUDIO_DEFAULT_URL, MLX_LM_DEFAULT_PORT,
};
pub use ollama_bridge::{OllamaBridge, OllamaModelDetails, OllamaModelTag};
pub use registry::{ModelMetadata, ModelRegistry, ModelSource};
pub use scanner::{
    clean_display_name, compute_group_key, infer_capabilities, inspect_model_directory,
    scan_model_dirs, DiscoveredModel, ModelFormat,
};
