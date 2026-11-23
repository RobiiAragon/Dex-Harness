use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use harness_core::{DexEvent, EventBus};
use harness_extensions::ExtensionSupervisor;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::error::AudioError;
use crate::stt::normalize_language_code;
use crate::voices::{
    list_voice_profiles, load_audio_settings, DEFAULT_MLX_TTS_MODEL,
};

pub const DEFAULT_TTS_DIR: &str = ".dex/media/audio/tts";
const TTS_TIMEOUT_SECS: u64 = 180;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsResult {
    pub path: PathBuf,
    pub duration_secs: Option<f64>,
    pub backend: String,
}

/// Rich options for Voice Studio speech synthesis & zero-shot voice cloning.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TtsOptions {
    /// Voice ID (matches a saved `ClonedVoiceProfile.id`), preset voice (`Vivian`, `Monica`, `Samantha`), or `None`.
    #[serde(default)]
    pub voice: Option<String>,
    /// Target language (`"auto"`, `"es"`, `"en"`, etc.).
    #[serde(default)]
    pub language: Option<String>,
    /// Engine override: `"auto"`, `"mlx-audio"`, `"sidecar"`, `"macos-say"`.
    #[serde(default)]
    pub engine: Option<String>,
    /// Model repo ID for `mlx-audio` / sidecar (e.g. `"mlx-community/Qwen3-TTS-12Hz-0.6B-Base-8bit"`).
    #[serde(default)]
    pub model: Option<String>,
    /// Optional explicit reference audio path for zero-shot voice cloning.
    #[serde(default)]
    pub ref_audio: Option<PathBuf>,
    /// Optional verbatim transcript of `ref_audio` for higher cloning fidelity.
    #[serde(default)]
    pub ref_text: Option<String>,
    /// Optional custom HTTP TTS sidecar endpoint URL (e.g. Fish Audio S2-Pro, Step-Audio EditX, OmniVoice).
    #[serde(default)]
    pub sidecar_url: Option<String>,
    /// Optional destination path (`.wav` or `.m4a`).
    #[serde(default)]
    pub output_path: Option<PathBuf>,
}

/// Synthesizes speech to an audio file using saved Voice Studio defaults (backward-compatible helper).
pub async fn synthesize_speech(
    text: &str,
    voice: Option<&str>,
    output_path: Option<&Path>,
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<&EventBus>,
) -> Result<TtsResult, AudioError> {
    let opts = TtsOptions {
        voice: voice.map(String::from),
        output_path: output_path.map(Path::to_path_buf),
        ..Default::default()
    };
    synthesize_speech_with_options(text, opts, supervisor, event_bus).await
}

