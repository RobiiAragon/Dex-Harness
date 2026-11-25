//! Dex's own chat runtime: picks a local MLX/GGUF model that fits this machine, launches it
//! through the extension supervisor (no LM Studio, Ollama or any other app needed), and
//! transparently reloads it when the memory manager unloaded it to make room for another model.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use harness_core::{
    CompletionRequest, CompletionResponse, DexEvent, EventBus, LlmProvider, ProviderError,
};
use harness_extensions::{
    ExtensionCategory, ExtensionKind, ExtensionManifest, ExtensionStatus, ExtensionSupervisor,
    DEFAULT_IDLE_TIMEOUT_SECS,
};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::config::ModelDirsConfig;
use crate::error::ModelError;
use crate::local_servers::is_chat_model_id;
use crate::runtime::{
    build_launch_spec, detect_runtime_binary, ActiveLoadedModel, RuntimeKind,
    DEX_RUNTIME_SIDECAR_ID, LOCAL_MODEL_STARTUP_TIMEOUT_SECS,
};
use crate::scanner::{
    scan_model_dirs, verify_model_path_integrity, DiscoveredModel, IntegrityStatus, ModelFormat,
};

/// Largest share of total RAM a default chat model may take, leaving room for image, music
/// and 3D models (the memory manager unloads the chat model when they need more).
pub const CHAT_MODEL_MAX_RAM_FRACTION: f64 = 0.45;

/// Runtime RAM estimate for a model of `size_bytes` on disk (weights + KV cache headroom).
pub fn estimated_runtime_ram_mb(size_bytes: u64) -> u64 {
    if size_bytes == 0 {
        return 8192;
    }
    ((size_bytes as f64 / (1024.0 * 1024.0)) * 1.15).round() as u64
}

/// Whether Dex can serve `model` as a chat model with its own runtime.
pub fn is_native_chat_candidate(model: &DiscoveredModel) -> bool {
    matches!(model.format, ModelFormat::Mlx | ModelFormat::Gguf)
        && model.path.is_some()
        && model.integrity_status != IntegrityStatus::Incomplete
        && is_chat_model_id(&model.id)
        && is_chat_model_id(&model.name)
        && model.size_bytes >= 512 * 1024 * 1024
}

/// Picks the best default chat model for a machine with `total_ram_bytes`: tool-capable models
/// first, then the largest that fits in [`CHAT_MODEL_MAX_RAM_FRACTION`] of RAM. Falls back to
/// the smallest candidate when nothing fits.
pub fn pick_default_chat_model(
    models: &[DiscoveredModel],
    total_ram_bytes: u64,
) -> Option<&DiscoveredModel> {
    let budget = (total_ram_bytes as f64 * CHAT_MODEL_MAX_RAM_FRACTION) as u64;
    let candidates: Vec<&DiscoveredModel> =
        models.iter().filter(|m| is_native_chat_candidate(m)).collect();
    let has_tools = |m: &DiscoveredModel| m.capabilities.iter().any(|c| c == "Tools");
    let best_fit = candidates
        .iter()
        .filter(|m| total_ram_bytes == 0 || m.size_bytes <= budget)
        .max_by_key(|m| (has_tools(m), m.size_bytes))
        .copied();
    best_fit.or_else(|| candidates.iter().min_by_key(|m| m.size_bytes).copied())
}

/// Finds a local model by id, name, or path (exact match first, then substring).
pub fn find_local_model<'a>(
    models: &'a [DiscoveredModel],
    query: &str,
) -> Option<&'a DiscoveredModel> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return None;
    }
    let candidates = models.iter().filter(|m| is_native_chat_candidate(m));
    let exact = candidates.clone().find(|m| {
        m.id.to_lowercase() == q
            || m.name.to_lowercase() == q
            || m.path.as_deref().map(str::to_lowercase).as_deref() == Some(q.as_str())
    });
    exact.or_else(|| {
        candidates
            .filter(|m| m.id.to_lowercase().contains(&q) || m.name.to_lowercase().contains(&q))
            .min_by_key(|m| m.id.len())
    })
}

/// Scans the configured model folders (`~/.dex/models`, `~/.lmstudio/models`, …) for chat
/// models Dex can run itself, largest first.
pub async fn discover_native_chat_models() -> Vec<DiscoveredModel> {
    let cfg = ModelDirsConfig::load_or_default().await;
    let dirs: Vec<PathBuf> = cfg.resolved_paths();
    match scan_model_dirs(&dirs).await {
        Ok(mut models) => {
            models.retain(is_native_chat_candidate);
            models.sort_by_key(|m| std::cmp::Reverse(m.size_bytes));
            models
        }
        Err(e) => {
            warn!(error = %e, "Local model scan failed");
            Vec::new()
        }
    }
}

