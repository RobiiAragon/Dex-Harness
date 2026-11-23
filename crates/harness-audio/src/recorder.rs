use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use harness_core::{DexEvent, EventBus};
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use tracing::{debug, info, warn};

use crate::error::AudioError;

pub const TARGET_SAMPLE_RATE: u32 = 16_000;
pub const TARGET_CHANNELS: u16 = 1;
pub const DEFAULT_RECORDINGS_DIR: &str = ".dex/media/audio/recordings";

/// Outcome metadata of a finished microphone capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingResult {
    pub path: PathBuf,
    pub duration_secs: f64,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Thread-safe handle to an in-flight native microphone capture running on a dedicated OS audio thread.
pub struct VoiceRecorderHandle {
    stop_flag: Arc<AtomicBool>,
    result_rx: AsyncMutex<Option<oneshot::Receiver<Result<RecordingResult, AudioError>>>>,
}

impl VoiceRecorderHandle {
    /// Starts capturing from the default system input device into a 16kHz mono PCM WAV file.
    pub async fn start(
        output_dir: impl AsRef<Path>,
        event_bus: Option<EventBus>,
    ) -> Result<Self, AudioError> {
        let dir = output_dir.as_ref().to_path_buf();
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| AudioError::Io {
                path: dir.to_string_lossy().to_string(),
                source: e,
            })?;

        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let out_path = dir.join(format!("rec_{ts_ms}.wav"));

        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop_flag);

        let (init_tx, init_rx) = oneshot::channel::<Result<(), AudioError>>();
        let (done_tx, done_rx) = oneshot::channel::<Result<RecordingResult, AudioError>>();

        std::thread::Builder::new()
            .name("dex-audio-capture".to_string())
            .spawn(move || {
                run_capture_thread(out_path, stop_for_thread, event_bus, init_tx, done_tx);
            })
            .map_err(|e| AudioError::Recording {
                message: format!("Failed to spawn audio capture thread: {e}"),
            })?;

        // Wait for the audio thread to acquire the input device and start streaming
        match tokio::time::timeout(Duration::from_secs(5), init_rx).await {
            Ok(Ok(Ok(()))) => Ok(Self {
                stop_flag,
                result_rx: AsyncMutex::new(Some(done_rx)),
            }),
            Ok(Ok(Err(err))) => Err(err),
            Ok(Err(_)) => Err(AudioError::Recording {
                message: "Audio capture thread terminated before initialization".to_string(),
            }),
            Err(_) => {
                stop_flag.store(true, Ordering::SeqCst);
                Err(AudioError::Timeout {
                    operation: "microphone initialization".to_string(),
                    timeout_secs: 5,
                })
            }
        }
    }

    /// Signals the audio thread to stop, flushes and finalizes the WAV file, and returns metadata.
    pub async fn stop(&self) -> Result<RecordingResult, AudioError> {
        self.stop_flag.store(true, Ordering::SeqCst);
        let rx_opt = {
            let mut guard = self.result_rx.lock().await;
            guard.take()
        };

        let rx = rx_opt.ok_or_else(|| AudioError::Recording {
            message: "Recording was already stopped".to_string(),
        })?;

        match tokio::time::timeout(Duration::from_secs(10), rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err(AudioError::Recording {
                message: "Audio capture thread exited without returning a result".to_string(),
            }),
            Err(_) => Err(AudioError::Timeout {
                operation: "microphone stop & WAV finalization".to_string(),
                timeout_secs: 10,
            }),
        }
    }
}

/// Records audio for a fixed number of seconds and writes a 16kHz mono PCM WAV file.
pub async fn record_fixed_duration(
    seconds: u64,
    output_dir: impl AsRef<Path>,
    event_bus: Option<EventBus>,
) -> Result<RecordingResult, AudioError> {
    if seconds == 0 {
        return Err(AudioError::InvalidInput {
            reason: "Recording duration must be at least 1 second".to_string(),
        });
    }

    let handle = VoiceRecorderHandle::start(output_dir, event_bus).await?;
    tokio::time::sleep(Duration::from_secs(seconds)).await;
    handle.stop().await
}

