use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::ModelError;
use crate::huggingface::parse_quantization;

const MAX_SCAN_DEPTH: usize = 6;

/// Recognized local model weight format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFormat {
    Gguf,
    Mlx,
    #[serde(rename = "pt")]
    PyTorch,
    Ollama,
}

impl std::fmt::Display for ModelFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gguf => write!(f, "GGUF"),
            Self::Mlx => write!(f, "MLX"),
            Self::PyTorch => write!(f, "PT"),
            Self::Ollama => write!(f, "Ollama"),
        }
    }
}

/// Rich model descriptor discovered from local folders (LM Studio / Bionic / Dex / custom),
/// Dex's registry, or Ollama.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredModel {
    pub id: String,
    pub name: String,
    pub group_key: String,
    pub format: ModelFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<String>,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub modified_at: u64,
    #[serde(default)]
    pub is_reference: bool,
}

/// Recursively scans all configured model directories inside `tokio::task::spawn_blocking`
/// for `GGUF`, `MLX`, and `PyTorch / Safetensors` models.
pub async fn scan_model_dirs(dirs: &[PathBuf]) -> Result<Vec<DiscoveredModel>, ModelError> {
    let owned_dirs: Vec<PathBuf> = dirs.to_vec();
    tokio::task::spawn_blocking(move || scan_model_dirs_blocking(&owned_dirs))
        .await
        .map_err(|e| ModelError::Io {
            path: "model_scanner".to_string(),
            source: std::io::Error::other(format!("Model scanner worker failed: {e}")),
        })
}

fn scan_model_dirs_blocking(dirs: &[PathBuf]) -> Vec<DiscoveredModel> {
    let mut results = Vec::new();
    let mut visited_dirs: HashSet<PathBuf> = HashSet::new();
    let mut seen_paths: HashSet<String> = HashSet::new();

    for root in dirs {
        if !root.exists() || !root.is_dir() {
            continue;
        }
        let source_root = root.to_string_lossy().to_string();
        scan_dir_recursive(
            root,
            root,
            &source_root,
            0,
            &mut visited_dirs,
            &mut seen_paths,
            &mut results,
        );
    }

    // Sort newest modified first
    results.sort_by(|a, b| {
        b.modified_at
            .cmp(&a.modified_at)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    debug!(count = results.len(), "Completed recursive model directory scan");
    results
}

fn scan_dir_recursive(
    root: &Path,
    dir: &Path,
    source_root: &str,
    depth: usize,
    visited_dirs: &mut HashSet<PathBuf>,
    seen_paths: &mut HashSet<String>,
    out: &mut Vec<DiscoveredModel>,
) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }

    let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    if !visited_dirs.insert(canonical.clone()) {
        return;
    }

    // Check if this directory itself is an MLX or PyTorch/Safetensors model directory
    if let Some(model) = inspect_model_directory(root, dir, source_root) {
        let key = canonical.to_string_lossy().to_string();
        if seen_paths.insert(key) {
            out.push(model);
        }
        // Do not recurse into shard subdirectories once recognized as a model folder
        return;
    }

    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };

    let mut subdirs = Vec::new();
    let mut gguf_files = Vec::new();
    let mut has_mmproj_sibling = false;

    for entry in rd.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        // Skip hidden metadata directories (e.g. .git, .cache)
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            subdirs.push(path);
        } else if path.is_file() {
            let lower = name.to_lowercase();
            if lower.ends_with(".gguf") {
                if lower.contains("mmproj") {
                    has_mmproj_sibling = true;
                } else {
                    gguf_files.push(path);
                }
            }
        }
    }

    for gguf in gguf_files {
        let canonical_file = std::fs::canonicalize(&gguf).unwrap_or_else(|_| gguf.clone());
        let file_key = canonical_file.to_string_lossy().to_string();
        if !seen_paths.insert(file_key.clone()) {
            continue;
        }
        if let Some(model) = inspect_gguf_file(root, &gguf, source_root, has_mmproj_sibling) {
            out.push(model);
        }
    }

    for sub in subdirs {
        scan_dir_recursive(
            root,
            &sub,
            source_root,
            depth + 1,
            visited_dirs,
            seen_paths,
            out,
        );
    }
}

