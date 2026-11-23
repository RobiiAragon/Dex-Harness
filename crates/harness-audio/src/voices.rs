use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};

use crate::error::AudioError;

pub const DEFAULT_WHISPER_MODEL: &str = "mlx-community/whisper-large-v3-turbo";
pub const DEFAULT_MLX_TTS_MODEL: &str = "mlx-community/Qwen3-TTS-12Hz-0.6B-Base-8bit";

/// Persisted Voice & Audio Studio configuration (`~/.dex/audio_settings.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioStudioSettings {
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_auto")]
    pub asr_engine: String,
    #[serde(default = "default_asr_model")]
    pub asr_model: String,
    #[serde(default = "default_auto")]
    pub tts_engine: String,
    #[serde(default = "default_tts_model")]
    pub tts_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_sidecar_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_voice_id: Option<String>,
}

fn default_language() -> String {
    "auto".to_string()
}

fn default_auto() -> String {
    "auto".to_string()
}

fn default_asr_model() -> String {
    DEFAULT_WHISPER_MODEL.to_string()
}

fn default_tts_model() -> String {
    DEFAULT_MLX_TTS_MODEL.to_string()
}

impl Default for AudioStudioSettings {
    fn default() -> Self {
        Self {
            language: default_language(),
            asr_engine: default_auto(),
            asr_model: default_asr_model(),
            tts_engine: default_auto(),
            tts_model: default_tts_model(),
            tts_sidecar_url: None,
            active_voice_id: None,
        }
    }
}

/// Saved zero-shot voice cloning profile stored under `~/.dex/voices/`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClonedVoiceProfile {
    pub id: String,
    pub name: String,
    pub language: String,
    pub ref_audio_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_text: Option<String>,
    pub created_at: u64,
}

/// Curated ASR, TTS, and external sidecar audio model entry for Audio Studio.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioModelPreset {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub engine: String,
    pub repo_id: String,
    pub runtime_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    pub languages: String,
    pub supports_cloning: bool,
    pub description: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct VoicesStoreFile {
    #[serde(default)]
    profiles: Vec<ClonedVoiceProfile>,
}

pub fn voices_dir() -> PathBuf {
    harness_core::home_dex_dir().join("voices")
}

pub fn voices_manifest_path() -> PathBuf {
    voices_dir().join("voices.json")
}

pub fn audio_settings_path() -> PathBuf {
    harness_core::home_dex_dir().join("audio_settings.json")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn slugify(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let clean = out.trim_matches('-').to_string();
    if clean.is_empty() {
        format!("voice-{}", now_secs())
    } else {
        clean
    }
}

pub async fn load_audio_settings() -> AudioStudioSettings {
    load_audio_settings_from(&audio_settings_path()).await
}

pub async fn load_audio_settings_from(path: &Path) -> AudioStudioSettings {
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        return AudioStudioSettings::default();
    }
    match tokio::fs::read_to_string(path).await {
        Ok(raw) => serde_json::from_str::<AudioStudioSettings>(&raw).unwrap_or_default(),
        Err(_) => AudioStudioSettings::default(),
    }
}

pub async fn save_audio_settings(settings: &AudioStudioSettings) -> Result<(), AudioError> {
    save_audio_settings_to(&audio_settings_path(), settings).await
}

pub async fn save_audio_settings_to(
    path: &Path,
    settings: &AudioStudioSettings,
) -> Result<(), AudioError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| AudioError::Io {
                path: parent.to_string_lossy().to_string(),
                source: e,
            })?;
    }
    let json = serde_json::to_string_pretty(settings).map_err(|e| AudioError::InvalidInput {
        reason: format!("Failed to serialize audio settings: {e}"),
    })?;
    tokio::fs::write(path, json)
        .await
        .map_err(|e| AudioError::Io {
            path: path.to_string_lossy().to_string(),
            source: e,
        })
}

pub async fn list_voice_profiles() -> Vec<ClonedVoiceProfile> {
    list_voice_profiles_in(&voices_dir()).await
}

