use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use async_trait::async_trait;
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::error::AudioError;
use crate::recorder::preprocess_wav_file;
use crate::voices::{load_audio_settings, DEFAULT_WHISPER_MODEL};

const STT_TIMEOUT_SECS: u64 = 300;
const SWIFT_STT_COMPILE_TIMEOUT_SECS: u64 = 60;
pub const DEFAULT_PARAKEET_MODEL: &str = "mlx-community/parakeet-tdt-0.6b-v3";
pub const DEFAULT_FASTER_WHISPER_MODEL: &str = "large-v3";

#[cfg(target_os = "macos")]
const APPLE_SPEECH_SWIFT_SOURCE: &str = r#"
import Foundation
import Speech

guard CommandLine.arguments.count >= 2 else {
    fputs("Usage: dex-macos-stt <audio-file> [locale]\n", stderr)
    exit(2)
}

let audioURL = URL(fileURLWithPath: CommandLine.arguments[1])
let requestedLocaleId: String? = CommandLine.arguments.count >= 3 ? CommandLine.arguments[2] : nil
let sem = DispatchSemaphore(value: 0)
var authStatus: SFSpeechRecognizerAuthorizationStatus = SFSpeechRecognizer.authorizationStatus()

if authStatus == .notDetermined {
    SFSpeechRecognizer.requestAuthorization { status in
        authStatus = status
        sem.signal()
    }
    _ = sem.wait(timeout: .now() + 15)
}

guard authStatus == .authorized else {
    fputs("PERMISSION_DENIED: Grant Speech Recognition access in System Settings -> Privacy & Security -> Speech Recognition, or install mlx-whisper (1-click in Dex).\n", stderr)
    exit(3)
}

let locale: Locale = {
    if let req = requestedLocaleId, !req.isEmpty {
        return Locale(identifier: req)
    }
    return Locale.current
}()

guard let recognizer = SFSpeechRecognizer(locale: locale) ?? SFSpeechRecognizer(locale: Locale(identifier: "en-US")), recognizer.isAvailable else {
    fputs("RECOGNIZER_UNAVAILABLE: Apple Speech Recognizer is not available for requested locale. Install mlx-whisper (1-click in Dex).\n", stderr)
    exit(4)
}

let request = SFSpeechURLRecognitionRequest(url: audioURL)
request.shouldReportPartialResults = false
if recognizer.supportsOnDeviceRecognition {
    request.requiresOnDeviceRecognition = true
}

let doneSem = DispatchSemaphore(value: 0)
var finalText = ""
var errMessage: String? = nil

let task = recognizer.recognitionTask(with: request) { result, error in
    if let r = result {
        finalText = r.bestTranscription.formattedString
        if r.isFinal {
            doneSem.signal()
        }
    }
    if let e = error {
        if finalText.isEmpty {
            errMessage = e.localizedDescription
        }
        doneSem.signal()
    }
}

let waitResult = doneSem.wait(timeout: .now() + 45)
task.cancel()

if waitResult == .timedOut {
    fputs("TIMEOUT: Apple Speech recognition timed out.\n", stderr)
    exit(5)
}
if let err = errMessage, finalText.isEmpty {
    fputs("SPEECH_ERROR: \(err)\n", stderr)
    exit(6)
}

print(finalText)
"#;

/// Configurable options for Speech-to-Text transcription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttOptions {
    /// Explicit ISO-639-1 language code (`"es"`, `"en"`, etc.) or `None`/`"auto"` for auto-detect.
    #[serde(default)]
    pub language: Option<String>,
    /// Preferred ASR engine (`"auto"`, `"mlx-whisper"`, `"parakeet-mlx"`, `"faster-whisper"`, `"whisper-cli"`, `"apple-speech"`, `"http"`).
    #[serde(default)]
    pub engine: Option<String>,
    /// Optional model ID or path override (e.g. `"mlx-community/whisper-large-v3-turbo"`).
    #[serde(default)]
    pub model: Option<String>,
    /// Include timestamped segments when supported by the backend.
    #[serde(default)]
    pub include_timestamps: bool,
    /// When true (default), applies 16kHz mono resample + VAD silence trim + soft peak normalization to WAV files before STT.
    #[serde(default = "default_true")]
    pub preprocess: bool,
}

fn default_true() -> bool {
    true
}

impl Default for SttOptions {
    fn default() -> Self {
        Self {
            language: None,
            engine: None,
            model: None,
            include_timestamps: false,
            preprocess: true,
        }
    }
}

