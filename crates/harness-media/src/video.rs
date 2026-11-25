use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use harness_core::{DexEvent, EventBus};
use harness_extensions::ExtensionSupervisor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::{debug, info};

use crate::comfyui::{
    inject_workflow_placeholders, load_workflow_template, ComfyUiClient, DEFAULT_WORKFLOWS_DIR,
};
use crate::error::MediaError;
use crate::image_gen::ImageBackend;

pub const DEFAULT_VIDEO_DIR: &str = ".dex/media/video";
pub const DEFAULT_FRAMES_DIR: &str = ".dex/media/frames";

const FFPROBE_TIMEOUT_SECS: u64 = 30;
const FFMPEG_TIMEOUT_SECS: u64 = 300;
const VIDEO_GEN_TIMEOUT_SECS: u64 = 600;

/// Metadata extracted from a video file via `ffprobe`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoMetadata {
    pub path: String,
    pub duration_secs: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub bitrate: Option<u64>,
    pub size_bytes: Option<u64>,
    pub format_name: Option<String>,
}

/// A single source video clip segment in a declarative timeline.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VideoClip {
    /// Path to the source video file.
    pub path: String,
    /// Optional start trim timestamp in seconds.
    #[serde(default)]
    pub start_secs: Option<f64>,
    /// Optional end trim timestamp in seconds.
    #[serde(default)]
    pub end_secs: Option<f64>,
}

/// Declarative video editing timeline translated safely into `ffmpeg` CLI arguments.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TimelineSpec {
    /// Ordered list of video clips to trim and concatenate.
    pub clips: Vec<VideoClip>,
    /// Optional external audio track (.wav, .mp3, .m4a) to overlay onto the final render.
    #[serde(default)]
    pub audio_track: Option<String>,
    /// Optional path to an `.srt` subtitle file to burn into the output video.
    #[serde(default)]
    pub subtitles_srt: Option<String>,
    /// Optional target output width in pixels.
    #[serde(default)]
    pub width: Option<u32>,
    /// Optional target output height in pixels.
    #[serde(default)]
    pub height: Option<u32>,
    /// Optional target output frame rate (frames per second).
    #[serde(default)]
    pub fps: Option<f64>,
    /// Target output container format: `mp4`, `mov`, `webm`, `mkv`, or `gif`.
    #[serde(default)]
    pub output_format: Option<String>,
}

impl TimelineSpec {
    /// Validates all fields of the timeline before building `ffmpeg` commands or running processes.
    pub fn validate(&self) -> Result<(), MediaError> {
        if self.clips.is_empty() {
            return Err(MediaError::InvalidInput {
                reason: "TimelineSpec must contain at least one video clip".to_string(),
            });
        }

        for (idx, clip) in self.clips.iter().enumerate() {
            validate_safe_path_str(&clip.path, &format!("clips[{idx}].path"))?;

            if let Some(start) = clip.start_secs {
                if !start.is_finite() || start < 0.0 {
                    return Err(MediaError::InvalidInput {
                        reason: format!(
                            "clips[{idx}].start_secs must be finite and >= 0.0 (got {start})"
                        ),
                    });
                }
            }
            if let Some(end) = clip.end_secs {
                if !end.is_finite() || end <= 0.0 {
                    return Err(MediaError::InvalidInput {
                        reason: format!(
                            "clips[{idx}].end_secs must be finite and > 0.0 (got {end})"
                        ),
                    });
                }
            }
            if let (Some(start), Some(end)) = (clip.start_secs, clip.end_secs) {
                if end <= start {
                    return Err(MediaError::InvalidInput {
                        reason: format!(
                            "clips[{idx}].end_secs ({end}) must be greater than start_secs ({start})"
                        ),
                    });
                }
            }
        }

        if let Some(ref audio) = self.audio_track {
            validate_safe_path_str(audio, "audio_track")?;
        }
        if let Some(ref srt) = self.subtitles_srt {
            validate_safe_path_str(srt, "subtitles_srt")?;
        }

        if let Some(w) = self.width {
            if w == 0 || w > 16_384 {
                return Err(MediaError::InvalidInput {
                    reason: format!("width must be between 1 and 16384 (got {w})"),
                });
            }
        }
        if let Some(h) = self.height {
            if h == 0 || h > 16_384 {
                return Err(MediaError::InvalidInput {
                    reason: format!("height must be between 1 and 16384 (got {h})"),
                });
            }
        }
        if let Some(fps) = self.fps {
            if !fps.is_finite() || fps <= 0.0 || fps > 240.0 {
                return Err(MediaError::InvalidInput {
                    reason: format!("fps must be finite and in (0, 240] (got {fps})"),
                });
            }
        }

        if let Some(ref fmt) = self.output_format {
            let normalized = fmt.trim().to_lowercase();
            if !matches!(normalized.as_str(), "mp4" | "mov" | "webm" | "mkv" | "gif") {
                return Err(MediaError::InvalidInput {
                    reason: format!(
                        "Unsupported output_format '{fmt}'. Supported: mp4, mov, webm, mkv, gif"
                    ),
                });
            }
        }

        Ok(())
    }