pub async fn list_voice_profiles_in(dir: &Path) -> Vec<ClonedVoiceProfile> {
    let manifest = dir.join("voices.json");
    if !tokio::fs::try_exists(&manifest).await.unwrap_or(false) {
        return Vec::new();
    }
    match tokio::fs::read_to_string(&manifest).await {
        Ok(raw) => serde_json::from_str::<VoicesStoreFile>(&raw)
            .map(|s| s.profiles)
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

pub async fn save_voice_profile(
    name: &str,
    language: &str,
    source_audio_path: &Path,
    ref_text: Option<&str>,
) -> Result<ClonedVoiceProfile, AudioError> {
    save_voice_profile_in(&voices_dir(), name, language, source_audio_path, ref_text).await
}

pub async fn save_voice_profile_in(
    dir: &Path,
    name: &str,
    language: &str,
    source_audio_path: &Path,
    ref_text: Option<&str>,
) -> Result<ClonedVoiceProfile, AudioError> {
    let clean_name = name.trim();
    if clean_name.is_empty() {
        return Err(AudioError::InvalidInput {
            reason: "Voice profile name cannot be empty".to_string(),
        });
    }
    if !tokio::fs::try_exists(source_audio_path)
        .await
        .unwrap_or(false)
    {
        return Err(AudioError::InvalidInput {
            reason: format!(
                "Reference audio file does not exist: '{}'",
                source_audio_path.display()
            ),
        });
    }

    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| AudioError::Io {
            path: dir.to_string_lossy().to_string(),
            source: e,
        })?;

    let mut profiles = list_voice_profiles_in(dir).await;
    let base_id = slugify(clean_name);
    let mut id = base_id.clone();
    let mut n = 2usize;
    while profiles.iter().any(|p| p.id == id) {
        id = format!("{base_id}-{n}");
        n += 1;
    }

    let ext = source_audio_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("wav")
        .to_ascii_lowercase();
    let dest_audio = dir.join(format!("{id}.{ext}"));

    tokio::fs::copy(source_audio_path, &dest_audio)
        .await
        .map_err(|e| AudioError::Io {
            path: dest_audio.to_string_lossy().to_string(),
            source: e,
        })?;

    let lang = match language.trim().to_lowercase().as_str() {
        "es" | "spanish" | "español" => "es".to_string(),
        "en" | "english" | "inglés" => "en".to_string(),
        other if !other.is_empty() => other.to_string(),
        _ => "auto".to_string(),
    };

    let profile = ClonedVoiceProfile {
        id,
        name: clean_name.to_string(),
        language: lang,
        ref_audio_path: dest_audio.to_string_lossy().to_string(),
        ref_text: ref_text
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from),
        created_at: now_secs(),
    };

    profiles.insert(0, profile.clone());
    let store = VoicesStoreFile { profiles };
    let manifest = dir.join("voices.json");
    let json = serde_json::to_string_pretty(&store).map_err(|e| AudioError::InvalidInput {
        reason: format!("Failed to serialize voice profiles: {e}"),
    })?;
    tokio::fs::write(&manifest, json)
        .await
        .map_err(|e| AudioError::Io {
            path: manifest.to_string_lossy().to_string(),
            source: e,
        })?;

    Ok(profile)
}

pub async fn delete_voice_profile(id: &str) -> Result<bool, AudioError> {
    delete_voice_profile_in(&voices_dir(), id).await
}

pub async fn delete_voice_profile_in(dir: &Path, id: &str) -> Result<bool, AudioError> {
    let mut profiles = list_voice_profiles_in(dir).await;
    let mut removed_audio: Option<PathBuf> = None;
    let before = profiles.len();
    profiles.retain(|p| {
        if p.id == id {
            removed_audio = Some(PathBuf::from(&p.ref_audio_path));
            false
        } else {
            true
        }
    });
    if profiles.len() == before {
        return Ok(false);
    }

    if let Some(audio) = removed_audio {
        if audio.starts_with(dir) {
            let _ = tokio::fs::remove_file(audio).await;
        }
    }

    let manifest = dir.join("voices.json");
    let store = VoicesStoreFile { profiles };
    let json = serde_json::to_string_pretty(&store).map_err(|e| AudioError::InvalidInput {
        reason: format!("Failed to serialize voice profiles: {e}"),
    })?;
    tokio::fs::write(&manifest, json)
        .await
        .map_err(|e| AudioError::Io {
            path: manifest.to_string_lossy().to_string(),
            source: e,
        })?;
    Ok(true)
}