/// Inspects a single directory to determine whether it contains a complete MLX or PyTorch/Safetensors model.
pub fn inspect_model_directory(
    root: &Path,
    dir: &Path,
    source_root: &str,
) -> Option<DiscoveredModel> {
    let config_path = dir.join("config.json");
    if !config_path.is_file() {
        return None;
    }

    let Ok(rd) = std::fs::read_dir(dir) else {
        return None;
    };

    let mut has_safetensors = false;
    let mut has_bin_weights = false;
    let mut has_index_json = false;
    let mut total_size: u64 = 0;
    let mut latest_mtime: u64 = 0;

    for entry in rd.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let meta = entry.metadata().ok();
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if mtime > latest_mtime {
            latest_mtime = mtime;
        }

        if name.ends_with(".safetensors") {
            has_safetensors = true;
            total_size = total_size.saturating_add(size);
        } else if name == "model.safetensors.index.json" {
            has_index_json = true;
            total_size = total_size.saturating_add(size);
        } else if name.ends_with(".bin") && !name.contains("tokenizer") {
            has_bin_weights = true;
            total_size = total_size.saturating_add(size);
        } else if name.ends_with(".json") || name.ends_with(".model") || name.ends_with(".tiktoken")
        {
            total_size = total_size.saturating_add(size);
        }
    }

    if !has_safetensors && !has_index_json && !has_bin_weights {
        return None;
    }

    // Parse config.json for MLX quantization, architecture, context length, and vision support
    let config_json = std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());

    let architecture = config_json
        .as_ref()
        .and_then(|v| v.get("model_type").and_then(|t| t.as_str()))
        .map(String::from);

    let context_length = config_json.as_ref().and_then(|v| {
        v.get("max_position_embeddings")
            .and_then(|n| n.as_u64())
            .or_else(|| {
                v.pointer("/text_config/max_position_embeddings")
                    .and_then(|n| n.as_u64())
            })
    });

    let has_vision_config = config_json
        .as_ref()
        .is_some_and(|v| v.get("vision_config").is_some() || v.get("visual").is_some());

    let config_quant_bits = config_json.as_ref().and_then(|v| {
        v.pointer("/quantization/bits")
            .and_then(|b| b.as_u64())
            .or_else(|| {
                v.pointer("/quantization_config/bits")
                    .and_then(|b| b.as_u64())
            })
    });

    let dir_str = dir.to_string_lossy().to_string();
    let dir_lower = dir_str.to_lowercase();
    let is_mlx = config_quant_bits.is_some()
        || config_json
            .as_ref()
            .is_some_and(|v| v.get("quantization").is_some())
        || dir_lower.contains("mlx");

    let format = if is_mlx && (has_safetensors || has_index_json) {
        ModelFormat::Mlx
    } else {
        ModelFormat::PyTorch
    };

    let raw_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("model")
        .to_string();

    let quantization = config_quant_bits
        .map(|bits| format!("{bits}bit"))
        .or_else(|| {
            let parsed = parse_quantization(&raw_name);
            if parsed != "UNKNOWN" {
                Some(parsed)
            } else {
                None
            }
        });

    let repo_hint = derive_repo_hint(root, dir);
    let clean_name = clean_display_name(&raw_name);
    let group_key = compute_group_key(&raw_name);
    let capabilities = infer_capabilities(&raw_name, architecture.as_deref(), has_vision_config);
    let id = format!(
        "{}:{}",
        match format {
            ModelFormat::Mlx => "mlx",
            _ => "pt",
        },
        sanitize_slug(&repo_hint.clone().unwrap_or_else(|| raw_name.clone()))
    );

    Some(DiscoveredModel {
        id,
        name: clean_name,
        group_key,
        format,
        quantization,
        size_bytes: total_size,
        path: Some(dir_str),
        source_dir: Some(source_root.to_string()),
        repo_hint,
        architecture,
        context_length,
        capabilities,
        modified_at: latest_mtime,
        is_reference: true,
    })
}