/// Synthesizes speech with full support for `mlx-audio` (Qwen3-TTS, Chatterbox, Kokoro),
/// saved voice cloning profiles (`~/.dex/voices/`), external VoiceStudio sidecars
/// (Fish Audio S2-Pro, Step-Audio EditX, OmniVoice), and native macOS `say` fallback.
pub async fn synthesize_speech_with_options(
    text: &str,
    mut opts: TtsOptions,
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<&EventBus>,
) -> Result<TtsResult, AudioError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(AudioError::InvalidInput {
            reason: "TTS text cannot be empty".to_string(),
        });
    }

    let settings = load_audio_settings().await;

    // Resolve saved cloned voice profile if `opts.voice` matches a saved profile ID
    if opts.ref_audio.is_none() {
        let candidate_id = opts
            .voice
            .as_deref()
            .or(settings.active_voice_id.as_deref());
        if let Some(vid) = candidate_id {
            let profiles = list_voice_profiles().await;
            if let Some(prof) = profiles.into_iter().find(|p| p.id == vid) {
                opts.ref_audio = Some(PathBuf::from(&prof.ref_audio_path));
                if opts.ref_text.is_none() {
                    opts.ref_text = prof.ref_text;
                }
                if opts.language.is_none() && !prof.language.eq_ignore_ascii_case("auto") {
                    opts.language = Some(prof.language);
                }
                // Clear preset voice name since we are using zero-shot reference cloning
                opts.voice = None;
            }
        }
    }

    let effective_lang = normalize_language_code(
        opts.language
            .as_deref()
            .or(Some(settings.language.as_str())),
    );
    let effective_engine = opts
        .engine
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(settings.tts_engine.as_str())
        .to_lowercase();
    let effective_model = opts
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(if settings.tts_model.trim().is_empty() {
            DEFAULT_MLX_TTS_MODEL
        } else {
            settings.tts_model.as_str()
        })
        .to_string();
    let effective_sidecar = opts
        .sidecar_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| {
            settings
                .tts_sidecar_url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        });

    let final_path = match opts.output_path.as_deref() {
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

    // Explicit macOS Say mode
    if effective_engine == "macos-say" || effective_engine == "say" {
        #[cfg(target_os = "macos")]
        {
            let mac_voice = resolve_macos_voice(opts.voice.as_deref(), effective_lang.as_deref());
            let result =
                synthesize_macos_native(trimmed, mac_voice.as_deref(), &final_path).await?;
            publish_tts_event(event_bus, &result, trimmed);
            return Ok(result);
        }
        #[cfg(not(target_os = "macos"))]
        {
            return Err(AudioError::TtsFailed {
                reason: "Native macOS `say` is only available on macOS.".to_string(),
            });
        }
    }

    // Explicit sidecar mode (Fish Audio S2-Pro / Step-Audio EditX / OmniVoice / custom HTTP)
    if effective_engine == "sidecar" || effective_engine == "http" {
        let endpoint = effective_sidecar
            .or(resolve_tts_http_endpoint().await)
            .ok_or_else(|| AudioError::BackendNotInstalled {
                kind: "tts".to_string(),
                backend: "tts-sidecar".to_string(),
                install_hint: "Start a TTS sidecar (Fish Audio S2-Pro :8080, Step-Audio EditX :8192, OmniVoice :8193) in Extensions or set a Sidecar URL.".to_string(),
            })?;
        let result = try_http_tts(
            &endpoint,
            trimmed,
            opts.voice.as_deref(),
            &effective_model,
            effective_lang.as_deref(),
            opts.ref_audio.as_deref(),
            opts.ref_text.as_deref(),
            &final_path,
        )
        .await?;
        publish_tts_event(event_bus, &result, trimmed);
        return Ok(result);
    }

    // Explicit `mlx-audio` or zero-shot voice cloning request
    let cloning_requested = opts.ref_audio.is_some();
    let mlx_audio_cmd = find_mlx_audio_command();
    if effective_engine == "mlx-audio" || (cloning_requested && effective_engine == "auto") {
        if let Some((bin, prefix_args)) = mlx_audio_cmd {
            let result = synthesize_via_mlx_audio(
                &bin,
                &prefix_args,
                trimmed,
                &effective_model,
                opts.voice.as_deref(),
                effective_lang.as_deref(),
                opts.ref_audio.as_deref(),
                opts.ref_text.as_deref(),
                &final_path,
            )
            .await?;
            publish_tts_event(event_bus, &result, trimmed);
            return Ok(result);
        }
        // If cloning was requested and an external sidecar is explicitly configured, try it before failing
        if cloning_requested {
            if let Some(ref endpoint) = effective_sidecar {
                let result = try_http_tts(
                    endpoint,
                    trimmed,
                    opts.voice.as_deref(),
                    &effective_model,
                    effective_lang.as_deref(),
                    opts.ref_audio.as_deref(),
                    opts.ref_text.as_deref(),
                    &final_path,
                )
                .await?;
                publish_tts_event(event_bus, &result, trimmed);
                return Ok(result);
            }
        }
        return Err(AudioError::BackendNotInstalled {
            kind: "tts".to_string(),
            backend: "mlx-audio".to_string(),
            install_hint: "Install Voice Engine (`mlx-audio`) with 1 click in Audio & Docs to run Qwen3-TTS, Chatterbox, Kokoro, and voice cloning locally on Apple Silicon.".to_string(),
        });
    }

    // Auto mode:
    // 1. Try `mlx-audio` if installed and a neural model is selected (not `apple/macos-say`)
    if !effective_model.eq_ignore_ascii_case("apple/macos-say") {
        if let Some((bin, prefix_args)) = mlx_audio_cmd {
            match synthesize_via_mlx_audio(
                &bin,
                &prefix_args,
                trimmed,
                &effective_model,
                opts.voice.as_deref(),
                effective_lang.as_deref(),
                opts.ref_audio.as_deref(),
                opts.ref_text.as_deref(),
                &final_path,
            )
            .await
            {
                Ok(result) => {
                    publish_tts_event(event_bus, &result, trimmed);
                    return Ok(result);
                }
                Err(e) => {
                    warn!(error = %e, "mlx-audio TTS failed in auto mode; attempting fallback");
                }
            }
        }
    }

    // 2. Wake soundbox-mcp if installed & enabled in ExtensionSupervisor
    if let Some(ref sup) = supervisor {
        let reg = sup.load_registry().await;
        if reg.get("soundbox-mcp").is_some_and(|m| m.enabled) {
            let _ = sup.start_extension("soundbox-mcp").await;
            sup.touch_last_used("soundbox-mcp").await;
        }
    }

    // 3. Try configured HTTP TTS sidecar (`sidecar_url`, `DEX_TTS_URL`, or local soundbox endpoint)
    let http_endpoint = match effective_sidecar {
        Some(url) => Some(url),
        None => resolve_tts_http_endpoint().await,
    };
    if let Some(endpoint) = http_endpoint {
        match try_http_tts(
            &endpoint,
            trimmed,
            opts.voice.as_deref(),
            &effective_model,
            effective_lang.as_deref(),
            opts.ref_audio.as_deref(),
            opts.ref_text.as_deref(),
            &final_path,
        )
        .await
        {
            Ok(result) => {
                publish_tts_event(event_bus, &result, trimmed);
                return Ok(result);
            }
            Err(e) => {
                warn!(endpoint = %endpoint, error = %e, "HTTP TTS sidecar failed; attempting native fallback");
            }
        }
    }

    // 4. Native macOS fallback (`say` + `afconvert`)
    #[cfg(target_os = "macos")]
    {
        let mac_voice = resolve_macos_voice(opts.voice.as_deref(), effective_lang.as_deref());
        let result = synthesize_macos_native(trimmed, mac_voice.as_deref(), &final_path).await?;
        publish_tts_event(event_bus, &result, trimmed);
        Ok(result)
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err(AudioError::BackendNotInstalled {
            kind: "tts".to_string(),
            backend: "mlx-audio".to_string(),
            install_hint: "Install `mlx-audio` or configure a local HTTP TTS sidecar (`DEX_TTS_URL`).".to_string(),
        })
    }
}