fn run_capture_thread(
    out_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
    event_bus: Option<EventBus>,
    init_tx: oneshot::Sender<Result<(), AudioError>>,
    done_tx: oneshot::Sender<Result<RecordingResult, AudioError>>,
) {
    let host = cpal::default_host();
    let device = match host.default_input_device() {
        Some(dev) => dev,
        None => {
            let _ = init_tx.send(Err(AudioError::Device {
                message: "No default audio input device (microphone) found".to_string(),
            }));
            return;
        }
    };

    let config = match device.default_input_config() {
        Ok(cfg) => cfg,
        Err(e) => {
            let _ = init_tx.send(Err(AudioError::Device {
                message: format!("Failed to query default microphone config: {e}"),
            }));
            return;
        }
    };

    let src_channels = usize::from(config.channels()).max(1);
    let src_sample_rate = config.sample_rate().0.max(1);
    let sample_format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();

    debug!(
        channels = src_channels,
        sample_rate = src_sample_rate,
        format = ?sample_format,
        "Opening native microphone input stream"
    );

    let captured = Arc::new(StdMutex::new(Vec::<f32>::new()));
    let captured_cb = Arc::clone(&captured);
    let last_emit = Arc::new(StdMutex::new(Instant::now()));
    let last_emit_cb = Arc::clone(&last_emit);
    let bus_cb = event_bus.clone();

    let err_fn = |err: cpal::StreamError| {
        warn!(error = %err, "Audio input stream callback reported error");
    };

    let stream_res = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                push_samples_and_emit(data, &captured_cb, &last_emit_cb, bus_cb.as_ref());
            },
            err_fn,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let floats: Vec<f32> = data.iter().map(|&s| f32::from(s) / 32768.0).collect();
                push_samples_and_emit(&floats, &captured_cb, &last_emit_cb, bus_cb.as_ref());
            },
            err_fn,
            None,
        ),
        SampleFormat::U16 => device.build_input_stream(
            &stream_config,
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                let floats: Vec<f32> = data
                    .iter()
                    .map(|&s| (f32::from(s) - 32768.0) / 32768.0)
                    .collect();
                push_samples_and_emit(&floats, &captured_cb, &last_emit_cb, bus_cb.as_ref());
            },
            err_fn,
            None,
        ),
        other => {
            let _ = init_tx.send(Err(AudioError::Device {
                message: format!("Unsupported microphone sample format: {other:?}"),
            }));
            return;
        }
    };

    let stream = match stream_res {
        Ok(s) => s,
        Err(e) => {
            let _ = init_tx.send(Err(AudioError::Device {
                message: format!("Failed to build audio input stream: {e}"),
            }));
            return;
        }
    };

    if let Err(e) = stream.play() {
        let _ = init_tx.send(Err(AudioError::Device {
            message: format!("Failed to start microphone capture: {e}"),
        }));
        return;
    }

    if let Some(ref bus) = event_bus {
        bus.publish(DexEvent::VoiceCaptureState {
            recording: true,
            level_db: -60.0,
        });
    }

    let _ = init_tx.send(Ok(()));

    // Park capture thread lightly until stop is signaled
    while !stop_flag.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(30));
    }

    // Stop and release audio stream before encoding WAV
    drop(stream);

    if let Some(ref bus) = event_bus {
        bus.publish(DexEvent::VoiceCaptureState {
            recording: false,
            level_db: -60.0,
        });
    }

    let raw_samples = {
        let mut guard = captured.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *guard)
    };

    let pcm16 = downmix_and_resample_to_16k_mono(&raw_samples, src_channels, src_sample_rate);
    let outcome = write_pcm16_wav(&out_path, &pcm16, TARGET_SAMPLE_RATE).map(|duration_secs| {
        info!(
            path = ?out_path,
            duration_secs = %format!("{:.2}", duration_secs),
            "Saved microphone WAV recording"
        );
        if let Some(ref bus) = event_bus {
            bus.publish(DexEvent::MediaGenerated {
                kind: "recording".to_string(),
                path: out_path.to_string_lossy().to_string(),
                prompt: "Voice microphone capture".to_string(),
                duration_secs: Some(duration_secs),
            });
        }
        RecordingResult {
            path: out_path,
            duration_secs,
            sample_rate: TARGET_SAMPLE_RATE,
            channels: TARGET_CHANNELS,
        }
    });

    let _ = done_tx.send(outcome);
}

fn push_samples_and_emit(
    samples: &[f32],
    captured: &StdMutex<Vec<f32>>,
    last_emit: &StdMutex<Instant>,
    event_bus: Option<&EventBus>,
) {
    {
        let mut buf = captured.lock().unwrap_or_else(|e| e.into_inner());
        buf.extend_from_slice(samples);
    }

    if let Some(bus) = event_bus {
        let mut ts = last_emit.lock().unwrap_or_else(|e| e.into_inner());
        if ts.elapsed() >= Duration::from_millis(120) {
            let level_db = compute_rms_db(samples);
            bus.publish(DexEvent::VoiceCaptureState {
                recording: true,
                level_db,
            });
            *ts = Instant::now();
        }
    }
}

/// Computes root-mean-square volume in decibels clamped to `[-60.0, 0.0]`.
pub fn compute_rms_db(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return -60.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    let rms = (sum_sq / samples.len() as f64).sqrt();
    if rms <= 1e-6 {
        -60.0
    } else {
        let db = 20.0 * rms.log10();
        (db as f32).clamp(-60.0, 0.0)
    }
}

