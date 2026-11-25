use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use harness_core::{DexEvent, EventBus};
use harness_extensions::{
    ExtensionCategory, ExtensionKind, ExtensionManifest, ExtensionSupervisor,
    DEFAULT_IDLE_TIMEOUT_SECS,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::error::AudioError;
use crate::tts::probe_wav_duration;

pub const DEFAULT_MUSIC_DIR: &str = ".dex/media/audio/music";
pub const DEFAULT_SFX_DIR: &str = ".dex/media/audio/sfx";
/// Upper bound for one generation request (first run includes model download + load).
const AUDIO_GEN_TIMEOUT_SECS: u64 = 900;
/// How long to wait for the managed sidecar to answer `/health` after spawning it.
const AUDIOGEN_STARTUP_TIMEOUT_SECS: u64 = 60;
const AUDIOGEN_RAM_MB: u64 = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedAudio {
    pub kind: String,
    pub path: PathBuf,
    pub prompt: String,
    pub duration_secs: Option<f64>,
    pub backend: String,
}

/// Explicit `DEX_AUDIO_GEN_URL` override (external music/SFX server), if set.
fn audio_gen_env_url() -> Option<String> {
    std::env::var("DEX_AUDIO_GEN_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
}

fn audio_gen_base_url() -> String {
    audio_gen_env_url().unwrap_or_else(harness_core::audiogen_default_base_url)
}

fn backend_not_installed(kind: &str, tool: &str, base_url: &str, detail: &str) -> AudioError {
    AudioError::BackendNotInstalled {
        kind: kind.to_string(),
        backend: harness_core::DEX_AUDIOGEN_SIDECAR_ID.to_string(),
        install_hint: format!(
            "No music/SFX sidecar reachable at '{base_url}' ({detail}). Install the 'Music & SFX Gen (MusicGen)' runtime with 1 click (`dex models install-runtime audiogen`, or Models Hub -> Runtimes), or point `DEX_AUDIO_GEN_URL` at a compatible server. Do NOT retry `{tool}`. [dex-install:audiogen]"
        ),
    }
}

/// Builds the `LocalSidecar` manifest for Dex's managed MusicGen music/SFX server.
pub fn audiogen_sidecar_manifest(python: &Path, script: &Path) -> ExtensionManifest {
    let port = harness_core::DEX_AUDIOGEN_PORT;
    ExtensionManifest {
        id: harness_core::DEX_AUDIOGEN_SIDECAR_ID.to_string(),
        name: "Dex Music & SFX Gen (MusicGen)".to_string(),
        category: ExtensionCategory::Audio,
        description: "Local music (Meta MusicGen) and sound-effect generation (MusicGen; AudioLDM opt-in via DEX_SFX_BACKEND) on Apple Silicon MPS.".to_string(),
        kind: ExtensionKind::LocalSidecar {
            command: python.to_string_lossy().to_string(),
            args: harness_core::audiogen_launch_args(script, port),
            port,
            healthcheck_url: format!("http://127.0.0.1:{port}/health"),
            env: HashMap::from([
                ("PYTHONUNBUFFERED".to_string(), "1".to_string()),
                ("PATH".to_string(), harness_core::enriched_path_env()),
            ]),
        },
        estimated_ram_mb: AUDIOGEN_RAM_MB,
        idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
        install_hint: "Install the `audiogen` runtime (torch + transformers MusicGen) into ~/.dex/runtimes/mlx-venv.".to_string(),
        enabled: true,
        tags: vec!["audio".to_string(), "music".to_string(), "sfx".to_string(), "musicgen".to_string()],
    }
}

/// Ensures a music/SFX sidecar is reachable, auto-launching Dex's managed MusicGen server
/// through `ExtensionSupervisor` when the runtime is installed. Returns the base URL to use.
async fn ensure_audio_sidecar(
    kind: &str,
    tool: &str,
    supervisor: Option<&Arc<ExtensionSupervisor>>,
) -> Result<String, AudioError> {
    let base_url = audio_gen_base_url();
    if is_audio_sidecar_reachable(&base_url).await {
        if let Some(sup) = supervisor {
            sup.touch_last_used(harness_core::DEX_AUDIOGEN_SIDECAR_ID).await;
        }
        return Ok(base_url);
    }

    if audio_gen_env_url().is_some() {
        return Err(backend_not_installed(kind, tool, &base_url, "DEX_AUDIO_GEN_URL is set but not answering"));
    }

    let Some(python) = harness_core::detect_audiogen_python() else {
        return Err(backend_not_installed(kind, tool, &base_url, "MusicGen runtime not installed"));
    };
    let Some(sup) = supervisor else {
        return Err(backend_not_installed(kind, tool, &base_url, "runtime installed but no extension supervisor to launch it"));
    };

    let script = harness_core::ensure_audiogen_server_script()
        .await
        .map_err(|e| AudioError::Io {
            path: harness_core::audiogen_server_script_path().to_string_lossy().to_string(),
            source: e,
        })?;

    sup.add_custom_extension(audiogen_sidecar_manifest(&python, &script))
        .await
        .map_err(|e| AudioError::GenerationFailed {
            kind: kind.to_string(),
            reason: format!("Could not register the dex-audiogen sidecar: {e}"),
        })?;

    info!(python = ?python, script = ?script, "Launching managed dex-audiogen sidecar");
    sup.start_extension_wait_ready(
        harness_core::DEX_AUDIOGEN_SIDECAR_ID,
        Duration::from_secs(AUDIOGEN_STARTUP_TIMEOUT_SECS),
    )
    .await
    .map_err(|e| AudioError::GenerationFailed {
        kind: kind.to_string(),
        reason: format!("dex-audiogen sidecar failed to start: {e}"),
    })?;
    sup.touch_last_used(harness_core::DEX_AUDIOGEN_SIDECAR_ID).await;
    Ok(base_url)
}

async fn is_audio_sidecar_reachable(base_url: &str) -> bool {
    let Ok(probe) = reqwest::Client::builder()
        .timeout(Duration::from_millis(800))
        .build()
    else {
        return false;
    };
    let health = format!("{}/health", base_url.trim_end_matches('/'));
    matches!(probe.get(&health).send().await, Ok(r) if r.status().is_success())
}

fn resolve_destination(
    output_path: Option<&Path>,
    default_dir: &str,
    stem: &str,
) -> PathBuf {
    if let Some(p) = output_path {
        return harness_core::resolve_safe_path(p);
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    harness_core::resolve_dex_subpath(default_dir).join(format!("{stem}_{ts}.wav"))
}

async fn ensure_parent_dir(dest: &Path) -> Result<(), AudioError> {
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AudioError::Io {
                    path: parent.to_string_lossy().to_string(),
                    source: e,
                })?;
        }
    }
    Ok(())
}