fn find_mlx_audio_command() -> Option<(PathBuf, Vec<String>)> {
    if let Some(bin) = harness_core::find_executable_in_enriched_path("mlx_audio.tts.generate") {
        return Some((bin, Vec::new()));
    }
    let venv_dir = harness_core::home_dex_dir().join("runtimes/mlx-venv");
    let py = venv_dir.join("bin/python");
    if !py.is_file() {
        return None;
    }
    let lib_dir = venv_dir.join("lib");
    let Ok(rd) = std::fs::read_dir(&lib_dir) else {
        return None;
    };
    for entry in rd.flatten() {
        if entry.path().join("site-packages/mlx_audio").exists() {
            return Some((
                py,
                vec!["-m".to_string(), "mlx_audio.tts.generate".to_string()],
            ));
        }
    }
    None
}

/// Maps ISO language codes into `mlx-audio` `--lang_code` values cleanly per model family.
pub fn resolve_mlx_audio_lang_code(model: &str, lang: Option<&str>) -> Option<String> {
    let code = normalize_language_code(lang)?;
    let lower = model.to_lowercase();
    if lower.contains("kokoro") {
        // Kokoro expects single-letter lang codes: 'a' (US EN), 'b' (UK EN), 'e' (ES), 'f' (FR), 'i' (IT), 'p' (PT), 'j' (JA), 'z' (ZH)
        let k = match code.as_str() {
            "es" => "e",
            "en" => "a",
            "fr" => "f",
            "it" => "i",
            "pt" => "p",
            "ja" => "j",
            "zh" => "z",
            _ => "a",
        };
        Some(k.to_string())
    } else if lower.contains("qwen3-tts") || lower.contains("qwen") {
        let q = match code.as_str() {
            "es" => "Spanish",
            "en" => "English",
            "fr" => "French",
            "de" => "German",
            "it" => "Italian",
            "pt" => "Portuguese",
            "ja" => "Japanese",
            "zh" => "Chinese",
            "ko" => "Korean",
            other => other,
        };
        Some(q.to_string())
    } else {
        Some(code)
    }
}