/// Normalizes a user-facing language setting (`"Auto"`, `"Español (es)"`, `"es"`, `"en"`) into
/// an optional ISO-639-1 code (`None` for auto-detect).
pub fn normalize_language_code(raw: Option<&str>) -> Option<String> {
    let val = raw?.trim();
    if val.is_empty() {
        return None;
    }
    let lower = val.to_lowercase();
    match lower.as_str() {
        "auto" | "detect" | "none" => None,
        "es" | "es-es" | "es-mx" | "es-419" | "spanish" | "español" | "espanol" => {
            Some("es".to_string())
        }
        "en" | "en-us" | "en-gb" | "english" | "inglés" | "ingles" => Some("en".to_string()),
        "fr" | "french" | "français" => Some("fr".to_string()),
        "de" | "german" | "deutsch" => Some("de".to_string()),
        "pt" | "portuguese" | "português" => Some("pt".to_string()),
        "it" | "italian" | "italiano" => Some("it".to_string()),
        "ja" | "japanese" => Some("ja".to_string()),
        "zh" | "chinese" => Some("zh".to_string()),
        "ko" | "korean" => Some("ko".to_string()),
        other => {
            // Strip region suffix like `es-MX` -> `es` when simple 2-letter prefix exists
            if let Some((prefix, _)) = other.split_once('-') {
                if prefix.len() == 2 && prefix.chars().all(|c| c.is_ascii_alphabetic()) {
                    return Some(prefix.to_string());
                }
            }
            Some(other.to_string())
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscriptSegment {
    pub start_secs: Option<f64>,
    pub end_secs: Option<f64>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionResult {
    pub text: String,
    pub language: Option<String>,
    pub duration_secs: Option<f64>,
    pub segments: Vec<TranscriptSegment>,
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_db: Option<f32>,
}

impl TranscriptionResult {
    /// Formats transcript with optional `[MM:SS.s - MM:SS.s]` timestamps.
    pub fn format_display(&self, include_timestamps: bool) -> String {
        if include_timestamps && !self.segments.is_empty() {
            let mut lines = Vec::with_capacity(self.segments.len());
            for seg in &self.segments {
                let start = seg.start_secs.unwrap_or(0.0);
                let end = seg.end_secs.unwrap_or(start);
                lines.push(format!("[{start:06.2}s -> {end:06.2}s] {}", seg.text.trim()));
            }
            lines.join("\n")
        } else {
            self.text.trim().to_string()
        }
    }
}

/// Pluggable Speech-to-Text engine interface.
#[async_trait]
pub trait SttBackend: Send + Sync {
    fn name(&self) -> &str;

    async fn transcribe(
        &self,
        audio_path: &Path,
        include_timestamps: bool,
    ) -> Result<TranscriptionResult, AudioError>;

    async fn transcribe_with_options(
        &self,
        audio_path: &Path,
        opts: &SttOptions,
    ) -> Result<TranscriptionResult, AudioError> {
        self.transcribe(audio_path, opts.include_timestamps).await
    }
}

/// HTTP Whisper backend compatible with OpenAI `/v1/audio/transcriptions`,
/// `whisper-server`, and `mlx-audio`.
pub struct HttpWhisperBackend {
    client: reqwest::Client,
    base_url: String,
    model: String,
    api_key: Option<String>,
}

impl HttpWhisperBackend {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        let mut url = base_url.into();
        while url.ends_with('/') {
            url.pop();
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(STT_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            client,
            base_url: url,
            model: model.into(),
            api_key,
        }
    }
}

#[derive(Debug, Deserialize)]
struct WhisperVerboseSegment {
    #[serde(default)]
    start: Option<f64>,
    #[serde(default)]
    end: Option<f64>,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct WhisperHttpResponse {
    #[serde(default)]
    text: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    segments: Vec<WhisperVerboseSegment>,
}

pub(crate) fn parse_whisper_http_json(
    body: &str,
    backend_name: &str,
) -> Result<TranscriptionResult, AudioError> {
    let parsed: WhisperHttpResponse =
        serde_json::from_str(body).map_err(|e| AudioError::InvalidInput {
            reason: format!("Failed to parse Whisper JSON response: {e}"),
        })?;

    let segments = parsed
        .segments
        .into_iter()
        .map(|s| TranscriptSegment {
            start_secs: s.start,
            end_secs: s.end,
            text: s.text,
        })
        .collect();

    Ok(TranscriptionResult {
        text: parsed.text.trim().to_string(),
        language: parsed.language,
        duration_secs: parsed.duration,
        segments,
        backend: backend_name.to_string(),
        peak_db: None,
    })
}

#[async_trait]
impl SttBackend for HttpWhisperBackend {
    fn name(&self) -> &str {
        "http-whisper"
    }

    async fn transcribe(
        &self,
        audio_path: &Path,
        include_timestamps: bool,
    ) -> Result<TranscriptionResult, AudioError> {
        let opts = SttOptions {
            include_timestamps,
            ..Default::default()
        };
        self.transcribe_with_options(audio_path, &opts).await
    }

    async fn transcribe_with_options(
        &self,
        audio_path: &Path,
        opts: &SttOptions,
    ) -> Result<TranscriptionResult, AudioError> {
        let bytes = tokio::fs::read(audio_path)
            .await
            .map_err(|e| AudioError::Io {
                path: audio_path.to_string_lossy().to_string(),
                source: e,
            })?;

        let file_name = audio_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("audio.wav")
            .to_string();

        let part = Part::bytes(bytes).file_name(file_name);
        let response_format = if opts.include_timestamps {
            "verbose_json"
        } else {
            "json"
        };

        let effective_model = opts
            .model
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.model)
            .to_string();

        let mut form = Form::new()
            .part("file", part)
            .text("model", effective_model.clone())
            .text("response_format", response_format.to_string());

        let lang_code = normalize_language_code(opts.language.as_deref());
        if let Some(ref lang) = lang_code {
            form = form.text("language", lang.clone());
        }

        let endpoint = if self.base_url.ends_with("/v1/audio/transcriptions") {
            self.base_url.clone()
        } else {
            format!("{}/v1/audio/transcriptions", self.base_url)
        };

        debug!(
            endpoint = %endpoint,
            model = %effective_model,
            language = ?lang_code,
            "Dispatching HTTP Whisper transcription"
        );

        let mut req = self.client.post(&endpoint).multipart(form);
        if let Some(ref key) = self.api_key {
            req = req.bearer_auth(key);
        }

        let resp = tokio::time::timeout(Duration::from_secs(STT_TIMEOUT_SECS), req.send())
            .await
            .map_err(|_| AudioError::Timeout {
                operation: "http whisper transcription".to_string(),
                timeout_secs: STT_TIMEOUT_SECS,
            })?
            .map_err(|e| AudioError::Http {
                url: endpoint.clone(),
                source: e,
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            return Err(AudioError::Subprocess {
                command: endpoint,
                message: format!("HTTP {status}: {err_body}"),
            });
        }

        let body = resp.text().await.map_err(|e| AudioError::Http {
            url: endpoint,
            source: e,
        })?;

        let mut parsed = parse_whisper_http_json(&body, self.name())?;
        if parsed.language.is_none() {
            parsed.language = lang_code;
        }
        Ok(parsed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhisperCliKind {
    MlxWhisper,
    WhisperCpp,
    ParakeetMlx,
    FasterWhisper,
}

fn find_mlx_venv_python_for_module(module_dir: &str) -> Option<PathBuf> {
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
        if entry
            .path()
            .join("site-packages")
            .join(module_dir)
            .exists()
        {
            return Some(py);
        }
    }
    None
}

/// Builds CLI argument vector for `mlx_whisper`.
pub fn build_mlx_whisper_args(
    audio_path: &Path,
    model: Option<&str>,
    language: Option<&str>,
    out_dir: &Path,
) -> Vec<String> {
    let mut args = vec![audio_path.to_string_lossy().to_string()];
    let effective_model = model
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_WHISPER_MODEL);
    args.push("--model".to_string());
    args.push(effective_model.to_string());

    if let Some(lang) = normalize_language_code(language) {
        args.push("--language".to_string());
        args.push(lang);
    }

    args.push("--output-format".to_string());
    args.push("txt".to_string());
    args.push("--output-dir".to_string());
    args.push(out_dir.to_string_lossy().to_string());
    args
}

/// Builds CLI argument vector for `parakeet-mlx`.
pub fn build_parakeet_args(
    audio_path: &Path,
    model: Option<&str>,
    out_dir: &Path,
) -> Vec<String> {
    let effective_model = model
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.to_lowercase().contains("parakeet"))
        .unwrap_or(DEFAULT_PARAKEET_MODEL);
    vec![
        audio_path.to_string_lossy().to_string(),
        "--model".to_string(),
        effective_model.to_string(),
        "--output-format".to_string(),
        "txt".to_string(),
        "--output-dir".to_string(),
        out_dir.to_string_lossy().to_string(),
    ]
}

/// CLI ASR backend invoking `mlx_whisper`, `parakeet-mlx`, `faster-whisper`, or `whisper-cli` asynchronously.
pub struct CliWhisperBackend {
    kind: WhisperCliKind,
    binary: String,
    model_path: Option<String>,
    via_python_module: Option<&'static str>,
}

impl CliWhisperBackend {
    pub fn mlx_whisper() -> Self {
        let resolved = harness_core::find_executable_in_enriched_path("mlx_whisper")
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "mlx_whisper".to_string());
        Self {
            kind: WhisperCliKind::MlxWhisper,
            binary: resolved,
            model_path: std::env::var("DEX_MLX_WHISPER_MODEL")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            via_python_module: None,
        }
    }

    pub fn parakeet_mlx() -> Self {
        if let Some(bin) = harness_core::find_executable_in_enriched_path("parakeet-mlx") {
            Self {
                kind: WhisperCliKind::ParakeetMlx,
                binary: bin.to_string_lossy().to_string(),
                model_path: Some(DEFAULT_PARAKEET_MODEL.to_string()),
                via_python_module: None,
            }
        } else if let Some(py) = find_mlx_venv_python_for_module("parakeet_mlx") {
            Self {
                kind: WhisperCliKind::ParakeetMlx,
                binary: py.to_string_lossy().to_string(),
                model_path: Some(DEFAULT_PARAKEET_MODEL.to_string()),
                via_python_module: Some("parakeet_mlx"),
            }
        } else {
            Self {
                kind: WhisperCliKind::ParakeetMlx,
                binary: "parakeet-mlx".to_string(),
                model_path: Some(DEFAULT_PARAKEET_MODEL.to_string()),
                via_python_module: None,
            }
        }
    }

    pub fn faster_whisper() -> Self {
        let py = find_mlx_venv_python_for_module("faster_whisper")
            .or_else(|| harness_core::find_executable_in_enriched_path("python3"))
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "python3".to_string());
        Self {
            kind: WhisperCliKind::FasterWhisper,
            binary: py,
            model_path: Some(DEFAULT_FASTER_WHISPER_MODEL.to_string()),
            via_python_module: Some("faster_whisper"),
        }
    }

    pub fn whisper_cpp() -> Self {
        let resolved = harness_core::find_executable_in_enriched_path("whisper-cli")
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "whisper-cli".to_string());
        Self {
            kind: WhisperCliKind::WhisperCpp,
            binary: resolved,
            model_path: std::env::var("DEX_WHISPER_MODEL")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            via_python_module: None,
        }
    }
}

const FASTER_WHISPER_PY_SCRIPT: &str = r#"
import json, sys
from faster_whisper import WhisperModel
audio_path = sys.argv[1]
model_name = sys.argv[2] if len(sys.argv) > 2 and sys.argv[2] else "large-v3"
lang = sys.argv[3] if len(sys.argv) > 3 and sys.argv[3] not in ("", "auto") else None
model = WhisperModel(model_name, device="cpu", compute_type="int8")
segments_iter, info = model.transcribe(audio_path, language=lang, vad_filter=True)
segs = []
parts = []
for s in segments_iter:
    t = (s.text or "").strip()
    if t:
        parts.append(t)
        segs.append({"start": float(s.start), "end": float(s.end), "text": t})
print(json.dumps({
    "text": " ".join(parts).strip(),
    "language": getattr(info, "language", lang),
    "duration": getattr(info, "duration", None),
    "segments": segs
}))
"#;

#[async_trait]
impl SttBackend for CliWhisperBackend {
    fn name(&self) -> &str {
        match self.kind {
            WhisperCliKind::MlxWhisper => "mlx_whisper",
            WhisperCliKind::WhisperCpp => "whisper-cli",
            WhisperCliKind::ParakeetMlx => "parakeet-mlx",
            WhisperCliKind::FasterWhisper => "faster-whisper",
        }
    }

    async fn transcribe(
        &self,
        audio_path: &Path,
        include_timestamps: bool,
    ) -> Result<TranscriptionResult, AudioError> {
        let opts = SttOptions {
            include_timestamps,
            ..Default::default()
        };
        self.transcribe_with_options(audio_path, &opts).await
    }

    async fn transcribe_with_options(
        &self,
        audio_path: &Path,
        opts: &SttOptions,
    ) -> Result<TranscriptionResult, AudioError> {
        let mut cmd = Command::new(&self.binary);
        harness_core::apply_enriched_path(&mut cmd);

        let mut tmp_out_dir: Option<PathBuf> = None;
        let lang_code = normalize_language_code(opts.language.as_deref());

        match self.kind {
            WhisperCliKind::MlxWhisper => {
                let ts = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                let out_dir = harness_core::resolve_dex_subpath("tmp/stt").join(format!("mlx_{ts}"));
                let _ = tokio::fs::create_dir_all(&out_dir).await;
                let chosen_model = opts
                    .model
                    .as_deref()
                    .filter(|s| !s.trim().is_empty() && s.to_lowercase().contains("whisper"))
                    .or(self.model_path.as_deref());
                let args = build_mlx_whisper_args(
                    audio_path,
                    chosen_model,
                    lang_code.as_deref(),
                    &out_dir,
                );
                cmd.args(&args);
                tmp_out_dir = Some(out_dir);
            }
            WhisperCliKind::ParakeetMlx => {
                let ts = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                let out_dir =
                    harness_core::resolve_dex_subpath("tmp/stt").join(format!("parakeet_{ts}"));
                let _ = tokio::fs::create_dir_all(&out_dir).await;
                if let Some(module_name) = self.via_python_module {
                    cmd.args(["-m", module_name]);
                }
                let chosen_model = opts.model.as_deref().or(self.model_path.as_deref());
                let args = build_parakeet_args(audio_path, chosen_model, &out_dir);
                cmd.args(&args);
                tmp_out_dir = Some(out_dir);
            }
            WhisperCliKind::FasterWhisper => {
                let model_arg = opts
                    .model
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .or(self.model_path.as_deref())
                    .unwrap_or(DEFAULT_FASTER_WHISPER_MODEL);
                cmd.args(["-c", FASTER_WHISPER_PY_SCRIPT])
                    .arg(audio_path)
                    .arg(model_arg)
                    .arg(lang_code.as_deref().unwrap_or("auto"));
            }
            WhisperCliKind::WhisperCpp => {
                if let Some(ref model) = self.model_path {
                    cmd.args(["-m", model]);
                }
                if let Some(ref lang) = lang_code {
                    cmd.args(["-l", lang]);
                }
                if !opts.include_timestamps {
                    cmd.arg("--no-timestamps");
                }
                cmd.arg("-f").arg(audio_path);
            }
        }

        info!(
            binary = %self.binary,
            engine = %self.name(),
            language = ?lang_code,
            path = ?audio_path,
            "Running local CLI STT"
        );

        let output = tokio::time::timeout(Duration::from_secs(STT_TIMEOUT_SECS), cmd.output())
            .await
            .map_err(|_| AudioError::Timeout {
                operation: format!("{} transcription", self.binary),
                timeout_secs: STT_TIMEOUT_SECS,
            })?
            .map_err(|e| AudioError::Subprocess {
                command: self.binary.clone(),
                message: e.to_string(),
            })?;

        if !output.status.success() {
            if let Some(ref dir) = tmp_out_dir {
                let _ = tokio::fs::remove_dir_all(dir).await;
            }
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let combined = match (stderr.is_empty(), stdout.is_empty()) {
                (false, false) => format!("stderr: {stderr}\nstdout: {stdout}"),
                (false, true) => stderr,
                (true, false) => stdout,
                (true, true) => "No output captured".to_string(),
            };
            return Err(AudioError::Subprocess {
                command: self.binary.clone(),
                message: format!("Exited with {}: {}", output.status, combined),
            });
        }

        if self.kind == WhisperCliKind::FasterWhisper {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let json_line = stdout.lines().rev().find(|l| l.trim().starts_with('{')).unwrap_or(&stdout);
            if let Ok(mut parsed) = parse_whisper_http_json(json_line, self.name()) {
                if parsed.language.is_none() {
                    parsed.language = lang_code;
                }
                return Ok(parsed);
            }
        }

        let mut text = String::new();
        if let Some(ref dir) = tmp_out_dir {
            if let Some(stem) = audio_path.file_stem().and_then(|s| s.to_str()) {
                let txt_file = dir.join(format!("{stem}.txt"));
                if let Ok(content) = tokio::fs::read_to_string(&txt_file).await {
                    text = content.trim().to_string();
                }
            }
            if text.is_empty() {
                if let Ok(mut rd) = tokio::fs::read_dir(dir).await {
                    while let Ok(Some(entry)) = rd.next_entry().await {
                        if entry.path().extension().and_then(|e| e.to_str()) == Some("txt") {
                            if let Ok(content) = tokio::fs::read_to_string(entry.path()).await {
                                text = content.trim().to_string();
                                break;
                            }
                        }
                    }
                }
            }
            let _ = tokio::fs::remove_dir_all(dir).await;
        }
        if text.is_empty() {
            text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        }

        Ok(TranscriptionResult {
            text,
            language: lang_code,
            duration_secs: None,
            segments: Vec::new(),
            backend: self.name().to_string(),
            peak_db: None,
        })
    }
}

/// Native macOS Speech-to-Text fallback built on Apple's `Speech` (`SFSpeechRecognizer`) framework.
#[cfg(target_os = "macos")]
pub struct AppleSpeechSttBackend;

#[cfg(target_os = "macos")]
async fn ensure_apple_stt_binary() -> Result<PathBuf, AudioError> {
    let bin_dir = harness_core::home_dex_dir().join("bin");
    let bin_path = bin_dir.join("dex-macos-stt-v2");
    if tokio::fs::try_exists(&bin_path).await.unwrap_or(false) {
        return Ok(bin_path);
    }

    tokio::fs::create_dir_all(&bin_dir)
        .await
        .map_err(|e| AudioError::Io {
            path: bin_dir.to_string_lossy().to_string(),
            source: e,
        })?;

    let src_path = bin_dir.join("dex_macos_stt_v2.swift");
    tokio::fs::write(&src_path, APPLE_SPEECH_SWIFT_SOURCE)
        .await
        .map_err(|e| AudioError::Io {
            path: src_path.to_string_lossy().to_string(),
            source: e,
        })?;

    info!(dest = ?bin_path, "Compiling native Apple Speech STT helper binary once via swiftc");
    let mut cmd = Command::new("swiftc");
    harness_core::apply_enriched_path(&mut cmd);
    let output = tokio::time::timeout(
        Duration::from_secs(SWIFT_STT_COMPILE_TIMEOUT_SECS),
        cmd.arg("-O")
            .arg(&src_path)
            .arg("-o")
            .arg(&bin_path)
            .output(),
    )
    .await
    .map_err(|_| AudioError::Timeout {
        operation: "swiftc apple speech compilation".to_string(),
        timeout_secs: SWIFT_STT_COMPILE_TIMEOUT_SECS,
    })?
    .map_err(|e| AudioError::SttUnavailable {
        reason: format!(
            "Could not compile native Apple Speech helper (`swiftc`: {e}). Install `mlx-whisper` (1-click in Dex -> Audio & Docs)."
        ),
    })?;

    let _ = tokio::fs::remove_file(&src_path).await;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AudioError::SttUnavailable {
            reason: format!(
                "Native Apple Speech helper compilation failed ({stderr}). Install `mlx-whisper` (1-click in Dex)."
            ),
        });
    }

    Ok(bin_path)
}