    pub fn resolved_format(&self, output_path: Option<&Path>) -> String {
        if let Some(ref fmt) = self.output_format {
            let clean = fmt.trim().to_lowercase();
            if !clean.is_empty() {
                return clean;
            }
        }
        if let Some(ext) = output_path
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
        {
            let lower = ext.to_lowercase();
            if matches!(lower.as_str(), "mp4" | "mov" | "webm" | "mkv" | "gif") {
                return lower;
            }
        }
        "mp4".to_string()
    }
}

/// Request specification for generating a video clip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoGenRequest {
    pub prompt: String,
    pub image_path: Option<String>,
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    pub seed: Option<u64>,
    pub duration_secs: Option<f64>,
    pub backend: ImageBackend,
    pub workflow: Option<String>,
}

impl Default for VideoGenRequest {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            image_path: None,
            width: 768,
            height: 512,
            steps: 24,
            seed: None,
            duration_secs: Some(4.0),
            backend: ImageBackend::Auto,
            workflow: None,
        }
    }
}

/// Outcome of a video generation operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoGenResult {
    pub path: PathBuf,
    pub backend: String,
    pub prompt: String,
    pub duration_secs: Option<f64>,
}

/// Outcome of frame extraction from a video.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedFramesResult {
    pub output_dir: PathBuf,
    pub frames: Vec<PathBuf>,
}

/// Outcome of rendering a declarative `TimelineSpec`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoEditResult {
    pub output_path: PathBuf,
    pub size_bytes: u64,
    pub format: String,
    pub clips_count: usize,
}

fn validate_safe_path_str(raw: &str, field: &str) -> Result<(), MediaError> {
    if raw.trim().is_empty() {
        return Err(MediaError::InvalidInput {
            reason: format!("Field '{field}' cannot be empty"),
        });
    }
    if raw.contains('\0') || raw.contains('\n') || raw.contains('\r') {
        return Err(MediaError::InvalidInput {
            reason: format!("Field '{field}' contains illegal control characters"),
        });
    }
    Ok(())
}

fn current_epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn escape_ffmpeg_filter_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    for ch in path.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ':' => out.push_str("\\:"),
            '\'' => out.push_str("\\'"),
            '[' => out.push_str("\\["),
            ']' => out.push_str("\\]"),
            ',' => out.push_str("\\,"),
            ';' => out.push_str("\\;"),
            _ => out.push(ch),
        }
    }
    out
}

fn build_scale_fps_filters(spec: &TimelineSpec) -> Vec<String> {
    let mut filters = Vec::new();
    match (spec.width, spec.height) {
        (Some(w), Some(h)) => {
            filters.push(format!(
                "scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2"
            ));
        }
        (Some(w), None) => {
            filters.push(format!("scale={w}:-2"));
        }
        (None, Some(h)) => {
            filters.push(format!("scale=-2:{h}"));
        }
        (None, None) => {}
    }
    if let Some(fps) = spec.fps {
        filters.push(format!("fps={fps:.3}"));
    }
    filters
}