/// Builds the CLI argument vector for `mlx_audio.tts.generate`.
#[allow(clippy::too_many_arguments)]
pub fn build_mlx_audio_tts_args(
    text: &str,
    model: &str,
    voice: Option<&str>,
    language: Option<&str>,
    ref_audio: Option<&Path>,
    ref_text: Option<&str>,
    out_dir: &Path,
    file_prefix: &str,
) -> Vec<String> {
    let mut args = vec![
        "--model".to_string(),
        model.to_string(),
        "--text".to_string(),
        text.to_string(),
        "--output_path".to_string(),
        out_dir.to_string_lossy().to_string(),
        "--file_prefix".to_string(),
        file_prefix.to_string(),
        "--audio_format".to_string(),
        "wav".to_string(),
    ];

    if let Some(ref_p) = ref_audio {
        args.push("--ref_audio".to_string());
        args.push(ref_p.to_string_lossy().to_string());
        if let Some(rt) = ref_text.map(str::trim).filter(|s| !s.is_empty()) {
            args.push("--ref_text".to_string());
            args.push(rt.to_string());
        }
    } else if let Some(v) = voice.map(str::trim).filter(|s| !s.is_empty()) {
        args.push("--voice".to_string());
        args.push(v.to_string());
    }

    if let Some(lang_arg) = resolve_mlx_audio_lang_code(model, language) {
        args.push("--lang_code".to_string());
        args.push(lang_arg);
    }

    args
}

#[allow(clippy::too_many_arguments)]
async fn synthesize_via_mlx_audio(
    bin: &Path,
    prefix_args: &[String],
    text: &str,
    model: &str,
    voice: Option<&str>,
    language: Option<&str>,
    ref_audio: Option<&Path>,
    ref_text: Option<&str>,
    dest: &Path,
) -> Result<TtsResult, AudioError> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let tmp_dir = harness_core::resolve_dex_subpath("tmp/tts").join(format!("mlx_audio_{ts}"));
    tokio::fs::create_dir_all(&tmp_dir)
        .await
        .map_err(|e| AudioError::Io {
            path: tmp_dir.to_string_lossy().to_string(),
            source: e,
        })?;

    let prefix = "dex_tts";
    let args = build_mlx_audio_tts_args(
        text, model, voice, language, ref_audio, ref_text, &tmp_dir, prefix,
    );

    let mut cmd = Command::new(bin);
    harness_core::apply_enriched_path(&mut cmd);
    cmd.args(prefix_args).args(&args);

    info!(
        binary = %bin.display(),
        model = %model,
        cloning = ref_audio.is_some(),
        "Running mlx-audio local neural TTS"
    );

    let output = tokio::time::timeout(Duration::from_secs(TTS_TIMEOUT_SECS), cmd.output())
        .await
        .map_err(|_| AudioError::Timeout {
            operation: "mlx-audio TTS synthesis".to_string(),
            timeout_secs: TTS_TIMEOUT_SECS,
        })?
        .map_err(|e| AudioError::Subprocess {
            command: bin.to_string_lossy().to_string(),
            message: e.to_string(),
        })?;

    if !output.status.success() {
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = match (stderr.is_empty(), stdout.is_empty()) {
            (false, false) => format!("stderr: {stderr}\nstdout: {stdout}"),
            (false, true) => stderr,
            (true, false) => stdout,
            (true, true) => "No output captured".to_string(),
        };
        return Err(AudioError::TtsFailed {
            reason: format!("mlx-audio exited with {}: {}", output.status, detail),
        });
    }

    // Locate generated .wav artifact (`dex_tts_000.wav`, `dex_tts.wav`, or first `.wav`)
    let mut generated_wav: Option<PathBuf> = None;
    for candidate in [
        tmp_dir.join(format!("{prefix}_000.wav")),
        tmp_dir.join(format!("{prefix}.wav")),
    ] {
        if tokio::fs::try_exists(&candidate).await.unwrap_or(false) {
            generated_wav = Some(candidate);
            break;
        }
    }
    if generated_wav.is_none() {
        if let Ok(mut rd) = tokio::fs::read_dir(&tmp_dir).await {
            while let Ok(Some(entry)) = rd.next_entry().await {
                let p = entry.path();
                if p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
                {
                    generated_wav = Some(p);
                    break;
                }
            }
        }
    }

    let Some(src_wav) = generated_wav else {
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
        return Err(AudioError::TtsFailed {
            reason: "mlx-audio completed without producing a .wav output file".to_string(),
        });
    };

    if tokio::fs::rename(&src_wav, dest).await.is_err() {
        tokio::fs::copy(&src_wav, dest)
            .await
            .map_err(|e| AudioError::Io {
                path: dest.to_string_lossy().to_string(),
                source: e,
            })?;
    }
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;

    let duration_secs = probe_wav_duration(dest);
    Ok(TtsResult {
        path: dest.to_path_buf(),
        duration_secs,
        backend: format!("mlx-audio ({model})"),
    })
}