fn inspect_gguf_file(
    root: &Path,
    file: &Path,
    source_root: &str,
    has_mmproj_sibling: bool,
) -> Option<DiscoveredModel> {
    let meta = std::fs::metadata(file).ok()?;
    let size_bytes = meta.len();
    let modified_at = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let filename = file.file_name()?.to_string_lossy().to_string();
    let stem = file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&filename)
        .to_string();

    let quant_raw = parse_quantization(&filename);
    let quantization = if quant_raw == "UNKNOWN" {
        None
    } else {
        Some(quant_raw)
    };

    let parent_dir = file.parent().unwrap_or(root);
    let repo_hint = derive_repo_hint(root, parent_dir);
    let clean_name = clean_display_name(&stem);
    let group_key = compute_group_key(&stem);
    let capabilities = infer_capabilities(&stem, None, has_mmproj_sibling);

    let id = format!("gguf:{}", sanitize_slug(&stem));

    Some(DiscoveredModel {
        id,
        name: clean_name,
        group_key,
        format: ModelFormat::Gguf,
        quantization,
        size_bytes,
        path: Some(file.to_string_lossy().to_string()),
        source_dir: Some(source_root.to_string()),
        repo_hint,
        architecture: None,
        context_length: None,
        capabilities,
        modified_at,
        is_reference: true,
    })
}

fn derive_repo_hint(root: &Path, target: &Path) -> Option<String> {
    let rel = target.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(String::from))
        .collect();
    if parts.is_empty() {
        return None;
    }
    if parts.len() >= 2 {
        Some(format!("{}/{}", parts[parts.len() - 2], parts[parts.len() - 1]))
    } else {
        Some(parts[0].clone())
    }
}

/// Removes trailing quantization / format suffixes to produce a clean human display name.
pub fn clean_display_name(raw: &str) -> String {
    let mut name = raw
        .trim_end_matches(".gguf")
        .trim_end_matches(".GGUF")
        .to_string();

    let suffixes = [
        "-mlx-4bit", "-mlx-8bit", "-mlx-6bit", "-mlx-bf16", "-mlx-fp16",
        "-4bit", "-8bit", "-6bit", "-3bit", "-2bit",
        "_4bit", "_8bit", "-GGUF", "-gguf", "-MLX", "-mlx",
    ];
    for suf in suffixes {
        if name.to_lowercase().ends_with(&suf.to_lowercase()) && name.len() > suf.len() + 2 {
            name.truncate(name.len() - suf.len());
        }
    }
    name
}

/// Computes a canonical grouping key so variants of the same model family (e.g. `4bit` & `8bit`)
/// group cleanly together.
pub fn compute_group_key(raw: &str) -> String {
    let mut s = clean_display_name(raw).to_lowercase();
    let quant = parse_quantization(raw);
    if quant != "UNKNOWN" {
        let q_low = quant.to_lowercase();
        for sep in ["-", "_", "."] {
            let pat = format!("{sep}{q_low}");
            if let Some(stripped) = s.strip_suffix(&pat) {
                s = stripped.to_string();
                break;
            }
        }
    }
    sanitize_slug(&s)
}

/// Infers model capabilities (`Vision`, `Tools`, `Reasoning`) from name, architecture, and config flags.
pub fn infer_capabilities(
    name_or_id: &str,
    architecture: Option<&str>,
    has_vision_config: bool,
) -> Vec<String> {
    let lower = format!(
        "{} {}",
        name_or_id.to_lowercase(),
        architecture.unwrap_or("").to_lowercase()
    );
    let mut caps = Vec::new();

    if has_vision_config
        || lower.contains("vl")
        || lower.contains("vision")
        || lower.contains("llava")
        || lower.contains("pixtral")
        || lower.contains("minicpm-v")
        || lower.contains("internvl")
        || lower.contains("qwen2.5-vl")
        || lower.contains("qwen3-vl")
    {
        caps.push("Vision".to_string());
    }

    if lower.contains("qwen")
        || lower.contains("llama-3")
        || lower.contains("llama3")
        || lower.contains("mistral")
        || lower.contains("devstral")
        || lower.contains("hermes")
        || lower.contains("coder")
        || lower.contains("glm-4")
        || lower.contains("gemma-3")
        || lower.contains("deepseek")
        || lower.contains("tool")
    {
        caps.push("Tools".to_string());
    }

    if lower.contains("-r1")
        || lower.contains("qwq")
        || lower.contains("thinking")
        || lower.contains("reason")
        || lower.contains("deepseek-r")
    {
        caps.push("Reasoning".to_string());
    }

    caps
}