/// Translates a validated `TimelineSpec` into safe, strictly typed `ffmpeg` CLI arguments
/// without any shell interpolation.
pub fn build_ffmpeg_timeline_args(
    spec: &TimelineSpec,
    output_path: &Path,
) -> Result<Vec<String>, MediaError> {
    spec.validate()?;

    let mut args = vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
    ];

    for clip in &spec.clips {
        if let Some(start) = clip.start_secs {
            args.push("-ss".to_string());
            args.push(format!("{start:.3}"));
        }
        if let Some(end) = clip.end_secs {
            args.push("-to".to_string());
            args.push(format!("{end:.3}"));
        }
        args.push("-i".to_string());
        args.push(clip.path.clone());
    }

    let audio_input_idx = if let Some(ref audio) = spec.audio_track {
        args.push("-i".to_string());
        args.push(audio.clone());
        Some(spec.clips.len())
    } else {
        None
    };

    let format = spec.resolved_format(Some(output_path));
    let is_gif = format == "gif";
    let base_filters = build_scale_fps_filters(spec);

    if spec.clips.len() == 1 {
        let mut chain = base_filters;
        if let Some(ref srt) = spec.subtitles_srt {
            chain.push(format!("subtitles='{}'", escape_ffmpeg_filter_path(srt)));
        }

        if chain.is_empty() {
            args.push("-map".to_string());
            args.push("0:v".to_string());
        } else {
            args.push("-filter_complex".to_string());
            args.push(format!("[0:v]{}[vout]", chain.join(",")));
            args.push("-map".to_string());
            args.push("[vout]".to_string());
        }

        if !is_gif {
            if let Some(a_idx) = audio_input_idx {
                args.push("-map".to_string());
                args.push(format!("{a_idx}:a"));
                args.push("-shortest".to_string());
            } else {
                args.push("-map".to_string());
                args.push("0:a?".to_string());
            }
        }
    } else {
        let mut fc_parts = Vec::new();
        let mut concat_inputs = String::new();

        for idx in 0..spec.clips.len() {
            let mut per_clip = base_filters.clone();
            per_clip.push("setsar=1".to_string());
            fc_parts.push(format!("[{idx}:v]{}[v{idx}]", per_clip.join(",")));
            concat_inputs.push_str(&format!("[v{idx}]"));
        }

        let concat_label = if spec.subtitles_srt.is_some() {
            "vcat"
        } else {
            "vout"
        };
        fc_parts.push(format!(
            "{concat_inputs}concat=n={}:v=1:a=0[{concat_label}]",
            spec.clips.len()
        ));

        if let Some(ref srt) = spec.subtitles_srt {
            fc_parts.push(format!(
                "[vcat]subtitles='{}'[vout]",
                escape_ffmpeg_filter_path(srt)
            ));
        }

        args.push("-filter_complex".to_string());
        args.push(fc_parts.join(";"));
        args.push("-map".to_string());
        args.push("[vout]".to_string());

        if !is_gif {
            if let Some(a_idx) = audio_input_idx {
                args.push("-map".to_string());
                args.push(format!("{a_idx}:a"));
                args.push("-shortest".to_string());
            }
        }
    }

    match format.as_str() {
        "webm" => {
            args.extend([
                "-c:v".to_string(),
                "libvpx-vp9".to_string(),
                "-c:a".to_string(),
                "libopus".to_string(),
            ]);
        }
        "gif" => {
            args.extend(["-loop".to_string(), "0".to_string()]);
        }
        "mp4" | "mov" => {
            args.extend([
                "-c:v".to_string(),
                "libx264".to_string(),
                "-pix_fmt".to_string(),
                "yuv420p".to_string(),
                "-movflags".to_string(),
                "+faststart".to_string(),
                "-c:a".to_string(),
                "aac".to_string(),
            ]);
        }
        _ => {
            args.extend([
                "-c:v".to_string(),
                "libx264".to_string(),
                "-pix_fmt".to_string(),
                "yuv420p".to_string(),
                "-c:a".to_string(),
                "aac".to_string(),
            ]);
        }
    }

    args.push(output_path.to_string_lossy().to_string());
    Ok(args)
}

fn parse_frame_rate(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "0/0" {
        return None;
    }
    if let Some((num_s, den_s)) = trimmed.split_once('/') {
        let num = num_s.parse::<f64>().ok()?;
        let den = den_s.parse::<f64>().ok()?;
        if den > 0.0 && num >= 0.0 {
            return Some(num / den);
        }
        return None;
    }
    trimmed.parse::<f64>().ok().filter(|v| *v > 0.0 && v.is_finite())
}

