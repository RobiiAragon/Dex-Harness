use std::path::Path;
use std::sync::Arc;
use async_trait::async_trait;
use harness_core::{DexEvent, EventBus, Tool, ToolError, ToolRegistry};
use harness_extensions::ExtensionSupervisor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::generator::{generate_music, generate_sfx};
use crate::stt::{AutoSttEngine, SttOptions};
use crate::tts::{synthesize_speech_with_options, TtsOptions};

/// Arguments for `audio_transcribe`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AudioTranscribeArgs {
    /// Path to the audio or video file (.wav, .mp3, .m4a).
    pub path: String,
    /// When true, includes segment start/end timestamps when supported by the backend.
    #[serde(default)]
    pub timestamps: bool,
    /// Optional explicit language code (e.g. "es", "en", or "auto").
    #[serde(default)]
    pub language: Option<String>,
}

/// Transcribes an audio file to text using the best available local or HTTP Whisper STT backend.
pub struct AudioTranscribeTool {
    event_bus: Option<EventBus>,
}

impl AudioTranscribeTool {
    pub fn new(event_bus: Option<EventBus>) -> Self {
        Self { event_bus }
    }
}

#[async_trait]
impl Tool for AudioTranscribeTool {
    fn name(&self) -> &str {
        "audio_transcribe"
    }

    fn description(&self) -> &str {
        "Transcribes an audio/video file (.wav, .mp3, .m4a) into text with optional segment timestamps using Whisper STT."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(AudioTranscribeArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: AudioTranscribeArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let opts = SttOptions {
            language: args.language.clone(),
            include_timestamps: args.timestamps,
            ..Default::default()
        };
        let result = AutoSttEngine::transcribe_with_options(Path::new(&args.path), &opts)
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: e.to_string(),
            })?;

        let formatted = result.format_display(args.timestamps);
        if let Some(ref bus) = self.event_bus {
            let preview: String = formatted.chars().take(120).collect();
            bus.publish(DexEvent::MediaGenerated {
                kind: "transcript".to_string(),
                path: args.path.clone(),
                prompt: preview,
                duration_secs: result.duration_secs,
            });
        }

        Ok(format!(
            "Transcription (backend: {}):\n{}",
            result.backend, formatted
        ))
    }
}

/// Arguments for `audio_tts`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AudioTtsArgs {
    /// Text to synthesize into spoken audio.
    pub text: String,
    /// Optional voice name or saved cloned voice profile ID (e.g. "Monica", "Samantha", "alloy").
    #[serde(default)]
    pub voice: Option<String>,
    /// Optional language code (e.g. "es", "en", "auto").
    #[serde(default)]
    pub language: Option<String>,
    /// Optional output path (.wav or .m4a). Defaults to `.dex/media/audio/tts/`.
    #[serde(default)]
    pub output_path: Option<String>,
}

/// Synthesizes speech via TTS sidecar or native macOS `say` + `afconvert`.
pub struct AudioTtsTool {
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<EventBus>,
}

impl AudioTtsTool {
    pub fn new(supervisor: Option<Arc<ExtensionSupervisor>>, event_bus: Option<EventBus>) -> Self {
        Self {
            supervisor,
            event_bus,
        }
    }
}

#[async_trait]
impl Tool for AudioTtsTool {
    fn name(&self) -> &str {
        "audio_tts"
    }

    fn description(&self) -> &str {
        "Synthesizes spoken voice audio (.wav/.m4a) from text using a configured TTS sidecar or native macOS speech fallback."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(AudioTtsArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: AudioTtsArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let opts = TtsOptions {
            voice: args.voice.clone(),
            language: args.language.clone(),
            output_path: args.output_path.as_ref().map(std::path::PathBuf::from),
            ..Default::default()
        };
        let result = synthesize_speech_with_options(
            &args.text,
            opts,
            self.supervisor.clone(),
            self.event_bus.as_ref(),
        )
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: e.to_string(),
        })?;

        let dur_str = result
            .duration_secs
            .map(|d| format!("{d:.2}s"))
            .unwrap_or_else(|| "unknown".to_string());
        Ok(format!(
            "Speech synthesized successfully (backend: {}, duration: {}). Saved to: {}",
            result.backend,
            dur_str,
            result.path.display()
        ))
    }
}

fn default_music_duration() -> f64 {
    15.0
}

/// Arguments for `audio_generate_music`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AudioGenerateMusicArgs {
    /// Musical style, mood, instruments, and tempo prompt.
    pub prompt: String,
    /// Optional song lyrics.
    #[serde(default)]
    pub lyrics: Option<String>,
    /// Target duration in seconds (default: 15.0).
    #[serde(default = "default_music_duration")]
    pub duration_secs: f64,
}