/// Curated catalog of ASR, MLX TTS, and VoiceStudio-inspired HTTP/MCP sidecar presets.
pub fn builtin_audio_catalog() -> Vec<AudioModelPreset> {
    vec![
        // --- ASR Engines ---
        AudioModelPreset {
            id: "mlx-whisper-turbo".to_string(),
            kind: "asr".to_string(),
            name: "MLX Whisper large-v3-turbo".to_string(),
            engine: "mlx-whisper".to_string(),
            repo_id: "mlx-community/whisper-large-v3-turbo".to_string(),
            runtime_id: "mlx-whisper".to_string(),
            endpoint: None,
            languages: "99 languages (excellent Spanish & noisy audio)".to_string(),
            supports_cloning: false,
            description: "High-accuracy multilingual Whisper distilled for Apple Silicon MLX.".to_string(),
        },
        AudioModelPreset {
            id: "parakeet-tdt-v3".to_string(),
            kind: "asr".to_string(),
            name: "Parakeet TDT v3 (MLX)".to_string(),
            engine: "parakeet-mlx".to_string(),
            repo_id: "mlx-community/parakeet-tdt-0.6b-v3".to_string(),
            runtime_id: "parakeet-mlx".to_string(),
            endpoint: None,
            languages: "25 European languages (ES, EN, FR, DE, IT, PT...)".to_string(),
            supports_cloning: false,
            description: "NVIDIA Parakeet 0.6B TDT v3 accelerated on Apple Silicon MLX (ultra-fast low-latency ASR).".to_string(),
        },
        AudioModelPreset {
            id: "faster-whisper-large-v3".to_string(),
            kind: "asr".to_string(),
            name: "Faster-Whisper Large v3".to_string(),
            engine: "faster-whisper".to_string(),
            repo_id: "Systran/faster-whisper-large-v3".to_string(),
            runtime_id: "faster-whisper".to_string(),
            endpoint: None,
            languages: "99 languages (CTranslate2 CPU fallback + Silero VAD)".to_string(),
            supports_cloning: false,
            description: "Cross-platform CTranslate2 Whisper engine with built-in VAD filtering.".to_string(),
        },
        // --- Native Apple Silicon MLX TTS & Voice Cloning (`mlx-audio`) ---
        AudioModelPreset {
            id: "qwen3-tts-base".to_string(),
            kind: "tts".to_string(),
            name: "Qwen3-TTS 0.6B Base (Voice Cloning · MLX)".to_string(),
            engine: "mlx-audio".to_string(),
            repo_id: "mlx-community/Qwen3-TTS-12Hz-0.6B-Base-8bit".to_string(),
            runtime_id: "mlx-audio".to_string(),
            endpoint: None,
            languages: "Multilingual (ES, EN, ZH, JA, KO, FR, DE...)".to_string(),
            supports_cloning: true,
            description: "Zero-shot voice cloning & natural multilingual synthesis running natively on Apple Silicon via mlx-audio.".to_string(),
        },
        AudioModelPreset {
            id: "qwen3-tts-custom".to_string(),
            kind: "tts".to_string(),
            name: "Qwen3-TTS 0.6B CustomVoice (Presets · MLX)".to_string(),
            engine: "mlx-audio".to_string(),
            repo_id: "mlx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice-8bit".to_string(),
            runtime_id: "mlx-audio".to_string(),
            endpoint: None,
            languages: "Multilingual (Vivian, Serena, Ryan, Aiden...)".to_string(),
            supports_cloning: false,
            description: "Curated studio voices with multilingual prosody via mlx-audio.".to_string(),
        },
        AudioModelPreset {
            id: "chatterbox-tts".to_string(),
            kind: "tts".to_string(),
            name: "Chatterbox TTS (Fast Cloning · MLX)".to_string(),
            engine: "mlx-audio".to_string(),
            repo_id: "mlx-community/chatterbox-turbo-fp16".to_string(),
            runtime_id: "mlx-audio".to_string(),
            endpoint: None,
            languages: "Multilingual zero-shot voice cloning".to_string(),
            supports_cloning: true,
            description: "Expressive low-latency Resemble Chatterbox neural TTS & cloning via mlx-audio.".to_string(),
        },
        AudioModelPreset {
            id: "kokoro-82m".to_string(),
            kind: "tts".to_string(),
            name: "Kokoro 82M (Ultra-Light · MLX)".to_string(),
            engine: "mlx-audio".to_string(),
            repo_id: "mlx-community/Kokoro-82M-bf16".to_string(),
            runtime_id: "mlx-audio".to_string(),
            endpoint: None,
            languages: "EN, ES, FR, JA, ZH, IT, PT".to_string(),
            supports_cloning: false,
            description: "Compact 82M parameter neural TTS with crisp studio quality and tiny RAM footprint.".to_string(),
        },
        AudioModelPreset {
            id: "macos-say".to_string(),
            kind: "tts".to_string(),
            name: "macOS Native Say (Instant Built-In)".to_string(),
            engine: "macos-say".to_string(),
            repo_id: "apple/macos-say".to_string(),
            runtime_id: "builtin".to_string(),
            endpoint: None,
            languages: "System installed voices (Monica, Paulina, Samantha...)".to_string(),
            supports_cloning: false,
            description: "Zero-download instant speech synthesis using macOS built-in speech voices.".to_string(),
        },
        // --- Configurable External Sidecar / HTTP-MCP Presets (VoiceStudio Ecosystem) ---
        AudioModelPreset {
            id: "fish-audio-s2-pro".to_string(),
            kind: "sidecar".to_string(),
            name: "Fish Audio S2-Pro (Sidecar)".to_string(),
            engine: "sidecar".to_string(),
            repo_id: "fishaudio/s2-pro".to_string(),
            runtime_id: "fish-audio-s2-pro".to_string(),
            endpoint: Some("http://127.0.0.1:8080/v1/audio/speech".to_string()),
            languages: "Multilingual + expressive zero-shot cloning".to_string(),
            supports_cloning: true,
            description: "Connect to a local Fish Speech S2-Pro API server (fishaudio/fish-speech) on :8080.".to_string(),
        },
        AudioModelPreset {
            id: "step-audio-editx".to_string(),
            kind: "sidecar".to_string(),
            name: "Step-Audio EditX (Sidecar)".to_string(),
            engine: "sidecar".to_string(),
            repo_id: "stepfun-ai/Step-Audio".to_string(),
            runtime_id: "step-audio-editx".to_string(),
            endpoint: Some("http://127.0.0.1:8192/v1/audio/speech".to_string()),
            languages: "Expressive speech editing & cloning".to_string(),
            supports_cloning: true,
            description: "Connect to a local Step-Audio EditX server on :8192.".to_string(),
        },
        AudioModelPreset {
            id: "omnivoice".to_string(),
            kind: "sidecar".to_string(),
            name: "OmniVoice k2-fsa (Sidecar)".to_string(),
            engine: "sidecar".to_string(),
            repo_id: "k2-fsa/OmniVoice".to_string(),
            runtime_id: "omnivoice-tts".to_string(),
            endpoint: Some("http://127.0.0.1:8193/v1/audio/speech".to_string()),
            languages: "600+ languages (VoiceStudio default engine)".to_string(),
            supports_cloning: true,
            description: "Connect to a local k2-fsa/OmniVoice diffusion TTS & cloning server on :8193.".to_string(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_audio_settings_and_voice_profiles_crud() {
        let base = std::env::temp_dir().join(format!("dex_voices_test_{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&base).await;
        tokio::fs::create_dir_all(&base).await.expect("mkdir");

        let settings_file = base.join("audio_settings.json");
        let mut s = load_audio_settings_from(&settings_file).await;
        assert_eq!(s.language, "auto");
        s.language = "es".to_string();
        s.asr_engine = "mlx-whisper".to_string();
        save_audio_settings_to(&settings_file, &s)
            .await
            .expect("save settings");
        let loaded = load_audio_settings_from(&settings_file).await;
        assert_eq!(loaded.language, "es");
        assert_eq!(loaded.asr_engine, "mlx-whisper");

        let sample_wav = base.join("sample.wav");
        tokio::fs::write(&sample_wav, b"RIFFfakewav")
            .await
            .expect("write wav");
        let vdir = base.join("voices");
        let prof = save_voice_profile_in(
            &vdir,
            "Robii - Español",
            "es",
            &sample_wav,
            Some("Hola mundo de prueba"),
        )
        .await
        .expect("save profile");
        assert_eq!(prof.language, "es");
        assert!(Path::new(&prof.ref_audio_path).is_file());

        let list = list_voice_profiles_in(&vdir).await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Robii - Español");

        assert!(delete_voice_profile_in(&vdir, &prof.id)
            .await
            .expect("delete profile"));
        assert!(list_voice_profiles_in(&vdir).await.is_empty());

        let _ = tokio::fs::remove_dir_all(&base).await;
    }
}
