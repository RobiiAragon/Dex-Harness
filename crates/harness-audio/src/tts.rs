use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use harness_core::{DexEvent, EventBus};
use harness_extensions::ExtensionSupervisor;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::error::AudioError;

pub const DEFAULT_TTS_DIR: &str = ".dex/media/audio/tts";
const TTS_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsResult {
    pub path: PathBuf,
    pub duration_secs: Option<f64>,
    pub backend: String,
}

/// Synthesizes speech to an audio file using a configured TTS sidecar (`/v1/audio/speech`),
/// automatically falling back to native macOS `say` + `afconvert` when no sidecar is active.
pub async fn synthesize_speech(
    text: &str,
    voice: Option<&str>,
    output_path: Option<&Path>,
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<&EventBus>,
) -> Result<TtsResult, AudioError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(AudioError::InvalidInput {
            reason: "TTS text cannot be empty".to_string(),
        });
    }

    let final_path = match output_path {
        Some(p) => harness_core::resolve_safe_path(p),
        None => {
            let dir = harness_core::resolve_dex_subpath(DEFAULT_TTS_DIR);
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|e| AudioError::Io {
                    path: dir.to_string_lossy().to_string(),
                    source: e,
                })?;
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            dir.join(format!("tts_{ts}.wav"))
        }
    };

    if let Some(parent) = final_path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AudioError::Io {
                    path: parent.to_string_lossy().to_string(),
                    source: e,
                })?;
        }
    }

    // Wake soundbox-mcp if installed & enabled in ExtensionSupervisor
    if let Some(ref sup) = supervisor {
        let reg = sup.load_registry().await;
        if reg.get("soundbox-mcp").is_some_and(|m| m.enabled) {
            let _ = sup.start_extension("soundbox-mcp").await;
            sup.touch_last_used("soundbox-mcp").await;
        }
    }

    // 1. Try configured HTTP TTS sidecar (`DEX_TTS_URL` or local soundbox/mlx-audio endpoint)
    if let Some(endpoint) = resolve_tts_http_endpoint().await {
        match try_http_tts(&endpoint, trimmed, voice, &final_path).await {
            Ok(result) => {
                publish_tts_event(event_bus, &result, trimmed);
                return Ok(result);
            }
            Err(e) => {
                warn!(endpoint = %endpoint, error = %e, "HTTP TTS sidecar failed; attempting native fallback");
            }
        }
    }

    // 2. Native macOS fallback (`say` + `afconvert`)
    #[cfg(target_os = "macos")]
    {
        let result = synthesize_macos_native(trimmed, voice, &final_path).await?;
        publish_tts_event(event_bus, &result, trimmed);
        Ok(result)
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err(AudioError::TtsFailed {
            reason: "No HTTP TTS sidecar reachable (`DEX_TTS_URL`) and native macOS `say` fallback is unavailable on this OS.".to_string(),
        })
    }
}

fn publish_tts_event(event_bus: Option<&EventBus>, result: &TtsResult, prompt: &str) {
    if let Some(bus) = event_bus {
        let preview: String = prompt.chars().take(120).collect();
        bus.publish(DexEvent::MediaGenerated {
            kind: "tts".to_string(),
            path: result.path.to_string_lossy().to_string(),
            prompt: preview,
            duration_secs: result.duration_secs,
        });
    }
}

async fn resolve_tts_http_endpoint() -> Option<String> {
    if let Ok(url) = std::env::var("DEX_TTS_URL") {
        let trimmed = url.trim().trim_end_matches('/').to_string();
        if !trimmed.is_empty() {
            return Some(if trimmed.ends_with("/v1/audio/speech") {
                trimmed
            } else {
                format!("{trimmed}/v1/audio/speech")
            });
        }
    }

    let probe = reqwest::Client::builder()
        .timeout(Duration::from_millis(250))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    for base in ["http://127.0.0.1:8189", "http://127.0.0.1:8880"] {
        if probe.get(base).send().await.is_ok() {
            return Some(format!("{base}/v1/audio/speech"));
        }
    }
    None
}