/// Total physical RAM in bytes (0 when unknown).
pub async fn total_ram_bytes() -> u64 {
    let snap = tokio::task::spawn_blocking(harness_core::cookbook::HardwareProfile::detect_live_memory)
        .await
        .ok();
    snap.map(|s| (s.total_memory_gb * 1024.0 * 1024.0 * 1024.0) as u64)
        .unwrap_or(0)
}

fn runtime_kind(format: ModelFormat) -> Result<RuntimeKind, ModelError> {
    match format {
        ModelFormat::Mlx => Ok(RuntimeKind::Mlx),
        ModelFormat::Gguf => Ok(RuntimeKind::LlamaCpp),
        other => Err(ModelError::InvalidInput {
            reason: format!("Format '{other}' cannot be served by Dex's runtime (mlx or gguf only)"),
        }),
    }
}

fn manifest_for(model: &DiscoveredModel, spec: &crate::runtime::RuntimeLaunchSpec) -> ExtensionManifest {
    let fmt = model.format.to_string().to_lowercase();
    ExtensionManifest {
        id: DEX_RUNTIME_SIDECAR_ID.to_string(),
        name: format!("Dex Native Runtime ({})", model.name),
        category: ExtensionCategory::Custom,
        description: format!("Dex's local {} chat server for {}", fmt.to_uppercase(), model.name),
        kind: ExtensionKind::LocalSidecar {
            command: spec.command.clone(),
            args: spec.args.clone(),
            port: spec.port,
            healthcheck_url: spec.healthcheck_url.clone(),
            env: HashMap::new(),
        },
        estimated_ram_mb: estimated_runtime_ram_mb(model.size_bytes),
        idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
        install_hint: spec.runtime.install_hint().to_string(),
        enabled: true,
        tags: vec!["dex-local".to_string(), fmt, "llm".to_string()],
    }
}

/// Loads `model` into Dex's own runtime (MLX or llama.cpp), replacing any previous chat model.
/// The memory manager unloads other idle models first when RAM is tight.
pub async fn load_native_chat_model(
    supervisor: &ExtensionSupervisor,
    model: &DiscoveredModel,
    event_bus: Option<&EventBus>,
) -> Result<ActiveLoadedModel, ModelError> {
    let kind = runtime_kind(model.format)?;
    let path = model.path.clone().ok_or_else(|| ModelError::InvalidInput {
        reason: format!("Model '{}' has no local path", model.name),
    })?;

    let bin = tokio::task::spawn_blocking(move || detect_runtime_binary(kind))
        .await
        .map_err(|e| ModelError::RuntimeFailed {
            runtime: kind.as_id().to_string(),
            message: format!("runtime lookup failed: {e}"),
        })?
        .ok_or_else(|| ModelError::RuntimeMissing {
            runtime: kind.as_id().to_string(),
            install_hint: harness_core::install::with_install_marker(
                kind.install_hint(),
                kind.as_id(),
            ),
        })?;

    let fmt = model.format.to_string().to_lowercase();
    let (fmt_check, path_check) = (fmt.clone(), PathBuf::from(&path));
    let (integrity, note) =
        tokio::task::spawn_blocking(move || verify_model_path_integrity(&fmt_check, &path_check))
            .await
            .map_err(|e| ModelError::RuntimeFailed {
                runtime: kind.as_id().to_string(),
                message: format!("integrity check failed: {e}"),
            })?;
    if integrity == IntegrityStatus::Incomplete {
        return Err(ModelError::InvalidInput {
            reason: format!(
                "'{}' has incomplete weights ({}). Re-download it before loading.",
                model.name,
                note.unwrap_or_else(|| "missing shards".to_string())
            ),
        });
    }

    let spec = build_launch_spec(&fmt, &path, Path::new(&bin))?;
    let _ = supervisor.stop_extension(DEX_RUNTIME_SIDECAR_ID).await;
    let manifest = manifest_for(model, &spec);
    supervisor
        .add_custom_extension(manifest)
        .await
        .map_err(|e| ModelError::RuntimeFailed {
            runtime: kind.as_id().to_string(),
            message: e.to_string(),
        })?;

    if let Some(bus) = event_bus {
        let _ = bus.publish(DexEvent::RuntimeProgress {
            runtime: DEX_RUNTIME_SIDECAR_ID.to_string(),
            stage: "loading".to_string(),
            message: format!("Loading {} into memory…", model.name),
            done: false,
            is_error: false,
        });
    }
    info!(model = %model.name, path = %path, "Loading chat model into Dex's native runtime");
    let info = supervisor
        .start_extension_wait_ready(
            DEX_RUNTIME_SIDECAR_ID,
            Duration::from_secs(LOCAL_MODEL_STARTUP_TIMEOUT_SECS),
        )
        .await
        .map_err(|e| ModelError::RuntimeFailed {
            runtime: kind.as_id().to_string(),
            message: e.to_string(),
        })?;

    Ok(ActiveLoadedModel {
        model_id: model.id.clone(),
        name: model.name.clone(),
        format: fmt,
        quantization: model.quantization.clone(),
        path,
        runtime: kind.as_id().to_string(),
        base_url: spec.base_url,
        port: spec.port,
        sidecar_id: DEX_RUNTIME_SIDECAR_ID.to_string(),
        size_bytes: model.size_bytes,
        pid: info.pid,
    })
}