/// Converts arbitrary multi-channel interleaved float audio at `src_rate` into 16kHz mono PCM16.
pub fn downmix_and_resample_to_16k_mono(
    interleaved: &[f32],
    src_channels: usize,
    src_rate: u32,
) -> Vec<i16> {
    let ch = src_channels.max(1);
    let frames = interleaved.len() / ch;
    if frames == 0 || src_rate == 0 {
        return Vec::new();
    }

    // Step 1: downmix to mono
    let mut mono = Vec::with_capacity(frames);
    for frame_idx in 0..frames {
        let base = frame_idx * ch;
        let mut acc = 0.0f32;
        for c in 0..ch {
            acc += interleaved[base + c];
        }
        mono.push(acc / ch as f32);
    }

    // Step 2: linear resample to TARGET_SAMPLE_RATE (16kHz)
    let out_len = if src_rate == TARGET_SAMPLE_RATE {
        mono.len()
    } else {
        ((mono.len() as f64) * (f64::from(TARGET_SAMPLE_RATE) / f64::from(src_rate))).round()
            as usize
    };
    if out_len == 0 {
        return Vec::new();
    }

    let ratio = f64::from(src_rate) / f64::from(TARGET_SAMPLE_RATE);
    let mut pcm16 = Vec::with_capacity(out_len);

    for i in 0..out_len {
        let sample = if src_rate == TARGET_SAMPLE_RATE {
            mono[i]
        } else {
            let src_pos = (i as f64) * ratio;
            let idx0 = (src_pos.floor() as usize).min(mono.len().saturating_sub(1));
            let idx1 = (idx0 + 1).min(mono.len().saturating_sub(1));
            let frac = (src_pos - idx0 as f64) as f32;
            mono[idx0] * (1.0 - frac) + mono[idx1] * frac
        };
        let clamped = sample.clamp(-1.0, 1.0);
        pcm16.push((clamped * 32767.0).round() as i16);
    }

    pcm16
}

/// Writes mono 16-bit PCM samples to a WAV file and returns its duration in seconds.
pub fn write_pcm16_wav(
    path: &Path,
    samples: &[i16],
    sample_rate: u32,
) -> Result<f64, AudioError> {
    let spec = hound::WavSpec {
        channels: TARGET_CHANNELS,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = hound::WavWriter::create(path, spec).map_err(|e| AudioError::Wav {
        path: path.to_string_lossy().to_string(),
        message: e.to_string(),
    })?;

    for &s in samples {
        writer.write_sample(s).map_err(|e| AudioError::Wav {
            path: path.to_string_lossy().to_string(),
            message: e.to_string(),
        })?;
    }

    writer.finalize().map_err(|e| AudioError::Wav {
        path: path.to_string_lossy().to_string(),
        message: e.to_string(),
    })?;

    let duration_secs = if sample_rate > 0 {
        samples.len() as f64 / f64::from(sample_rate)
    } else {
        0.0
    };
    Ok((duration_secs * 100.0).round() / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_rms_db_silence_and_full_scale() {
        assert_eq!(compute_rms_db(&[]), -60.0);
        assert_eq!(compute_rms_db(&[0.0; 128]), -60.0);
        let loud = compute_rms_db(&[1.0; 128]);
        assert!((loud - 0.0).abs() < 0.01);
        let mid = compute_rms_db(&[0.5; 128]);
        assert!(mid < -5.0 && mid > -7.0);
    }

    #[test]
    fn test_downmix_resample_and_wav_roundtrip() {
        // 1 second of 48kHz stereo 440Hz sine wave -> 16kHz mono WAV
        let src_rate = 48_000u32;
        let frames = src_rate as usize;
        let mut stereo = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let t = i as f32 / src_rate as f32;
            let v = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5;
            stereo.push(v);
            stereo.push(v);
        }

        let pcm16 = downmix_and_resample_to_16k_mono(&stereo, 2, src_rate);
        assert_eq!(pcm16.len(), TARGET_SAMPLE_RATE as usize);

        let wav_path =
            std::env::temp_dir().join(format!("dex_audio_rec_test_{}.wav", std::process::id()));
        let duration = write_pcm16_wav(&wav_path, &pcm16, TARGET_SAMPLE_RATE)
            .expect("write_pcm16_wav should succeed");
        assert!((duration - 1.0).abs() < 0.02);

        let reader = hound::WavReader::open(&wav_path).expect("should read WAV back");
        assert_eq!(reader.spec().sample_rate, 16_000);
        assert_eq!(reader.spec().channels, 1);
        assert_eq!(reader.spec().bits_per_sample, 16);

        let _ = std::fs::remove_file(&wav_path);
    }
}