/// Parses raw JSON output from `ffprobe -v quiet -print_format json -show_format -show_streams`.
pub fn parse_ffprobe_json(raw: &str, path: &Path) -> Result<VideoMetadata, MediaError> {
    let root: serde_json::Value = serde_json::from_str(raw).map_err(|e| MediaError::Json {
        context: format!("parse ffprobe JSON for '{}'", path.display()),
        source: e,
    })?;

    let format = root.get("format");
    let duration_secs = format
        .and_then(|f| f.get("duration"))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok());
    let size_bytes = format
        .and_then(|f| f.get("size"))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok());
    let bitrate = format
        .and_then(|f| f.get("bit_rate"))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok());
    let format_name = format
        .and_then(|f| f.get("format_name"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let mut width = None;
    let mut height = None;
    let mut fps = None;
    let mut video_codec = None;
    let mut audio_codec = None;

    if let Some(streams) = root.get("streams").and_then(|v| v.as_array()) {
        for stream in streams {
            let codec_type = stream
                .get("codec_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if codec_type == "video" && video_codec.is_none() {
                video_codec = stream
                    .get("codec_name")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                width = stream
                    .get("width")
                    .and_then(|v| v.as_u64())
                    .and_then(|w| u32::try_from(w).ok());
                height = stream
                    .get("height")
                    .and_then(|v| v.as_u64())
                    .and_then(|h| u32::try_from(h).ok());
                fps = stream
                    .get("avg_frame_rate")
                    .and_then(|v| v.as_str())
                    .and_then(parse_frame_rate)
                    .or_else(|| {
                        stream
                            .get("r_frame_rate")
                            .and_then(|v| v.as_str())
                            .and_then(parse_frame_rate)
                    });
            } else if codec_type == "audio" && audio_codec.is_none() {
                audio_codec = stream
                    .get("codec_name")
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
        }
    }

    Ok(VideoMetadata {
        path: path.to_string_lossy().to_string(),
        duration_secs,
        width,
        height,
        fps,
        video_codec,
        audio_codec,
        bitrate,
        size_bytes,
        format_name,
    })
}

/// Inspects a video file's duration, resolution, FPS, codecs, and bitrate using `ffprobe`.
pub async fn inspect_video(path: &Path) -> Result<VideoMetadata, MediaError> {
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        return Err(MediaError::InvalidInput {
            reason: format!("Video file does not exist: '{}'", path.display()),
        });
    }

    let mut ffprobe_cmd = Command::new("ffprobe");
    harness_core::apply_enriched_path(&mut ffprobe_cmd);
    let output = tokio::time::timeout(
        Duration::from_secs(FFPROBE_TIMEOUT_SECS),
        ffprobe_cmd
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(path)
            .output(),
    )
    .await
    .map_err(|_| MediaError::Timeout {
        operation: "ffprobe".to_string(),
        timeout_secs: FFPROBE_TIMEOUT_SECS,
    })?
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            MediaError::BackendUnavailable {
                kind: "video".to_string(),
                reason: "`ffprobe` not found in $PATH. Install FFmpeg with 1 click (`brew install ffmpeg`). [dex-install:ffmpeg]"
                    .to_string(),
            }
        } else {
            MediaError::Subprocess {
                command: "ffprobe".to_string(),
                message: e.to_string(),
            }
        }
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(MediaError::Subprocess {
            command: "ffprobe".to_string(),
            message: if stderr.is_empty() {
                format!("Exited with status {}", output.status)
            } else {
                stderr
            },
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_ffprobe_json(&stdout, path)
}

/// Extracts keyframes from a video into PNG files inside `.dex/media/frames/` (or `output_dir`)
/// so the LLM can visually inspect or OCR video content.
pub async fn extract_frames(
    path: &Path,
    fps: Option<f64>,
    timestamps_secs: Option<&[f64]>,
    output_dir: Option<&Path>,
) -> Result<ExtractedFramesResult, MediaError> {
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        return Err(MediaError::InvalidInput {
            reason: format!("Video file does not exist: '{}'", path.display()),
        });
    }

    let dest_dir = match output_dir {
        Some(dir) => harness_core::resolve_safe_path(dir),
        None => {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("video");
            harness_core::resolve_dex_subpath(DEFAULT_FRAMES_DIR)
                .join(format!("{stem}_{}", current_epoch_millis()))
        }
    };

    tokio::fs::create_dir_all(&dest_dir)
        .await
        .map_err(|e| MediaError::Io {
            path: dest_dir.to_string_lossy().to_string(),
            source: e,
        })?;

    if let Some(timestamps) = timestamps_secs.filter(|s| !s.is_empty()) {
        for (idx, &ts) in timestamps.iter().enumerate() {
            if !ts.is_finite() || ts < 0.0 {
                return Err(MediaError::InvalidInput {
                    reason: format!("Invalid timestamp {ts}: must be finite and >= 0.0"),
                });
            }
            let frame_path = dest_dir.join(format!("frame_{:04}.png", idx + 1));
            let args = vec![
                "-y".to_string(),
                "-hide_banner".to_string(),
                "-loglevel".to_string(),
                "error".to_string(),
                "-ss".to_string(),
                format!("{ts:.3}"),
                "-i".to_string(),
                path.to_string_lossy().to_string(),
                "-frames:v".to_string(),
                "1".to_string(),
                frame_path.to_string_lossy().to_string(),
            ];
            run_ffmpeg(&args, "ffmpeg extract_frames").await?;
        }
    } else {
        let rate = fps.unwrap_or(1.0);
        if !rate.is_finite() || rate <= 0.0 || rate > 60.0 {
            return Err(MediaError::InvalidInput {
                reason: format!("Extraction fps must be finite and in (0, 60] (got {rate})"),
            });
        }
        let pattern = dest_dir.join("frame_%04d.png");
        let args = vec![
            "-y".to_string(),
            "-hide_banner".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-i".to_string(),
            path.to_string_lossy().to_string(),
            "-vf".to_string(),
            format!("fps={rate:.3}"),
            pattern.to_string_lossy().to_string(),
        ];
        run_ffmpeg(&args, "ffmpeg extract_frames").await?;
    }

    let mut frames = Vec::new();
    let mut rd = tokio::fs::read_dir(&dest_dir)
        .await
        .map_err(|e| MediaError::Io {
            path: dest_dir.to_string_lossy().to_string(),
            source: e,
        })?;
    while let Ok(Some(entry)) = rd.next_entry().await {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("png") {
            frames.push(p);
        }
    }
    frames.sort();

    Ok(ExtractedFramesResult {
        output_dir: dest_dir,
        frames,
    })
}

/// Renders a declarative `TimelineSpec` safely via `ffmpeg`.
pub async fn render_timeline(
    spec: &TimelineSpec,
    output_path: Option<&Path>,
    event_bus: Option<&EventBus>,
) -> Result<VideoEditResult, MediaError> {
    spec.validate()?;

    for clip in &spec.clips {
        if !tokio::fs::try_exists(Path::new(&clip.path))
            .await
            .unwrap_or(false)
        {
            return Err(MediaError::InvalidInput {
                reason: format!("Clip file does not exist: '{}'", clip.path),
            });
        }
    }
    if let Some(ref audio) = spec.audio_track {
        if !tokio::fs::try_exists(Path::new(audio)).await.unwrap_or(false) {
            return Err(MediaError::InvalidInput {
                reason: format!("Audio track file does not exist: '{audio}'"),
            });
        }
    }
    if let Some(ref srt) = spec.subtitles_srt {
        if !tokio::fs::try_exists(Path::new(srt)).await.unwrap_or(false) {
            return Err(MediaError::InvalidInput {
                reason: format!("Subtitles SRT file does not exist: '{srt}'"),
            });
        }
    }

    let fmt = spec.resolved_format(output_path);
    let dest = match output_path {
        Some(p) => harness_core::resolve_safe_path(p),
        None => {
            let dir = harness_core::resolve_dex_subpath(DEFAULT_VIDEO_DIR);
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|e| MediaError::Io {
                    path: dir.to_string_lossy().to_string(),
                    source: e,
                })?;
            dir.join(format!("edit_{}.{fmt}", current_epoch_millis()))
        }
    };

    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| MediaError::Io {
                    path: parent.to_string_lossy().to_string(),
                    source: e,
                })?;
        }
    }

    let task_id = format!("vid_edit_{}", current_epoch_millis());
    if let Some(bus) = event_bus {
        bus.publish(DexEvent::MediaGenerationProgress {
            task_id: task_id.clone(),
            kind: "video".to_string(),
            step: 0,
            total_steps: 2,
            message: format!("Rendering timeline ({} clips) via ffmpeg...", spec.clips.len()),
        });
    }

    let args = build_ffmpeg_timeline_args(spec, &dest)?;
    debug!(args = ?args, "Running ffmpeg timeline render");
    run_ffmpeg(&args, "ffmpeg render_timeline").await?;

    let size_bytes = tokio::fs::metadata(&dest)
        .await
        .map(|m| m.len())
        .unwrap_or(0);

    if let Some(bus) = event_bus {
        bus.publish(DexEvent::MediaGenerationProgress {
            task_id,
            kind: "video".to_string(),
            step: 2,
            total_steps: 2,
            message: "Timeline rendered".to_string(),
        });
        bus.publish(DexEvent::MediaGenerated {
            kind: "video".to_string(),
            path: dest.to_string_lossy().to_string(),
            prompt: format!("Timeline render ({} clips)", spec.clips.len()),
            duration_secs: None,
        });
    }

    Ok(VideoEditResult {
        output_path: dest,
        size_bytes,
        format: fmt,
        clips_count: spec.clips.len(),
    })
}

