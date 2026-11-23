pub mod error;
pub mod generator;
pub mod recorder;
pub mod stt;
pub mod tools;
pub mod tts;

pub use error::AudioError;
pub use generator::{
    generate_music, generate_sfx, GeneratedAudio, DEFAULT_MUSIC_DIR, DEFAULT_SFX_DIR,
};
pub use recorder::{
    compute_rms_db, downmix_and_resample_to_16k_mono, record_fixed_duration, write_pcm16_wav,
    RecordingResult, VoiceRecorderHandle, DEFAULT_RECORDINGS_DIR, TARGET_CHANNELS,
    TARGET_SAMPLE_RATE,
};
pub use stt::{
    AutoSttEngine, CliWhisperBackend, HttpWhisperBackend, SttBackend, TranscriptSegment,
    TranscriptionResult, WhisperCliKind,
};
pub use tools::{
    register_audio_tools, AudioGenerateMusicArgs, AudioGenerateMusicTool, AudioGenerateSfxArgs,
    AudioGenerateSfxTool, AudioTranscribeArgs, AudioTranscribeTool, AudioTtsArgs, AudioTtsTool,
};
pub use tts::{synthesize_speech, TtsResult, DEFAULT_TTS_DIR};