/// Generates music tracks via a local audio sidecar (`soundbox` / `acestep-cpp`).
pub struct AudioGenerateMusicTool {
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<EventBus>,
}

impl AudioGenerateMusicTool {
    pub fn new(supervisor: Option<Arc<ExtensionSupervisor>>, event_bus: Option<EventBus>) -> Self {
        Self {
            supervisor,
            event_bus,
        }
    }
}

#[async_trait]
impl Tool for AudioGenerateMusicTool {
    fn name(&self) -> &str {
        "audio_generate_music"
    }

    fn description(&self) -> &str {
        "Generates an original music clip from a style/genre prompt and optional lyrics using a local audio generation sidecar."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(AudioGenerateMusicArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: AudioGenerateMusicArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let result = generate_music(
            &args.prompt,
            args.lyrics.as_deref(),
            args.duration_secs,
            self.supervisor.clone(),
            self.event_bus.as_ref(),
        )
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: e.to_string(),
        })?;

        Ok(format!(
            "Music generated and saved to: {} (duration: {:?}s)",
            result.path.display(),
            result.duration_secs
        ))
    }
}

fn default_sfx_duration() -> f64 {
    4.0
}

/// Arguments for `audio_generate_sfx`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AudioGenerateSfxArgs {
    /// Sound effect description (e.g. "futuristic laser blast", "rain on window").
    pub prompt: String,
    /// Target duration in seconds (default: 4.0).
    #[serde(default = "default_sfx_duration")]
    pub duration_secs: f64,
}

/// Generates sound effects (SFX) via a local audio generation sidecar.
pub struct AudioGenerateSfxTool {
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<EventBus>,
}

impl AudioGenerateSfxTool {
    pub fn new(supervisor: Option<Arc<ExtensionSupervisor>>, event_bus: Option<EventBus>) -> Self {
        Self {
            supervisor,
            event_bus,
        }
    }
}

#[async_trait]
impl Tool for AudioGenerateSfxTool {
    fn name(&self) -> &str {
        "audio_generate_sfx"
    }

    fn description(&self) -> &str {
        "Generates a sound effect (SFX) audio file from a text description and duration via a local audio sidecar."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(AudioGenerateSfxArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: AudioGenerateSfxArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let result = generate_sfx(
            &args.prompt,
            args.duration_secs,
            self.supervisor.clone(),
            self.event_bus.as_ref(),
        )
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: e.to_string(),
        })?;

        Ok(format!(
            "SFX generated and saved to: {} (duration: {:?}s)",
            result.path.display(),
            result.duration_secs
        ))
    }
}

/// Registers all audio agent tools into `registry` as `ToolVisibility::Deferred` under category `"Audio"`.
pub fn register_audio_tools(
    registry: &mut ToolRegistry,
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<EventBus>,
) {
    registry.register_deferred(
        Arc::new(AudioTranscribeTool::new(event_bus.clone())),
        "Audio",
        vec![
            "audio".to_string(),
            "stt".to_string(),
            "whisper".to_string(),
            "transcribe".to_string(),
            "voice".to_string(),
        ],
    );

    registry.register_deferred(
        Arc::new(AudioTtsTool::new(supervisor.clone(), event_bus.clone())),
        "Audio",
        vec![
            "audio".to_string(),
            "tts".to_string(),
            "speech".to_string(),
            "voice".to_string(),
            "say".to_string(),
        ],
    );

    registry.register_deferred(
        Arc::new(AudioGenerateMusicTool::new(
            supervisor.clone(),
            event_bus.clone(),
        )),
        "Audio",
        vec![
            "audio".to_string(),
            "music".to_string(),
            "soundbox".to_string(),
            "generate".to_string(),
        ],
    );

    registry.register_deferred(
        Arc::new(AudioGenerateSfxTool::new(supervisor, event_bus)),
        "Audio",
        vec![
            "audio".to_string(),
            "sfx".to_string(),
            "sound".to_string(),
            "soundbox".to_string(),
            "effects".to_string(),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_tools_registered_as_deferred_under_audio_category() {
        let mut registry = ToolRegistry::new();
        register_audio_tools(&mut registry, None, None);

        assert!(registry.active_definitions().is_empty());
        assert_eq!(registry.deferred_count(), 4);

        let hits = registry.search_deferred("music", Some("Audio"));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "audio_generate_music");
    }
}