async fn run_ffmpeg(args: &[String], op_name: &str) -> Result<(), MediaError> {
    let mut cmd = Command::new("ffmpeg");
    harness_core::apply_enriched_path(&mut cmd);
    let output = tokio::time::timeout(
        Duration::from_secs(FFMPEG_TIMEOUT_SECS),
        cmd.args(args).output(),
    )
    .await
    .map_err(|_| MediaError::Timeout {
        operation: op_name.to_string(),
        timeout_secs: FFMPEG_TIMEOUT_SECS,
    })?
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            MediaError::BackendUnavailable {
                kind: "video".to_string(),
                reason: "`ffmpeg` not found in $PATH. Install FFmpeg with 1 click (`brew install ffmpeg`). [dex-install:ffmpeg]"
                    .to_string(),
            }
        } else {
            MediaError::Subprocess {
                command: "ffmpeg".to_string(),
                message: e.to_string(),
            }
        }
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(MediaError::Subprocess {
            command: "ffmpeg".to_string(),
            message: if stderr.is_empty() {
                format!("Exited with status {}", output.status)
            } else {
                stderr
            },
        });
    }

    Ok(())
}

fn mlx_base_url() -> String {
    std::env::var("DEX_MLX_GEN_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:8190".to_string())
        .trim_end_matches('/')
        .to_string()
}