#[cfg(target_os = "macos")]
fn language_to_apple_locale(lang: Option<&str>) -> Option<&'static str> {
    match normalize_language_code(lang).as_deref() {
        Some("es") => Some("es-ES"),
        Some("en") => Some("en-US"),
        Some("fr") => Some("fr-FR"),
        Some("de") => Some("de-DE"),
        Some("pt") => Some("pt-BR"),
        Some("it") => Some("it-IT"),
        Some("ja") => Some("ja-JP"),
        Some("zh") => Some("zh-CN"),
        Some("ko") => Some("ko-KR"),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
#[async_trait]
impl SttBackend for AppleSpeechSttBackend {
    fn name(&self) -> &str {
        "apple-speech"
    }

    async fn transcribe(
        &self,
        audio_path: &Path,
        include_timestamps: bool,
    ) -> Result<TranscriptionResult, AudioError> {
        let opts = SttOptions {
            include_timestamps,
            ..Default::default()
        };
        self.transcribe_with_options(audio_path, &opts).await
    }

    async fn transcribe_with_options(
        &self,
        audio_path: &Path,
        opts: &SttOptions,
    ) -> Result<TranscriptionResult, AudioError> {
        let bin = ensure_apple_stt_binary().await?;
        let mut cmd = Command::new(&bin);
        harness_core::apply_enriched_path(&mut cmd);
        cmd.arg(audio_path);
        if let Some(locale) = language_to_apple_locale(opts.language.as_deref()) {
            cmd.arg(locale);
        }

        let output = tokio::time::timeout(Duration::from_secs(60), cmd.output())
            .await
            .map_err(|_| AudioError::Timeout {
                operation: "apple speech transcription".to_string(),
                timeout_secs: 60,
            })?
            .map_err(|e| AudioError::Subprocess {
                command: bin.to_string_lossy().to_string(),
                message: e.to_string(),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(AudioError::SttUnavailable {
                reason: if stderr.is_empty() {
                    "Apple Speech Recognizer failed. Grant Speech Recognition permission in System Settings or install `mlx-whisper` (1-click in Audio & Docs / Models Hub).".to_string()
                } else {
                    stderr
                },
            });
        }

        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(TranscriptionResult {
            text,
            language: normalize_language_code(opts.language.as_deref()),
            duration_secs: None,
            segments: Vec::new(),
            backend: self.name().to_string(),
            peak_db: None,
        })
    }
}

/// Automatically detects and runs the best available local or configured STT backend.
pub struct AutoSttEngine;

impl AutoSttEngine {
    pub async fn detect_for_engine(
        engine: Option<&str>,
    ) -> Result<Box<dyn SttBackend>, AudioError> {
        let desired = engine
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("auto")
            .to_lowercase();

        match desired.as_str() {
            "mlx-whisper" | "mlx_whisper" => {
                if is_binary_in_path("mlx_whisper").await {
                    return Ok(Box::new(CliWhisperBackend::mlx_whisper()));
                }
                Err(AudioError::BackendNotInstalled {
                    kind: "stt".to_string(),
                    backend: "mlx-whisper".to_string(),
                    install_hint: "Install `mlx-whisper` (1-click in Audio & Docs -> Audio Models or Models Hub). [dex-install:mlx-whisper]".to_string(),
                })
            }
            "parakeet-mlx" | "parakeet_mlx" | "parakeet" => {
                if is_binary_in_path("parakeet-mlx").await
                    || find_mlx_venv_python_for_module("parakeet_mlx").is_some()
                {
                    return Ok(Box::new(CliWhisperBackend::parakeet_mlx()));
                }
                Err(AudioError::BackendNotInstalled {
                    kind: "stt".to_string(),
                    backend: "parakeet-mlx".to_string(),
                    install_hint: "Install `parakeet-mlx` (1-click in Audio & Docs -> Audio Models or Models Hub). [dex-install:parakeet-mlx]".to_string(),
                })
            }
            "faster-whisper" | "faster_whisper" => {
                if find_mlx_venv_python_for_module("faster_whisper").is_some() {
                    return Ok(Box::new(CliWhisperBackend::faster_whisper()));
                }
                Err(AudioError::BackendNotInstalled {
                    kind: "stt".to_string(),
                    backend: "faster-whisper".to_string(),
                    install_hint: "Install `faster-whisper` (1-click in Audio & Docs -> Audio Models or Models Hub). [dex-install:faster-whisper]".to_string(),
                })
            }
            "whisper-cli" | "whispercpp" | "whisper.cpp" => {
                if is_binary_in_path("whisper-cli").await {
                    return Ok(Box::new(CliWhisperBackend::whisper_cpp()));
                }
                Err(AudioError::BackendNotInstalled {
                    kind: "stt".to_string(),
                    backend: "whisper-cli".to_string(),
                    install_hint: "Install `whisper-cpp` via Homebrew (`brew install whisper-cpp`) or switch to `mlx-whisper`. [dex-install:mlx-whisper]".to_string(),
                })
            }
            #[cfg(target_os = "macos")]
            "apple-speech" | "apple" => Ok(Box::new(AppleSpeechSttBackend)),
            _ => Self::detect_best().await,
        }
    }

    pub async fn detect_best() -> Result<Box<dyn SttBackend>, AudioError> {
        // 1. Explicit DEX_WHISPER_URL environment variable
        if let Ok(url) = std::env::var("DEX_WHISPER_URL") {
            if !url.trim().is_empty() {
                let model = std::env::var("DEX_WHISPER_MODEL")
                    .unwrap_or_else(|_| "whisper-1".to_string());
                let key = std::env::var("OPENAI_API_KEY").ok();
                return Ok(Box::new(HttpWhisperBackend::new(url, model, key)));
            }
        }

        // 2. Local Apple Silicon CLI engines (`mlx_whisper` first for best multilingual accuracy, then `parakeet-mlx`, `faster-whisper`, `whisper-cli`)
        if is_binary_in_path("mlx_whisper").await {
            return Ok(Box::new(CliWhisperBackend::mlx_whisper()));
        }
        if is_binary_in_path("parakeet-mlx").await
            || find_mlx_venv_python_for_module("parakeet_mlx").is_some()
        {
            return Ok(Box::new(CliWhisperBackend::parakeet_mlx()));
        }
        if find_mlx_venv_python_for_module("faster_whisper").is_some() {
            return Ok(Box::new(CliWhisperBackend::faster_whisper()));
        }
        if is_binary_in_path("whisper-cli").await {
            return Ok(Box::new(CliWhisperBackend::whisper_cpp()));
        }

        // 3. Probe common local HTTP whisper / mlx-audio ports quickly
        //    (8189 belongs to the `dex-audiogen` music sidecar and is intentionally excluded)
        let probe_client = reqwest::Client::builder()
            .timeout(Duration::from_millis(350))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        for candidate in [
            "http://127.0.0.1:8000",
        ] {
            if probe_client.get(candidate).send().await.is_ok() {
                debug!(endpoint = candidate, "Detected live local HTTP audio server for STT");
                return Ok(Box::new(HttpWhisperBackend::new(
                    candidate,
                    "whisper-1",
                    None,
                )));
            }
        }

        // 4. OpenAI API fallback if OPENAI_API_KEY is configured
        if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
            if !api_key.trim().is_empty() {
                return Ok(Box::new(HttpWhisperBackend::new(
                    "https://api.openai.com",
                    "whisper-1",
                    Some(api_key),
                )));
            }
        }

        // 5. Native macOS Speech framework (`SFSpeechRecognizer`) zero-download fallback
        #[cfg(target_os = "macos")]
        if harness_core::find_executable_in_enriched_path("swiftc").is_some()
            || tokio::fs::try_exists(harness_core::home_dex_dir().join("bin/dex-macos-stt-v2"))
                .await
                .unwrap_or(false)
        {
            return Ok(Box::new(AppleSpeechSttBackend));
        }

        Err(AudioError::BackendNotInstalled {
            kind: "stt".to_string(),
            backend: "mlx-whisper".to_string(),
            install_hint: "Install Voice Engine (`mlx-whisper` / `parakeet-mlx`) with 1 click in Audio & Docs or Models Hub. [dex-install:mlx-whisper]".to_string(),
        })
    }

    pub async fn transcribe(
        audio_path: &Path,
        include_timestamps: bool,
    ) -> Result<TranscriptionResult, AudioError> {
        let opts = SttOptions {
            include_timestamps,
            ..Default::default()
        };
        Self::transcribe_with_options(audio_path, &opts).await
    }

    pub async fn transcribe_with_options(
        audio_path: &Path,
        opts: &SttOptions,
    ) -> Result<TranscriptionResult, AudioError> {
        if !tokio::fs::try_exists(audio_path).await.unwrap_or(false) {
            return Err(AudioError::InvalidInput {
                reason: format!("Audio file does not exist: '{}'", audio_path.display()),
            });
        }

        let settings = load_audio_settings().await;
        let effective_opts = SttOptions {
            language: opts
                .language
                .clone()
                .or_else(|| normalize_language_code(Some(&settings.language))),
            engine: opts
                .engine
                .clone()
                .or_else(|| {
                    if settings.asr_engine.eq_ignore_ascii_case("auto") {
                        None
                    } else {
                        Some(settings.asr_engine.clone())
                    }
                }),
            model: opts
                .model
                .clone()
                .or_else(|| {
                    if settings.asr_model.trim().is_empty() {
                        None
                    } else {
                        Some(settings.asr_model.clone())
                    }
                }),
            include_timestamps: opts.include_timestamps,
            preprocess: opts.preprocess,
        };

        let backend = Self::detect_for_engine(effective_opts.engine.as_deref()).await?;

        // Preprocess WAV audio (16kHz mono resample + VAD silence trim + soft peak normalization)
        let is_wav = audio_path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("wav"));

        let (target_path, temp_prep, peak_db) = if effective_opts.preprocess && is_wav {
            match preprocess_wav_file(audio_path).await {
                Ok((prep_path, peak)) => (prep_path.clone(), Some(prep_path), Some(peak)),
                Err(e) => {
                    warn!(error = %e, "WAV preprocessing skipped; using original file");
                    (audio_path.to_path_buf(), None, None)
                }
            }
        } else {
            (audio_path.to_path_buf(), None, None)
        };

        let res = backend
            .transcribe_with_options(&target_path, &effective_opts)
            .await;

        if let Some(tmp) = temp_prep {
            let _ = tokio::fs::remove_file(tmp).await;
        }

        let mut out = res?;
        if out.peak_db.is_none() {
            out.peak_db = peak_db;
        }
        Ok(out)
    }
}