fn sanitize_slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '/' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_scanner_detects_mlx_gguf_and_pytorch_folders() {
        let base = std::env::temp_dir().join(format!("dex_scan_test_{}", std::process::id()));

        // 1. Mock MLX model directory (LM Studio / mlx-community layout)
        let mlx_dir = base.join("mlx-community/Qwen3-VL-30B-4bit");
        tokio::fs::create_dir_all(&mlx_dir).await.expect("mlx dir");
        let mlx_cfg = serde_json::json!({
            "model_type": "qwen3_vl",
            "max_position_embeddings": 131072,
            "vision_config": { "depth": 24 },
            "quantization": { "group_size": 64, "bits": 4 }
        });
        tokio::fs::write(
            mlx_dir.join("config.json"),
            serde_json::to_string_pretty(&mlx_cfg).expect("json"),
        )
        .await
        .expect("write mlx config");
        tokio::fs::write(mlx_dir.join("model-00001-of-00002.safetensors"), vec![0u8; 4096])
            .await
            .expect("shard 1");
        tokio::fs::write(mlx_dir.join("model-00002-of-00002.safetensors"), vec![0u8; 2048])
            .await
            .expect("shard 2");

        // 2. Mock GGUF model with Unsloth dynamic quant + sibling mmproj
        let gguf_dir = base.join("unsloth/Qwen3.5-Coder-GGUF");
        tokio::fs::create_dir_all(&gguf_dir).await.expect("gguf dir");
        tokio::fs::write(gguf_dir.join("Qwen3.5-Coder-UD-Q4_K_XL.gguf"), vec![1u8; 8192])
            .await
            .expect("gguf");
        tokio::fs::write(gguf_dir.join("mmproj-BF16.gguf"), vec![2u8; 512])
            .await
            .expect("mmproj");

        // 3. Mock PyTorch/Safetensors (non-MLX) model directory
        let pt_dir = base.join("org/DeepSeek-R1-Distill-8B");
        tokio::fs::create_dir_all(&pt_dir).await.expect("pt dir");
        tokio::fs::write(
            pt_dir.join("config.json"),
            r#"{"model_type":"llama","max_position_embeddings":65536}"#,
        )
        .await
        .expect("pt cfg");
        tokio::fs::write(pt_dir.join("model.safetensors"), vec![3u8; 1024])
            .await
            .expect("pt weights");

        let discovered = scan_model_dirs(std::slice::from_ref(&base))
            .await
            .expect("scan succeeds");
        assert_eq!(discovered.len(), 3, "mmproj should not be listed as separate LLM");

        let mlx = discovered
            .iter()
            .find(|m| m.format == ModelFormat::Mlx)
            .expect("found MLX");
        assert_eq!(mlx.quantization.as_deref(), Some("4bit"));
        assert_eq!(mlx.architecture.as_deref(), Some("qwen3_vl"));
        assert_eq!(mlx.context_length, Some(131072));
        assert!(mlx.size_bytes >= 6144);
        assert!(mlx.capabilities.contains(&"Vision".to_string()));
        assert!(mlx.capabilities.contains(&"Tools".to_string()));
        assert_eq!(
            mlx.repo_hint.as_deref(),
            Some("mlx-community/Qwen3-VL-30B-4bit")
        );

        let gguf = discovered
            .iter()
            .find(|m| m.format == ModelFormat::Gguf)
            .expect("found GGUF");
        assert_eq!(gguf.quantization.as_deref(), Some("UD-Q4_K_XL"));
        assert_eq!(gguf.size_bytes, 8192);
        assert!(gguf.capabilities.contains(&"Vision".to_string()));

        let pt = discovered
            .iter()
            .find(|m| m.format == ModelFormat::PyTorch)
            .expect("found PT");
        assert!(pt.capabilities.contains(&"Reasoning".to_string()));

        let _ = tokio::fs::remove_dir_all(&base).await;
    }
}