async fn wake_extension_if_enabled(
    supervisor: Option<&Arc<ExtensionSupervisor>>,
    extension_id: &str,
) {
    if let Some(sup) = supervisor {
        let reg = sup.load_registry().await;
        if reg.get(extension_id).is_some_and(|m| m.enabled) {
            let _ = sup.start_extension(extension_id).await;
            sup.touch_last_used(extension_id).await;
        }
    }
}

async fn is_http_reachable(url: &str) -> bool {
    let probe = reqwest::Client::builder()
        .timeout(Duration::from_millis(350))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    probe.get(url).send().await.is_ok()
}

/// Generates a short video clip (text-to-video or image-to-video) via `mlx-gen` or ComfyUI,
/// saving the resulting file into `.dex/media/video/` and emitting live progress events.
pub async fn generate_video(
    req: VideoGenRequest,
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<&EventBus>,
) -> Result<VideoGenResult, MediaError> {
    let prompt = req.prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(MediaError::InvalidInput {
            reason: "Video generation prompt cannot be empty".to_string(),
        });
    }

    let out_dir = harness_core::resolve_dex_subpath(DEFAULT_VIDEO_DIR);
    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| MediaError::Io {
            path: out_dir.to_string_lossy().to_string(),
            source: e,
        })?;

    let ts = current_epoch_millis();
    let seed = req.seed.unwrap_or((ts % 2_147_483_647) as u64);
    let task_id = format!("vid_{ts}");

    if let Some(bus) = event_bus {
        bus.publish(DexEvent::MediaGenerationProgress {
            task_id: task_id.clone(),
            kind: "video".to_string(),
            step: 0,
            total_steps: req.steps.max(1),
            message: format!("Starting video generation ({:?})...", req.backend),
        });
    }

    let result = match req.backend {
        ImageBackend::Mlx | ImageBackend::Http => {
            generate_video_via_mlx(
                &req,
                &prompt,
                seed,
                &out_dir,
                &task_id,
                supervisor.as_ref(),
                event_bus,
            )
            .await?
        }
        ImageBackend::ComfyUi => {
            generate_video_via_comfyui(
                &req,
                &prompt,
                seed,
                &out_dir,
                &task_id,
                supervisor.as_ref(),
                event_bus,
            )
            .await?
        }
        ImageBackend::Auto => {
            wake_extension_if_enabled(supervisor.as_ref(), "mlx-gen").await;
            let mlx_url = mlx_base_url();
            if is_http_reachable(&format!("{mlx_url}/health")).await {
                generate_video_via_mlx(
                    &req,
                    &prompt,
                    seed,
                    &out_dir,
                    &task_id,
                    supervisor.as_ref(),
                    event_bus,
                )
                .await?
            } else {
                wake_extension_if_enabled(supervisor.as_ref(), "comfyui-mcp").await;
                let comfy = ComfyUiClient::default();
                if comfy.is_reachable().await {
                    generate_video_via_comfyui(
                        &req,
                        &prompt,
                        seed,
                        &out_dir,
                        &task_id,
                        supervisor.as_ref(),
                        event_bus,
                    )
                    .await?
                } else {
                    return Err(MediaError::BackendNotInstalled {
                        kind: "video".to_string(),
                        backend: "mlx-gen / comfyui-mcp".to_string(),
                        install_hint: "Install or start `mlx-gen` (port 8190) or ComfyUI (port 8188) with a video workflow. Do NOT retry `video_generate` in a loop.".to_string(),
                    });
                }
            }
        }
    };

    if let Some(bus) = event_bus {
        bus.publish(DexEvent::MediaGenerated {
            kind: "video".to_string(),
            path: result.path.to_string_lossy().to_string(),
            prompt: prompt.chars().take(140).collect(),
            duration_secs: result.duration_secs,
        });
    }

    Ok(result)
}

