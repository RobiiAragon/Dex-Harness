use std::path::Path;
use std::time::Duration;
use async_trait::async_trait;
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::{debug, info};

use crate::error::AudioError;

const STT_TIMEOUT_SECS: u64 = 180;

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
}

/// HTTP Whisper backend compatible with OpenAI `/v1/audio/transcriptions`,
/// `whisper-server`, `mlx-audio`, and `soundbox`.
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
        let response_format = if include_timestamps {
            "verbose_json"
        } else {
            "json"
        };

        let form = Form::new()
            .part("file", part)
            .text("model", self.model.clone())
            .text("response_format", response_format.to_string());

        let endpoint = if self.base_url.ends_with("/v1/audio/transcriptions") {
            self.base_url.clone()
        } else {
            format!("{}/v1/audio/transcriptions", self.base_url)
        };

        debug!(endpoint = %endpoint, model = %self.model, "Dispatching HTTP Whisper transcription");

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

        parse_whisper_http_json(&body, self.name())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhisperCliKind {
    MlxWhisper,
    WhisperCpp,
}

/// CLI Whisper backend invoking `mlx_whisper` or `whisper-cli` asynchronously if present in `$PATH`.
pub struct CliWhisperBackend {
    kind: WhisperCliKind,
    binary: String,
    model_path: Option<String>,
}

impl CliWhisperBackend {
    pub fn mlx_whisper() -> Self {
        Self {
            kind: WhisperCliKind::MlxWhisper,
            binary: "mlx_whisper".to_string(),
            model_path: std::env::var("DEX_MLX_WHISPER_MODEL").ok(),
        }
    }

    pub fn whisper_cpp() -> Self {
        Self {
            kind: WhisperCliKind::WhisperCpp,
            binary: "whisper-cli".to_string(),
            model_path: std::env::var("DEX_WHISPER_MODEL").ok(),
        }
    }
}

#[async_trait]
impl SttBackend for CliWhisperBackend {
    fn name(&self) -> &str {
        match self.kind {
            WhisperCliKind::MlxWhisper => "mlx_whisper",
            WhisperCliKind::WhisperCpp => "whisper-cli",
        }
    }

    async fn transcribe(
        &self,
        audio_path: &Path,
        include_timestamps: bool,
    ) -> Result<TranscriptionResult, AudioError> {
        let mut cmd = Command::new(&self.binary);

        match self.kind {
            WhisperCliKind::MlxWhisper => {
                cmd.arg(audio_path);
                if let Some(ref model) = self.model_path {
                    cmd.args(["--model", model]);
                }
                cmd.args(["--output-format", "txt"]);
            }
            WhisperCliKind::WhisperCpp => {
                if let Some(ref model) = self.model_path {
                    cmd.args(["-m", model]);
                }
                if !include_timestamps {
                    cmd.arg("--no-timestamps");
                }
                cmd.args(["-f"]);
                cmd.arg(audio_path);
            }
        }

        info!(binary = %self.binary, path = ?audio_path, "Running local CLI Whisper STT");

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
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(AudioError::Subprocess {
                command: self.binary.clone(),
                message: format!("Exited with {}: {}", output.status, stderr),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(TranscriptionResult {
            text: stdout,
            language: None,
            duration_secs: None,
            segments: Vec::new(),
            backend: self.name().to_string(),
        })
    }
}

/// Automatically detects and runs the best available local or configured STT backend.
pub struct AutoSttEngine;

impl AutoSttEngine {
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

        // 2. Probe common local HTTP whisper / mlx-audio / soundbox ports quickly
        let probe_client = reqwest::Client::builder()
            .timeout(Duration::from_millis(350))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        for candidate in [
            "http://127.0.0.1:8080",
            "http://127.0.0.1:8189",
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

        // 3. Local CLI binaries (`mlx_whisper` or `whisper-cli`)
        if is_binary_in_path("mlx_whisper").await {
            return Ok(Box::new(CliWhisperBackend::mlx_whisper()));
        }
        if is_binary_in_path("whisper-cli").await {
            return Ok(Box::new(CliWhisperBackend::whisper_cpp()));
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

        Err(AudioError::SttUnavailable {
            reason: "No local or remote Whisper STT backend found. Start `whisper-server`/`mlx-audio` (or set `DEX_WHISPER_URL`), install `mlx_whisper` (`pip install mlx-whisper`) or `whisper-cli` (`brew install whisper-cpp`), or set `OPENAI_API_KEY`.".to_string(),
        })
    }

    pub async fn transcribe(
        audio_path: &Path,
        include_timestamps: bool,
    ) -> Result<TranscriptionResult, AudioError> {
        if !tokio::fs::try_exists(audio_path).await.unwrap_or(false) {
            return Err(AudioError::InvalidInput {
                reason: format!("Audio file does not exist: '{}'", audio_path.display()),
            });
        }

        let backend = Self::detect_best().await?;
        backend.transcribe(audio_path, include_timestamps).await
    }
}

async fn is_binary_in_path(binary: &str) -> bool {
    let probe = Command::new("which").arg(binary).output();
    match tokio::time::timeout(Duration::from_secs(2), probe).await {
        Ok(Ok(out)) => out.status.success(),
        _ => false,
    }
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
}
