use std::path::Path;
use std::sync::Arc;
use anyhow::{Context, Result};
use harness_audio::{
    record_fixed_duration, synthesize_speech_with_options, AutoSttEngine, SttOptions, TtsOptions,
    DEFAULT_RECORDINGS_DIR,
};
use harness_core::EventBus;
use harness_extensions::ExtensionSupervisor;

pub async fn record_audio(seconds: u64, event_bus: EventBus) -> Result<()> {
    println!("Recording microphone audio for {seconds}s (16kHz mono PCM WAV)...");
    let result = record_fixed_duration(seconds, DEFAULT_RECORDINGS_DIR, Some(event_bus))
        .await
        .context("Microphone recording failed")?;

    println!("Saved recording to: {}", result.path.display());
    println!(
        "   Duration: {:.2}s | {} Hz | {} channel(s) | Peak: {:.1} dB",
        result.duration_secs, result.sample_rate, result.channels, result.peak_db
    );
    Ok(())
}

pub async fn transcribe_audio(
    path: &str,
    timestamps: bool,
    language: Option<&str>,
    engine: Option<&str>,
    model: Option<&str>,
) -> Result<()> {
    println!(
        "Transcribing '{}' (language: {}, engine: {})...",
        path,
        language.unwrap_or("auto"),
        engine.unwrap_or("auto")
    );
    let opts = SttOptions {
        language: language.map(String::from),
        engine: engine.map(String::from),
        model: model.map(String::from),
        include_timestamps: timestamps,
        preprocess: true,
    };
    let result = AutoSttEngine::transcribe_with_options(Path::new(path), &opts)
        .await
        .context("Audio transcription failed")?;

    let peak_note = result
        .peak_db
        .map(|db| format!(", peak: {db:.1} dB"))
        .unwrap_or_default();
    let lang_note = result
        .language
        .as_deref()
        .map(|l| format!(", lang: {l}"))
        .unwrap_or_default();
    println!(
        "Transcript (backend: {}{lang_note}{peak_note}):\n",
        result.backend
    );
    println!("{}", result.format_display(timestamps));
    Ok(())
}

pub async fn speak_text(
    text: &str,
    voice: Option<&str>,
    language: Option<&str>,
    engine: Option<&str>,
    model: Option<&str>,
    out: Option<&str>,
    event_bus: EventBus,
) -> Result<()> {
    println!("Synthesizing speech...");
    let supervisor = Arc::new(ExtensionSupervisor::new(event_bus.clone()));
    let opts = TtsOptions {
        voice: voice.map(String::from),
        language: language.map(String::from),
        engine: engine.map(String::from),
        model: model.map(String::from),
        output_path: out.map(|p| Path::new(p).to_path_buf()),
        ..Default::default()
    };
    let result = synthesize_speech_with_options(text, opts, Some(supervisor), Some(&event_bus))
        .await
        .context("Speech synthesis failed")?;

    let dur = result
        .duration_secs
        .map(|d| format!("{d:.2}s"))
        .unwrap_or_else(|| "unknown".to_string());
    println!(
        "Speech synthesized (backend: {}, duration: {})",
        result.backend, dur
    );
    println!("   Saved to: {}", result.path.display());
    Ok(())
}