fn resolve_macos_voice(explicit_voice: Option<&str>, lang: Option<&str>) -> Option<String> {
    if let Some(v) = explicit_voice.map(str::trim).filter(|s| !s.is_empty()) {
        // Ignore non-macOS neural voice names if falling back
        if !matches!(
            v.to_lowercase().as_str(),
            "auto" | "alloy" | "default"
        ) {
            return Some(v.to_string());
        }
    }
    match lang {
        Some("es") => Some("Monica".to_string()),
        Some("en") => Some("Samantha".to_string()),
        Some("fr") => Some("Thomas".to_string()),
        Some("de") => Some("Anna".to_string()),
        Some("it") => Some("Alice".to_string()),
        Some("pt") => Some("Luciana".to_string()),
        _ => None,
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

    for base in [
        "http://127.0.0.1:8189",
        "http://127.0.0.1:8880",
        "http://127.0.0.1:8192",
        "http://127.0.0.1:8193",
    ] {
        if probe.get(base).send().await.is_ok() {
            return Some(format!("{base}/v1/audio/speech"));
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
async fn try_http_tts(
    endpoint: &str,
    text: &str,
    voice: Option<&str>,
    model: &str,
    language: Option<&str>,
    ref_audio: Option<&Path>,
    ref_text: Option<&str>,
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

    let mut payload = serde_json::json!({
        "model": std::env::var("DEX_TTS_MODEL").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| model.to_string()),
        "input": text,
        "voice": voice.unwrap_or("alloy"),
        "response_format": ext,
    });
    if let Some(lang) = language {
        payload["language"] = serde_json::json!(lang);
    }
    if let Some(ref_p) = ref_audio {
        payload["reference_audio_path"] = serde_json::json!(ref_p.to_string_lossy().to_string());
    }
    if let Some(rt) = ref_text.filter(|s| !s.trim().is_empty()) {
        payload["reference_text"] = serde_json::json!(rt);
    }

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

    // If requested voice isn't installed on macOS, retry cleanly with default system voice
    if !say_out.status.success() && voice.is_some() {
        let _ = tokio::fs::remove_file(&temp_aiff).await;
        let mut fallback_cmd = Command::new("say");
        fallback_cmd.arg("-o").arg(&temp_aiff).arg(text);
        let fb_out =
            tokio::time::timeout(Duration::from_secs(TTS_TIMEOUT_SECS), fallback_cmd.output())
                .await
                .map_err(|_| AudioError::Timeout {
                    operation: "macos say fallback".to_string(),
                    timeout_secs: TTS_TIMEOUT_SECS,
                })?
                .map_err(|e| AudioError::Subprocess {
                    command: "say".to_string(),
                    message: e.to_string(),
                })?;
        if !fb_out.status.success() {
            let stderr = String::from_utf8_lossy(&fb_out.stderr).trim().to_string();
            return Err(AudioError::TtsFailed {
                reason: format!("`say` exited with {}: {}", fb_out.status, stderr),
            });
        }
    } else if !say_out.status.success() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mlx_audio_args_cloning_and_language_mapping() {
        assert_eq!(
            resolve_mlx_audio_lang_code("mlx-community/Kokoro-82M-bf16", Some("es")),
            Some("e".to_string())
        );
        assert_eq!(
            resolve_mlx_audio_lang_code("mlx-community/Qwen3-TTS-12Hz-0.6B-Base-8bit", Some("es")),
            Some("Spanish".to_string())
        );
        let args = build_mlx_audio_tts_args(
            "Hola desde Dex",
            "mlx-community/Qwen3-TTS-12Hz-0.6B-Base-8bit",
            None,
            Some("es"),
            Some(Path::new("/tmp/ref.wav")),
            Some("Muestra de voz"),
            Path::new("/tmp/out"),
            "dex_tts",
        );
        assert!(args.contains(&"--ref_audio".to_string()));
        assert!(args.contains(&"--ref_text".to_string()));
        assert!(args.contains(&"--lang_code".to_string()));
        assert!(args.contains(&"Spanish".to_string()));
    }

    #[cfg(target_os = "macos")]
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
