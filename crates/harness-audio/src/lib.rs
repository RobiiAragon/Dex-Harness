pub mod error;
pub mod generator;
pub mod recorder;
pub mod stt;
pub mod tools;
pub mod tts;
pub mod voices;

pub use error::AudioError;
pub use generator::{
    audiogen_sidecar_manifest, generate_music, generate_sfx, GeneratedAudio, DEFAULT_MUSIC_DIR, DEFAULT_SFX_DIR,
};
pub use recorder::{
    compute_rms_db, downmix_and_resample_to_16k_mono, preprocess_speech_pcm16,
    preprocess_wav_file, record_fixed_duration, write_pcm16_wav, RecordingResult,
    VoiceRecorderHandle, DEFAULT_RECORDINGS_DIR, TARGET_CHANNELS, TARGET_SAMPLE_RATE,
};
pub use stt::{
    build_mlx_whisper_args, build_parakeet_args, normalize_language_code, AutoSttEngine,
    CliWhisperBackend, HttpWhisperBackend, SttBackend, SttOptions, TranscriptSegment,
    TranscriptionResult, WhisperCliKind, DEFAULT_FASTER_WHISPER_MODEL, DEFAULT_PARAKEET_MODEL,
};
pub use tools::{
    register_audio_tools, AudioGenerateMusicArgs, AudioGenerateMusicTool, AudioGenerateSfxArgs,
    AudioGenerateSfxTool, AudioTranscribeArgs, AudioTranscribeTool, AudioTtsArgs, AudioTtsTool,
};
pub use tts::{
    build_mlx_audio_tts_args, resolve_mlx_audio_lang_code, synthesize_speech,
    synthesize_speech_with_options, TtsOptions, TtsResult, DEFAULT_TTS_DIR,
};
pub use voices::{
    audio_settings_path, builtin_audio_catalog, delete_voice_profile, delete_voice_profile_in,
    list_voice_profiles, list_voice_profiles_in, load_audio_settings, load_audio_settings_from,
    save_audio_settings, save_audio_settings_to, save_voice_profile, save_voice_profile_in,
    voices_dir, voices_manifest_path, AudioModelPreset, AudioStudioSettings, ClonedVoiceProfile,
    DEFAULT_MLX_TTS_MODEL, DEFAULT_WHISPER_MODEL,
};
