use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::info;

use crate::config::ModelDirsConfig;
use crate::error::ModelError;
use crate::huggingface::parse_quantization;
use crate::ollama_bridge::OllamaBridge;
use crate::registry::{ModelMetadata, ModelRegistry, ModelSource};
use crate::scanner::{inspect_model_directory, ModelFormat};

/// Imports either a local `.gguf` file or a local MLX/Safetensors model directory.
/// Directories (MLX/PT) are registered **by reference** without duplicating gigabytes of weights.
pub async fn import_local_model(
    source_path: impl AsRef<Path>,
    custom_name: Option<&str>,
    copy_gguf: bool,
    register_in_ollama: bool,
    ollama_bridge: Option<&OllamaBridge>,
) -> Result<ModelMetadata, ModelError> {
    let raw = source_path.as_ref().to_string_lossy().to_string();
    let expanded = ModelDirsConfig::expand_tilde(&raw);

    if !tokio::fs::try_exists(&expanded).await.unwrap_or(false) {
        return Err(ModelError::InvalidInput {
            reason: format!("Model path does not exist: '{}'", expanded.display()),
        });
    }

    let meta = tokio::fs::metadata(&expanded)
        .await
        .map_err(|e| ModelError::Io {
            path: expanded.to_string_lossy().to_string(),
            source: e,
        })?;

    if meta.is_dir() {
        return import_local_mlx_or_pt_dir(&expanded, custom_name).await;
    }

    import_local_gguf(
        &expanded,
        custom_name,
        copy_gguf,
        register_in_ollama,
        ollama_bridge,
    )
    .await
}

async fn import_local_mlx_or_pt_dir(
    dir_path: &Path,
    custom_name: Option<&str>,
) -> Result<ModelMetadata, ModelError> {
    let canonical = tokio::fs::canonicalize(dir_path)
        .await
        .unwrap_or_else(|_| dir_path.to_path_buf());
    let parent = canonical
        .parent()
        .unwrap_or(canonical.as_path())
        .to_path_buf();
    let owned_dir = canonical.clone();

    let discovered = tokio::task::spawn_blocking(move || {
        inspect_model_directory(&parent, &owned_dir, &parent.to_string_lossy())
    })
    .await
    .map_err(|e| ModelError::Io {
        path: canonical.to_string_lossy().to_string(),
        source: std::io::Error::other(format!("Failed to inspect model folder: {e}")),
    })?
    .ok_or_else(|| ModelError::InvalidInput {
        reason: format!(
            "Directory '{}' does not look like a valid MLX/Safetensors model (requires `config.json` + `.safetensors`/`.bin` weights).",
            canonical.display()
        ),
    })?;

    let model_name = custom_name
        .filter(|s| !s.trim().is_empty())
        .map(String::from)
        .unwrap_or(discovered.name);
    let fmt_str = match discovered.format {
        ModelFormat::Mlx => "mlx",
        _ => "pt",
    };
    let model_id = format!("{fmt_str}:{}", sanitize_id(&model_name));
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let metadata = ModelMetadata {
        id: model_id,
        name: model_name,
        source: ModelSource::Local,
        file_path: Some(canonical.to_string_lossy().to_string()),
        repo_id: discovered.repo_hint,
        filename: canonical
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from),
        quantization: discovered.quantization,
        format: Some(fmt_str.to_string()),
        size_bytes: discovered.size_bytes,
        created_at: now_secs,
    };

    let mut registry = ModelRegistry::load_or_default().await;
    registry.add(metadata.clone()).await?;
    info!(id = %metadata.id, path = ?canonical, "Registered local MLX/PT model directory by reference");
    Ok(metadata)
}

pub async fn import_local_gguf(
    source_path: impl AsRef<Path>,
    custom_name: Option<&str>,
    copy_file: bool,
    register_in_ollama: bool,
    ollama_bridge: Option<&OllamaBridge>,
) -> Result<ModelMetadata, ModelError> {
    let src = source_path.as_ref();
    if !tokio::fs::try_exists(src).await.unwrap_or(false) {
        return Err(ModelError::InvalidInput {
            reason: format!("Source GGUF file does not exist at '{:?}'", src),
        });
    }

    let file_name = src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| ModelError::InvalidInput {
            reason: "Invalid file name for GGUF model".to_string(),
        })?
        .to_string();

    if !file_name.to_lowercase().ends_with(".gguf") {
        return Err(ModelError::InvalidInput {
            reason: format!("File '{}' is not a .gguf format model file", file_name),
        });
    }

    let meta = tokio::fs::metadata(src).await.map_err(|e| ModelError::Io {
        path: src.to_string_lossy().to_string(),
        source: e,
    })?;

    let size_bytes = meta.len();
    let quantization = parse_quantization(&file_name);

    let models_dir = ModelRegistry::default_dir();
    tokio::fs::create_dir_all(&models_dir)
        .await
        .map_err(|e| ModelError::Io {
            path: models_dir.to_string_lossy().to_string(),
            source: e,
        })?;

    let final_path = if copy_file {
        let dest = models_dir.join(&file_name);
        if dest != src {
            info!(from = ?src, to = ?dest, "Copying GGUF model to ~/.dex/models/");
            tokio::fs::copy(src, &dest)
                .await
                .map_err(|e| ModelError::Io {
                    path: dest.to_string_lossy().to_string(),
                    source: e,
                })?;
        }
        dest
    } else if src.is_absolute() {
        src.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(src))
            .unwrap_or_else(|_| src.to_path_buf())
    };

    let base_name = file_name
        .trim_end_matches(".gguf")
        .trim_end_matches(".GGUF");

    let model_name = custom_name.unwrap_or(base_name).to_string();
    let model_id = format!("local:{}", sanitize_id(&model_name));

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let metadata = ModelMetadata {
        id: model_id.clone(),
        name: model_name.clone(),
        source: ModelSource::Local,
        file_path: Some(final_path.to_string_lossy().to_string()),
        repo_id: None,
        filename: Some(file_name),
        quantization: Some(quantization),
        format: Some("gguf".to_string()),
        size_bytes,
        created_at: now_secs,
    };

    let mut registry = ModelRegistry::load_or_default().await;
    registry.add(metadata.clone()).await?;

    if register_in_ollama {
        if let Some(bridge) = ollama_bridge {
            let ollama_tag = sanitize_id(&model_name);
            info!(tag = %ollama_tag, "Registering model in local Ollama instance");
            bridge.create_from_gguf(&ollama_tag, &final_path).await?;
        }
    }

    Ok(metadata)
}

fn sanitize_id(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}