/// Restarts Dex's chat runtime when it is not running (idle unload, or unloaded by the memory
/// manager to make room for an image/music/3D model) and marks it as used.
pub async fn ensure_native_runtime_running(
    supervisor: &ExtensionSupervisor,
    sidecar_id: &str,
) -> Result<(), String> {
    if supervisor.status_of(sidecar_id).await != ExtensionStatus::Running {
        info!(sidecar = sidecar_id, "Chat model was unloaded; reloading it");
        supervisor
            .start_extension_wait_ready(
                sidecar_id,
                Duration::from_secs(LOCAL_MODEL_STARTUP_TIMEOUT_SECS),
            )
            .await
            .map_err(|e| e.to_string())?;
    }
    supervisor.touch_last_used(sidecar_id).await;
    Ok(())
}

/// Wraps a provider that talks to Dex's native runtime so every request first makes sure the
/// model is loaded, reloading it on demand.
pub struct NativeReloadProvider {
    inner: Arc<dyn LlmProvider>,
    supervisor: Arc<ExtensionSupervisor>,
    sidecar_id: String,
    reload_lock: Mutex<()>,
}

impl NativeReloadProvider {
    pub fn new(
        inner: Arc<dyn LlmProvider>,
        supervisor: Arc<ExtensionSupervisor>,
        sidecar_id: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            supervisor,
            sidecar_id: sidecar_id.into(),
            reload_lock: Mutex::new(()),
        }
    }
}

#[async_trait]
impl LlmProvider for NativeReloadProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        {
            let _guard = self.reload_lock.lock().await;
            ensure_native_runtime_running(&self.supervisor, &self.sidecar_id)
                .await
                .map_err(|message| ProviderError::Custom {
                    provider: "dex-native".to_string(),
                    message: format!("could not reload the local chat model: {message}"),
                })?;
        }
        let result = self.inner.complete(request).await;
        self.supervisor.touch_last_used(&self.sidecar_id).await;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, format: ModelFormat, gb: f64, tools: bool) -> DiscoveredModel {
        let raw = serde_json::json!({
            "id": id,
            "name": id,
            "group_key": id,
            "format": format,
            "size_bytes": (gb * 1024.0 * 1024.0 * 1024.0) as u64,
            "path": format!("/models/{id}"),
            "capabilities": if tools { vec!["Tools"] } else { vec![] },
            "modified_at": 0,
        });
        serde_json::from_value(raw).expect("valid model json")
    }

    #[test]
    fn picks_largest_tool_model_that_fits_48gb() {
        let models = vec![
            model("qwen3.8-27b-8bit", ModelFormat::Mlx, 28.0, true),
            model("qwen3.5-35b-a3b-4bit", ModelFormat::Mlx, 19.0, true),
            model("qwen3.8-27b-4bit", ModelFormat::Mlx, 15.0, true),
            model("big-no-tools", ModelFormat::Mlx, 20.0, false),
            model("nomic-embed-text", ModelFormat::Gguf, 0.6, false),
        ];
        let ram = 48 * 1024 * 1024 * 1024;
        let picked = pick_default_chat_model(&models, ram).expect("a model");
        assert_eq!(picked.id, "qwen3.5-35b-a3b-4bit");
    }

    #[test]
    fn falls_back_to_smallest_when_nothing_fits() {
        let models = vec![
            model("huge-a", ModelFormat::Mlx, 40.0, true),
            model("huge-b", ModelFormat::Gguf, 30.0, true),
        ];
        let ram = 16 * 1024 * 1024 * 1024;
        assert_eq!(pick_default_chat_model(&models, ram).map(|m| m.id.as_str()), Some("huge-b"));
    }

    #[test]
    fn finds_models_by_substring_and_skips_embeddings() {
        let models = vec![
            model("lmstudio-community/Qwen3.8-27B-MLX-4bit", ModelFormat::Mlx, 15.0, true),
            model("nomic-embed-text-v1.5", ModelFormat::Gguf, 0.6, false),
        ];
        assert!(find_local_model(&models, "qwen3.8").is_some());
        assert!(find_local_model(&models, "nomic-embed").is_none());
    }
}
