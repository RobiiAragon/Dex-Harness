use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use harness_core::{DexEvent, EventBus};
use harness_extensions::ExtensionSupervisor;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::error::AudioError;
use crate::tts::probe_wav_duration;

pub const DEFAULT_MUSIC_DIR: &str = ".dex/media/audio/music";
pub const DEFAULT_SFX_DIR: &str = ".dex/media/audio/sfx";
const AUDIO_GEN_TIMEOUT_SECS: u64 = 180;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedAudio {
    pub kind: String,
    pub path: PathBuf,
    pub prompt: String,
    pub duration_secs: Option<f64>,
    pub backend: String,
}

fn audio_gen_base_url() -> String {
    std::env::var("DEX_AUDIO_GEN_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:8189".to_string())
        .trim_end_matches('/')
        .to_string()
}

async fn wake_audio_extension(supervisor: Option<&Arc<ExtensionSupervisor>>) {
    if let Some(sup) = supervisor {
        let reg = sup.load_registry().await;
        if reg.get("soundbox-mcp").is_some_and(|m| m.enabled) {
            let _ = sup.start_extension("soundbox-mcp").await;
            sup.touch_last_used("soundbox-mcp").await;
        }
    }
}

/// Generates music via a local compatible sidecar (`soundbox` / `acestep-cpp` / HTTP audio endpoint),
/// waking the extension through `ExtensionSupervisor` if sleeping.
pub async fn generate_music(
    prompt: &str,
    lyrics: Option<&str>,
    duration_secs: f64,
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<&EventBus>,
) -> Result<GeneratedAudio, AudioError> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return Err(AudioError::InvalidInput {
            reason: "Music generation prompt cannot be empty".to_string(),
        });
    }

    wake_audio_extension(supervisor.as_ref()).await;

    let out_dir = PathBuf::from(DEFAULT_MUSIC_DIR);
    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| AudioError::Io {
            path: out_dir.to_string_lossy().to_string(),
            source: e,
        })?;

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dest = out_dir.join(format!("music_{ts}.wav"));

    let endpoint = format!("{}/v1/audio/music", audio_gen_base_url());
    let payload = serde_json::json!({
        "prompt": trimmed,
        "lyrics": lyrics,
        "duration_secs": duration_secs.clamp(2.0, 240.0),
    });

    dispatch_audio_generation("music", &endpoint, payload, dest, trimmed, Some(duration_secs), event_bus).await
}

/// Generates a sound effect (SFX) via a local compatible sidecar (`soundbox` / HTTP audio endpoint),
/// waking the extension through `ExtensionSupervisor` if sleeping.
pub async fn generate_sfx(
    prompt: &str,
    duration_secs: f64,
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<&EventBus>,
) -> Result<GeneratedAudio, AudioError> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return Err(AudioError::InvalidInput {
            reason: "SFX description prompt cannot be empty".to_string(),
        });
    }

    wake_audio_extension(supervisor.as_ref()).await;

    let out_dir = PathBuf::from(DEFAULT_SFX_DIR);
    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| AudioError::Io {
            path: out_dir.to_string_lossy().to_string(),
            source: e,
        })?;

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dest = out_dir.join(format!("sfx_{ts}.wav"));

    let endpoint = format!("{}/v1/audio/sfx", audio_gen_base_url());
    let payload = serde_json::json!({
        "prompt": trimmed,
        "duration_secs": duration_secs.clamp(0.5, 30.0),
    });

    dispatch_audio_generation("sfx", &endpoint, payload, dest, trimmed, Some(duration_secs), event_bus).await
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
        .unwrap_or_else(|_| reqwest::Client::new());

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
            "Audio sidecar unreachable at '{endpoint}': {e}. Install/start `soundbox-mcp` or set `DEX_AUDIO_GEN_URL`."
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

    let bytes = resp.bytes().await.map_err(|e| AudioError::Http {
        url: endpoint.to_string(),
        source: e,
    })?;

    tokio::fs::write(&dest, &bytes)
        .await
        .map_err(|e| AudioError::Io {
            path: dest.to_string_lossy().to_string(),
            source: e,
        })?;

    let measured_duration = probe_wav_duration(&dest).or(requested_duration);
    info!(kind = %kind, path = ?dest, "Saved generated audio asset");

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
        backend: endpoint.to_string(),
    })
}