async fn generate_video_via_mlx(
    req: &VideoGenRequest,
    prompt: &str,
    seed: u64,
    out_dir: &Path,
    task_id: &str,
    supervisor: Option<&Arc<ExtensionSupervisor>>,
    event_bus: Option<&EventBus>,
) -> Result<VideoGenResult, MediaError> {
    wake_extension_if_enabled(supervisor, "mlx-gen").await;
    let base = mlx_base_url();
    let endpoint = format!("{base}/v1/videos/generations");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(VIDEO_GEN_TIMEOUT_SECS))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let payload = serde_json::json!({
        "prompt": prompt,
        "image_path": req.image_path,
        "width": req.width,
        "height": req.height,
        "steps": req.steps,
        "seed": seed,
        "duration_secs": req.duration_secs.unwrap_or(4.0),
    });

    info!(endpoint = %endpoint, "Dispatching MLX video generation");
    let resp = tokio::time::timeout(
        Duration::from_secs(VIDEO_GEN_TIMEOUT_SECS),
        client.post(&endpoint).json(&payload).send(),
    )
    .await
    .map_err(|_| MediaError::Timeout {
        operation: "mlx video generation".to_string(),
        timeout_secs: VIDEO_GEN_TIMEOUT_SECS,
    })?
    .map_err(|e| MediaError::Http {
        url: endpoint.clone(),
        source: e,
    })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(MediaError::BackendUnavailable {
            kind: "video".to_string(),
            reason: format!("MLX video endpoint returned HTTP {status}: {body}"),
        });
    }

    let bytes = resp.bytes().await.map_err(|e| MediaError::Http {
        url: endpoint,
        source: e,
    })?;

    let dest = out_dir.join(format!("mlx_vid_{}.mp4", current_epoch_millis()));
    tokio::fs::write(&dest, &bytes)
        .await
        .map_err(|e| MediaError::Io {
            path: dest.to_string_lossy().to_string(),
            source: e,
        })?;

    if let Some(bus) = event_bus {
        bus.publish(DexEvent::MediaGenerationProgress {
            task_id: task_id.to_string(),
            kind: "video".to_string(),
            step: req.steps.max(1),
            total_steps: req.steps.max(1),
            message: "Video saved".to_string(),
        });
    }

    Ok(VideoGenResult {
        path: dest,
        backend: "mlx-gen".to_string(),
        prompt: prompt.to_string(),
        duration_secs: req.duration_secs,
    })
}

