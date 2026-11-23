use std::path::Path;
use std::sync::Arc;
use anyhow::{Context, Result};
use harness_audio::{
    record_fixed_duration, synthesize_speech, AutoSttEngine, DEFAULT_RECORDINGS_DIR,
};
use harness_core::EventBus;
use harness_extensions::ExtensionSupervisor;

pub async fn record_audio(seconds: u64, event_bus: EventBus) -> Result<()> {
    println!("🎙️ Recording microphone audio for {seconds}s (16kHz mono PCM WAV)...");
    let result = record_fixed_duration(seconds, DEFAULT_RECORDINGS_DIR, Some(event_bus))
        .await
        .context("Microphone recording failed")?;

    println!("✅ Saved recording to: {}", result.path.display());
    println!(
        "   Duration: {:.2}s | {} Hz | {} channel(s)",
        result.duration_secs, result.sample_rate, result.channels
    );
    Ok(())
}

pub async fn transcribe_audio(path: &str, timestamps: bool) -> Result<()> {
    println!("🎧 Transcribing '{}'...", path);
    let result = AutoSttEngine::transcribe(Path::new(path), timestamps)
        .await
        .context("Audio transcription failed")?;

    println!("✅ Transcript (backend: {}):\n", result.backend);
    println!("{}", result.format_display(timestamps));
    Ok(())
}

pub async fn speak_text(
    text: &str,
    voice: Option<&str>,
    out: Option<&str>,
    event_bus: EventBus,
) -> Result<()> {
    println!("🗣️ Synthesizing speech...");
    let supervisor = Arc::new(ExtensionSupervisor::new(event_bus.clone()));
    let out_path = out.map(Path::new);
    let result = synthesize_speech(text, voice, out_path, Some(supervisor), Some(&event_bus))
        .await
        .context("Speech synthesis failed")?;

    let dur = result
        .duration_secs
        .map(|d| format!("{d:.2}s"))
        .unwrap_or_else(|| "unknown".to_string());
    println!("✅ Speech synthesized (backend: {}, duration: {})", result.backend, dur);
    println!("   Saved to: {}", result.path.display());
    Ok(())
}
