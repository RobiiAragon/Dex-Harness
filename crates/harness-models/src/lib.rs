pub mod config;
pub mod downloader;
pub mod error;
pub mod huggingface;
pub mod importer;
pub mod local_servers;
pub mod native;
pub mod ollama_bridge;
pub mod registry;
pub mod runtime;
pub mod scanner;

pub use config::ModelDirsConfig;
pub use downloader::ModelDownloader;
pub use error::ModelError;
pub use huggingface::{
    evaluate_memory_fit, is_auxiliary_model_file, normalize_hf_avatar_url, parse_quantization,
    resolve_hf_image_url, strip_yaml_frontmatter, HfClient, HfGgufFile, HfModelDetails,
    HfModelSummary, MemoryFit,
};
pub use importer::{import_local_gguf, import_local_model};
pub use local_servers::{
    detect_local_servers, is_chat_model_id, pick_chat_endpoint, LocalServerInfo,
    LLAMA_SERVER_DEFAULT_PORT, LM_STUDIO_CANDIDATE_PORTS, LM_STUDIO_DEFAULT_URL,
    MLX_LM_DEFAULT_PORT,
};
pub use native::{
    discover_native_chat_models, ensure_native_runtime_running, estimated_runtime_ram_mb,
    find_local_model, is_native_chat_candidate, load_native_chat_model, pick_default_chat_model,
    total_ram_bytes, NativeReloadProvider, CHAT_MODEL_MAX_RAM_FRACTION,
};
pub use ollama_bridge::{OllamaBridge, OllamaModelDetails, OllamaModelTag};
pub use registry::{ModelMetadata, ModelRegistry, ModelSource};
pub use runtime::{
    build_launch_spec, detect_runtime_binary, detect_runtimes, install_runtime,
    ActiveLoadedModel, LoadModelOutcome, RuntimeKind, RuntimeLaunchSpec, RuntimeStatus, ALL_RUNTIMES,
    DEX_LLAMA_PORT, DEX_MLX_PORT, DEX_RUNTIME_SIDECAR_ID, LOCAL_MODEL_STARTUP_TIMEOUT_SECS,
};
pub use scanner::{
    clean_display_name, compute_group_key, infer_capabilities, inspect_model_directory,
    scan_model_dirs, verify_model_path_integrity, DiscoveredModel, IntegrityStatus, ModelFormat,
};