async fn try_http_tts(
    endpoint: &str,
    text: &str,
    voice: Option<&str>,
    dest: &Path,
) -> Result<TtsResult, AudioError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(TTS_TIMEOUT_SECS))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let ext = dest
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("wav")
        .to_lowercase();

    let payload = serde_json::json!({
        "model": std::env::var("DEX_TTS_MODEL").unwrap_or_else(|_| "tts-1".to_string()),
        "input": text,
        "voice": voice.unwrap_or("alloy"),
        "response_format": ext,
    });

    debug!(endpoint = %endpoint, "Calling HTTP TTS endpoint");
    let mut req = client.post(endpoint).json(&payload);
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if !key.trim().is_empty() {
            req = req.bearer_auth(key);
        }
    }

    let resp = tokio::time::timeout(Duration::from_secs(TTS_TIMEOUT_SECS), req.send())
        .await
        .map_err(|_| AudioError::Timeout {
            operation: "http tts".to_string(),
            timeout_secs: TTS_TIMEOUT_SECS,
        })?
        .map_err(|e| AudioError::Http {
            url: endpoint.to_string(),
            source: e,
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AudioError::TtsFailed {
            reason: format!("HTTP {status} from {endpoint}: {body}"),
        });
    }

    let bytes = resp.bytes().await.map_err(|e| AudioError::Http {
        url: endpoint.to_string(),
        source: e,
    })?;

    tokio::fs::write(dest, &bytes)
        .await
        .map_err(|e| AudioError::Io {
            path: dest.to_string_lossy().to_string(),
            source: e,
        })?;

    let duration_secs = probe_wav_duration(dest);
    Ok(TtsResult {
        path: dest.to_path_buf(),
        duration_secs,
        backend: "http-tts".to_string(),
    })
}

#[cfg(target_os = "macos")]
async fn synthesize_macos_native(
    text: &str,
    voice: Option<&str>,
    dest: &Path,
) -> Result<TtsResult, AudioError> {
    let temp_aiff = dest.with_extension("tmp.aiff");

    let mut say_cmd = Command::new("say");
    if let Some(v) = voice {
        if !v.trim().is_empty() {
            say_cmd.args(["-v", v.trim()]);
        }
    }
    say_cmd.arg("-o").arg(&temp_aiff).arg(text);

    info!(dest = ?dest, voice = ?voice, "Synthesizing speech via native macOS `say`");
    let say_out = tokio::time::timeout(Duration::from_secs(TTS_TIMEOUT_SECS), say_cmd.output())
        .await
        .map_err(|_| AudioError::Timeout {
            operation: "macos say".to_string(),
            timeout_secs: TTS_TIMEOUT_SECS,
        })?
        .map_err(|e| AudioError::Subprocess {
            command: "say".to_string(),
            message: e.to_string(),
        })?;

    if !say_out.status.success() {
        let stderr = String::from_utf8_lossy(&say_out.stderr).trim().to_string();
        let _ = tokio::fs::remove_file(&temp_aiff).await;
        return Err(AudioError::TtsFailed {
            reason: format!("`say` exited with {}: {}", say_out.status, stderr),
        });
    }

    let is_m4a = dest
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("m4a"));

    let mut af_cmd = Command::new("afconvert");
    if is_m4a {
        af_cmd.args(["-f", "m4af", "-d", "aac"]);
    } else {
        af_cmd.args(["-f", "WAVE", "-d", "LEI16@22050"]);
    }
    af_cmd.arg(&temp_aiff).arg(dest);

    let af_out = tokio::time::timeout(Duration::from_secs(TTS_TIMEOUT_SECS), af_cmd.output())
        .await
        .map_err(|_| AudioError::Timeout {
            operation: "macos afconvert".to_string(),
            timeout_secs: TTS_TIMEOUT_SECS,
        })?
        .map_err(|e| AudioError::Subprocess {
            command: "afconvert".to_string(),
            message: e.to_string(),
        })?;

    let _ = tokio::fs::remove_file(&temp_aiff).await;

    if !af_out.status.success() {
        let stderr = String::from_utf8_lossy(&af_out.stderr).trim().to_string();
        return Err(AudioError::TtsFailed {
            reason: format!("`afconvert` exited with {}: {}", af_out.status, stderr),
        });
    }

    let duration_secs = probe_wav_duration(dest);
    Ok(TtsResult {
        path: dest.to_path_buf(),
        duration_secs,
        backend: "macos-say".to_string(),
    })
}

pub(crate) fn probe_wav_duration(path: &Path) -> Option<f64> {
    let reader = hound::WavReader::open(path).ok()?;
    let spec = reader.spec();
    let sample_rate = f64::from(spec.sample_rate);
    let channels = f64::from(spec.channels.max(1));
    if sample_rate <= 0.0 {
        return None;
    }
    let frames = reader.len() as f64 / channels;
    let secs = frames / sample_rate;
    Some((secs * 100.0).round() / 100.0)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_macos_native_tts_generates_valid_wav() {
        let out_path =
            std::env::temp_dir().join(format!("dex_tts_test_{}.wav", std::process::id()));
        let res = synthesize_macos_native("Dex audio test.", None, &out_path)
            .await
            .expect("macOS native TTS should succeed");

        assert_eq!(res.backend, "macos-say");
        assert!(tokio::fs::try_exists(&res.path).await.unwrap_or(false));
        assert!(res.duration_secs.unwrap_or(0.0) > 0.1);

        let _ = tokio::fs::remove_file(&out_path).await;
    }
}