/// Generates music through a local HTTP sidecar (Dex-managed MusicGen on :8189, or `DEX_AUDIO_GEN_URL`),
/// auto-launching the managed sidecar through `ExtensionSupervisor` when needed.
/// `output_path` (absolute or relative) overrides the default `.dex/media/audio/music/` destination.
pub async fn generate_music(
    prompt: &str,
    lyrics: Option<&str>,
    duration_secs: f64,
    output_path: Option<&Path>,
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<&EventBus>,
) -> Result<GeneratedAudio, AudioError> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return Err(AudioError::InvalidInput {
            reason: "Music generation prompt cannot be empty".to_string(),
        });
    }
    let duration = sanitize_duration(duration_secs, 2.0, 120.0, 15.0);

    let base_url = ensure_audio_sidecar("music", "audio_generate_music", supervisor.as_ref()).await?;
    let dest = resolve_destination(output_path, DEFAULT_MUSIC_DIR, "music");
    ensure_parent_dir(&dest).await?;

    let endpoint = format!("{base_url}/v1/audio/music");
    let payload = serde_json::json!({
        "prompt": trimmed,
        "lyrics": lyrics,
        "duration_secs": duration,
    });

    dispatch_audio_generation("music", &endpoint, payload, dest, trimmed, Some(duration), event_bus).await
}