async fn generate_video_via_comfyui(
    req: &VideoGenRequest,
    prompt: &str,
    seed: u64,
    out_dir: &Path,
    task_id: &str,
    supervisor: Option<&Arc<ExtensionSupervisor>>,
    event_bus: Option<&EventBus>,
) -> Result<VideoGenResult, MediaError> {
    wake_extension_if_enabled(supervisor, "comfyui-mcp").await;
    let client = ComfyUiClient::default();

    let wf_name = req
        .workflow
        .as_deref()
        .filter(|w| !w.trim().is_empty())
        .unwrap_or("video_default.json");
    let wf_dir = harness_core::resolve_dex_subpath(DEFAULT_WORKFLOWS_DIR);
    let template = load_workflow_template(&wf_dir, wf_name).await?;

    let mut str_vars = HashMap::new();
    str_vars.insert("PROMPT", prompt.to_string());
    if let Some(ref img) = req.image_path {
        str_vars.insert("IMAGE_PATH", img.clone());
    }

    let mut num_vars = HashMap::new();
    num_vars.insert("SEED", seed);
    num_vars.insert("WIDTH", u64::from(req.width));
    num_vars.insert("HEIGHT", u64::from(req.height));
    num_vars.insert("STEPS", u64::from(req.steps));

    let workflow = inject_workflow_placeholders(&template, &str_vars, &num_vars)?;
    let prompt_id = client.queue_prompt(workflow, task_id).await?;
    let files = client
        .poll_history_and_download(&prompt_id, out_dir, task_id, "video", req.steps, event_bus)
        .await?;

    let dest = files.into_iter().next().ok_or_else(|| MediaError::ComfyUi {
        message: format!("ComfyUI video prompt {prompt_id} produced no output files"),
    })?;

    Ok(VideoGenResult {
        path: dest,
        backend: "comfyui".to_string(),
        prompt: prompt.to_string(),
        duration_secs: req.duration_secs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timeline_spec_validation_and_ffmpeg_args() {
        // 1. Invalid empty timeline
        let empty = TimelineSpec {
            clips: vec![],
            audio_track: None,
            subtitles_srt: None,
            width: None,
            height: None,
            fps: None,
            output_format: None,
        };
        assert!(empty.validate().is_err());

        // 2. Invalid trim window (end <= start)
        let bad_trim = TimelineSpec {
            clips: vec![VideoClip {
                path: "clip1.mp4".to_string(),
                start_secs: Some(5.0),
                end_secs: Some(2.0),
            }],
            audio_track: None,
            subtitles_srt: None,
            width: None,
            height: None,
            fps: None,
            output_format: Some("mp4".to_string()),
        };
        assert!(bad_trim.validate().is_err());

        // 3. Invalid format
        let bad_fmt = TimelineSpec {
            clips: vec![VideoClip {
                path: "clip1.mp4".to_string(),
                start_secs: None,
                end_secs: None,
            }],
            audio_track: None,
            subtitles_srt: None,
            width: None,
            height: None,
            fps: None,
            output_format: Some("exe".to_string()),
        };
        assert!(bad_fmt.validate().is_err());

        // 4. Valid multi-clip timeline with audio track, subtitles, scaling, and FPS
        let spec = TimelineSpec {
            clips: vec![
                VideoClip {
                    path: "intro.mp4".to_string(),
                    start_secs: Some(0.5),
                    end_secs: Some(4.0),
                },
                VideoClip {
                    path: "main.mp4".to_string(),
                    start_secs: Some(1.0),
                    end_secs: None,
                },
            ],
            audio_track: Some("voiceover.wav".to_string()),
            subtitles_srt: Some("captions:v1.srt".to_string()),
            width: Some(1280),
            height: Some(720),
            fps: Some(30.0),
            output_format: Some("mp4".to_string()),
        };

        let args = build_ffmpeg_timeline_args(&spec, Path::new("out/final.mp4"))
            .expect("valid timeline must produce ffmpeg args");

        assert!(args.contains(&"-ss".to_string()));
        assert!(args.contains(&"0.500".to_string()));
        assert!(args.contains(&"-to".to_string()));
        assert!(args.contains(&"4.000".to_string()));
        assert!(args.contains(&"voiceover.wav".to_string()));
        assert!(args.contains(&"-filter_complex".to_string()));
        assert!(args.contains(&"2:a".to_string()));
        assert!(args.contains(&"-shortest".to_string()));
        assert!(args.contains(&"libx264".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("out/final.mp4"));

        let fc_idx = args
            .iter()
            .position(|a| a == "-filter_complex")
            .expect("filter_complex flag present");
        let fc = &args[fc_idx + 1];
        assert!(fc.contains("concat=n=2:v=1:a=0[vcat]"));
        assert!(fc.contains("scale=1280:720"));
        assert!(fc.contains("fps=30.000"));
        assert!(fc.contains("subtitles='captions\\:v1.srt'[vout]"));
    }

    #[test]
    fn test_parse_ffprobe_json() {
        let sample_json = r#"{
          "streams": [
            {
              "index": 0,
              "codec_name": "h264",
              "codec_type": "video",
              "width": 1920,
              "height": 1080,
              "avg_frame_rate": "30000/1001"
            },
            {
              "index": 1,
              "codec_name": "aac",
              "codec_type": "audio"
            }
          ],
          "format": {
            "format_name": "mov,mp4,m4a,3gp,3g2,mj2",
            "duration": "12.450000",
            "size": "4829104",
            "bit_rate": "3103038"
          }
        }"#;

        let meta = parse_ffprobe_json(sample_json, Path::new("demo.mp4"))
            .expect("ffprobe JSON should parse");

        assert_eq!(meta.width, Some(1920));
        assert_eq!(meta.height, Some(1080));
        assert_eq!(meta.video_codec.as_deref(), Some("h264"));
        assert_eq!(meta.audio_codec.as_deref(), Some("aac"));
        assert_eq!(meta.size_bytes, Some(4_829_104));
        assert_eq!(meta.bitrate, Some(3_103_038));
        assert!((meta.duration_secs.unwrap_or(0.0) - 12.45).abs() < 1e-4);
        assert!((meta.fps.unwrap_or(0.0) - 29.97003).abs() < 1e-3);
    }
}