async fn is_binary_in_path(binary: &str) -> bool {
    harness_core::find_executable_in_enriched_path(binary).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_whisper_http_verbose_json_and_format() {
        let raw = r#"{
            "text": "Hola Dex, compila el proyecto.",
            "language": "es",
            "duration": 2.4,
            "segments": [
                { "start": 0.0, "end": 1.1, "text": " Hola Dex," },
                { "start": 1.1, "end": 2.4, "text": " compila el proyecto." }
            ]
        }"#;

        let res = parse_whisper_http_json(raw, "http-whisper").expect("parse verbose json");
        assert_eq!(res.text, "Hola Dex, compila el proyecto.");
        assert_eq!(res.language.as_deref(), Some("es"));
        assert_eq!(res.segments.len(), 2);

        let with_ts = res.format_display(true);
        assert!(with_ts.contains("[000.00s -> 001.10s] Hola Dex,"));
        assert!(with_ts.contains("[001.10s -> 002.40s] compila el proyecto."));

        let plain = res.format_display(false);
        assert_eq!(plain, "Hola Dex, compila el proyecto.");
    }

    #[test]
    fn test_normalize_language_code_and_mlx_whisper_args() {
        assert_eq!(normalize_language_code(None), None);
        assert_eq!(normalize_language_code(Some("auto")), None);
        assert_eq!(normalize_language_code(Some("Español")), Some("es".to_string()));
        assert_eq!(normalize_language_code(Some("es-MX")), Some("es".to_string()));
        assert_eq!(normalize_language_code(Some("English")), Some("en".to_string()));

        let args_es = build_mlx_whisper_args(
            Path::new("/tmp/test.wav"),
            None,
            Some("es"),
            Path::new("/tmp/out"),
        );
        assert!(args_es.contains(&"--language".to_string()));
        assert!(args_es.contains(&"es".to_string()));
        assert!(args_es.contains(&DEFAULT_WHISPER_MODEL.to_string()));

        let args_auto = build_mlx_whisper_args(
            Path::new("/tmp/test.wav"),
            None,
            Some("auto"),
            Path::new("/tmp/out"),
        );
        assert!(!args_auto.contains(&"--language".to_string()));

        let pk_args = build_parakeet_args(
            Path::new("/tmp/test.wav"),
            None,
            Path::new("/tmp/out"),
        );
        assert!(pk_args.contains(&DEFAULT_PARAKEET_MODEL.to_string()));
    }
}