/// Generates a sound effect (SFX) through the same local sidecar (MusicGen by default, AudioLDM opt-in).
pub async fn generate_sfx(
    prompt: &str,
    duration_secs: f64,
    output_path: Option<&Path>,
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<&EventBus>,
) -> Result<GeneratedAudio, AudioError> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return Err(AudioError::InvalidInput {
            reason: "SFX description prompt cannot be empty".to_string(),
        });
    }
    let duration = sanitize_duration(duration_secs, 0.5, 30.0, 4.0);

    let base_url = ensure_audio_sidecar("sfx", "audio_generate_sfx", supervisor.as_ref()).await?;
    let dest = resolve_destination(output_path, DEFAULT_SFX_DIR, "sfx");
    ensure_parent_dir(&dest).await?;

    let endpoint = format!("{base_url}/v1/audio/sfx");
    let payload = serde_json::json!({
        "prompt": trimmed,
        "duration_secs": duration,
    });

    dispatch_audio_generation("sfx", &endpoint, payload, dest, trimmed, Some(duration), event_bus).await
}

fn sanitize_duration(raw: f64, min: f64, max: f64, default: f64) -> f64 {
    if raw.is_finite() {
        raw.clamp(min, max)
    } else {
        default
    }
}

async fn dispatch_audio_generation(
    kind: &str,
    endpoint: &str,
    payload: serde_json::Value,
    dest: PathBuf,
    prompt: &str,
    requested_duration: Option<f64>,
    event_bus: Option<&EventBus>,
) -> Result<GeneratedAudio, AudioError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(AUDIO_GEN_TIMEOUT_SECS))
        .build()
        .map_err(|e| AudioError::Http {
            url: endpoint.to_string(),
            source: e,
        })?;

    debug!(kind = %kind, endpoint = %endpoint, "Requesting audio generation from local sidecar");

    let resp = tokio::time::timeout(
        Duration::from_secs(AUDIO_GEN_TIMEOUT_SECS),
        client.post(endpoint).json(&payload).send(),
    )
    .await
    .map_err(|_| AudioError::Timeout {
        operation: format!("audio_{kind} generation"),
        timeout_secs: AUDIO_GEN_TIMEOUT_SECS,
    })?
    .map_err(|e| AudioError::GenerationFailed {
        kind: kind.to_string(),
        reason: format!(
            "Audio sidecar unreachable at '{endpoint}': {e}. Install the `audiogen` runtime or set `DEX_AUDIO_GEN_URL`."
        ),
    })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AudioError::GenerationFailed {
            kind: kind.to_string(),
            reason: format!("Sidecar returned HTTP {status}: {body}"),
        });
    }

    let backend = resp
        .headers()
        .get("x-dex-backend")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .unwrap_or_else(|| endpoint.to_string());

    let bytes = resp.bytes().await.map_err(|e| AudioError::Http {
        url: endpoint.to_string(),
        source: e,
    })?;

    if bytes.len() < 44 || &bytes[..4] != b"RIFF" {
        return Err(AudioError::GenerationFailed {
            kind: kind.to_string(),
            reason: format!(
                "Sidecar at '{endpoint}' returned {} bytes that are not a WAV file",
                bytes.len()
            ),
        });
    }

    tokio::fs::write(&dest, &bytes)
        .await
        .map_err(|e| AudioError::Io {
            path: dest.to_string_lossy().to_string(),
            source: e,
        })?;

    let measured_duration = probe_wav_duration(&dest).or(requested_duration);
    info!(kind = %kind, path = ?dest, backend = %backend, "Saved generated audio asset");

    if let Some(bus) = event_bus {
        bus.publish(DexEvent::MediaGenerated {
            kind: kind.to_string(),
            path: dest.to_string_lossy().to_string(),
            prompt: prompt.to_string(),
            duration_secs: measured_duration,
        });
    }

    Ok(GeneratedAudio {
        kind: kind.to_string(),
        path: dest,
        prompt: prompt.to_string(),
        duration_secs: measured_duration,
        backend,
    })
}
