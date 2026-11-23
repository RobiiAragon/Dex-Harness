use std::path::{Path, PathBuf};
use std::sync::Arc;
use async_trait::async_trait;
use harness_core::{EventBus, Tool, ToolError, ToolRegistry};
use harness_extensions::ExtensionSupervisor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::image_gen::{generate_image, upscale_image, ImageBackend, ImageGenRequest};
use crate::image_ops::{process_image, CropRegion, ImageProcessOptions};
use crate::video::{
    extract_frames, generate_video, inspect_video, render_timeline, TimelineSpec, VideoGenRequest,
};

fn default_image_size() -> u32 {
    1024
}

fn default_image_steps() -> u32 {
    20
}

/// Arguments for `image_generate`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImageGenerateArgs {
    /// Text prompt describing the desired image.
    pub prompt: String,
    /// Optional negative prompt of elements to avoid.
    #[serde(default)]
    pub negative_prompt: Option<String>,
    /// Output image width in pixels (default: 1024).
    #[serde(default = "default_image_size")]
    pub width: u32,
    /// Output image height in pixels (default: 1024).
    #[serde(default = "default_image_size")]
    pub height: u32,
    /// Number of denoising steps (default: 20).
    #[serde(default = "default_image_steps")]
    pub steps: u32,
    /// Optional deterministic RNG seed.
    #[serde(default)]
    pub seed: Option<u64>,
    /// Backend selector: `auto`, `mlx`, `comfy_ui`, or `http`.
    #[serde(default)]
    pub backend: ImageBackend,
    /// Optional ComfyUI workflow JSON filename inside `.dex/workflows/` (e.g. `txt2img_default.json`).
    #[serde(default)]
    pub workflow: Option<String>,
    /// Optional model identifier (e.g. `schnell`, `dev`, `gpt-image-1`).
    #[serde(default)]
    pub model: Option<String>,
    /// Optional explicit output file path (e.g. `/Users/.../concept.png`). Defaults to Dex's writable media directory.
    #[serde(default)]
    pub output_path: Option<String>,
}

/// Generates an image via local MLX (`mlx-gen`/`mflux`), ComfyUI, or an OpenAI-compatible HTTP endpoint.
pub struct ImageGenerateTool {
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<EventBus>,
}

impl ImageGenerateTool {
    pub fn new(supervisor: Option<Arc<ExtensionSupervisor>>, event_bus: Option<EventBus>) -> Self {
        Self {
            supervisor,
            event_bus,
        }
    }
}

#[async_trait]
impl Tool for ImageGenerateTool {
    fn name(&self) -> &str {
        "image_generate"
    }

    fn description(&self) -> &str {
        "Generates an image from a text prompt using local Apple Silicon MLX (`mlx-gen`/`mflux`), ComfyUI workflows, or an OpenAI-compatible HTTP image endpoint. Saves PNGs to `.dex/media/images/`."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(ImageGenerateArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: ImageGenerateArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let req = ImageGenRequest {
            prompt: args.prompt,
            negative_prompt: args.negative_prompt,
            width: args.width,
            height: args.height,
            steps: args.steps,
            seed: args.seed,
            backend: args.backend,
            workflow: args.workflow,
            model: args.model,
            output_path: args
                .output_path
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from),
        };

        let res = generate_image(req, self.supervisor.clone(), self.event_bus.as_ref())
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: e.to_string(),
            })?;

        Ok(format!(
            "Image generated ({}x{}, backend: {}, seed: {}). Saved to: {}",
            res.width,
            res.height,
            res.backend,
            res.seed,
            res.path.display()
        ))
    }
}

fn default_upscale_factor() -> u32 {
    2
}

/// Arguments for `image_upscale`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImageUpscaleArgs {
    /// Path to the input image file.
    pub path: String,
    /// Upscale multiplier between 2 and 8 (default: 2).
    #[serde(default = "default_upscale_factor")]
    pub scale: u32,
    /// Preferred upscaling backend: `auto`, `mlx`, `comfy_ui`, or `http`. Falls back to native Lanczos3.
    #[serde(default)]
    pub backend: ImageBackend,
}

/// Upscales an image via MLX/ComfyUI neural upscaler with deterministic pure-Rust Lanczos3 fallback.
pub struct ImageUpscaleTool {
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<EventBus>,
}

impl ImageUpscaleTool {
    pub fn new(supervisor: Option<Arc<ExtensionSupervisor>>, event_bus: Option<EventBus>) -> Self {
        Self {
            supervisor,
            event_bus,
        }
    }
}

#[async_trait]
impl Tool for ImageUpscaleTool {
    fn name(&self) -> &str {
        "image_upscale"
    }

    fn description(&self) -> &str {
        "Upscales an image by 2x-8x using MLX/ComfyUI neural upscalers when available, or deterministic high-quality Lanczos3 in pure Rust."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(ImageUpscaleArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: ImageUpscaleArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let res = upscale_image(
            Path::new(&args.path),
            args.scale,
            args.backend,
            self.supervisor.clone(),
            self.event_bus.as_ref(),
        )
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: e.to_string(),
        })?;

        Ok(format!(
            "Image upscaled {}x ({}x{}, {}, {} bytes, backend: {}). Saved to: {}",
            args.scale,
            res.width,
            res.height,
            res.format,
            res.size_bytes,
            res.backend,
            res.output_path.display()
        ))
    }
}

/// Arguments for `image_process`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImageProcessArgs {
    /// Path to the source image (.png, .jpg, .webp).
    pub path: String,
    /// Optional target width in pixels.
    #[serde(default)]
    pub resize_width: Option<u32>,
    /// Optional target height in pixels.
    #[serde(default)]
    pub resize_height: Option<u32>,
    /// When true and both resize_width & resize_height are set, preserves aspect ratio within bounds.
    #[serde(default)]
    pub preserve_aspect: bool,
    /// Optional rectangular crop region applied before resizing.
    #[serde(default)]
    pub crop: Option<CropRegion>,
    /// Optional output format conversion: `png`, `jpg`, or `webp`.
    #[serde(default)]
    pub format: Option<String>,
    /// Optional output file path. Defaults to `.dex/media/images/`.
    #[serde(default)]
    pub output_path: Option<String>,
}

/// Fast pure-Rust image processing (crop, resize, convert PNG/JPG/WebP).
pub struct ImageProcessTool;

#[async_trait]
impl Tool for ImageProcessTool {
    fn name(&self) -> &str {
        "image_process"
    }

    fn description(&self) -> &str {
        "Performs fast native pure-Rust image operations: crop, Lanczos3 resize, and format conversion between PNG, JPG, and WebP."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(ImageProcessArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: ImageProcessArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let opts = ImageProcessOptions {
            resize_width: args.resize_width,
            resize_height: args.resize_height,
            preserve_aspect: args.preserve_aspect,
            crop: args.crop,
            target_format: args.format,
            output_path: args.output_path.map(PathBuf::from),
        };

        let res = process_image(Path::new(&args.path), opts)
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: e.to_string(),
            })?;

        Ok(format!(
            "Processed image ({}x{}, {}, {} bytes). Saved to: {}",
            res.width,
            res.height,
            res.format,
            res.size_bytes,
            res.output_path.display()
        ))
    }
}

fn default_video_width() -> u32 {
    768
}

fn default_video_height() -> u32 {
    512
}

fn default_video_steps() -> u32 {
    24
}

/// Arguments for `video_generate`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VideoGenerateArgs {
    /// Text prompt describing the scene and motion.
    pub prompt: String,
    /// Optional conditioning image path for image-to-video workflows.
    #[serde(default)]
    pub image_path: Option<String>,
    /// Output video width in pixels (default: 768).
    #[serde(default = "default_video_width")]
    pub width: u32,
    /// Output video height in pixels (default: 512).
    #[serde(default = "default_video_height")]
    pub height: u32,
    /// Number of generation steps (default: 24).
    #[serde(default = "default_video_steps")]
    pub steps: u32,
    /// Optional deterministic RNG seed.
    #[serde(default)]
    pub seed: Option<u64>,
    /// Optional target duration in seconds (default: 4.0).
    #[serde(default)]
    pub duration_secs: Option<f64>,
    /// Backend selector: `auto`, `mlx`, or `comfy_ui`.
    #[serde(default)]
    pub backend: ImageBackend,
    /// Optional ComfyUI workflow JSON filename inside `.dex/workflows/` (default: `video_default.json`).
    #[serde(default)]
    pub workflow: Option<String>,
}

/// Generates a short video clip via `mlx-gen` or ComfyUI video workflows.
pub struct VideoGenerateTool {
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<EventBus>,
}

impl VideoGenerateTool {
    pub fn new(supervisor: Option<Arc<ExtensionSupervisor>>, event_bus: Option<EventBus>) -> Self {
        Self {
            supervisor,
            event_bus,
        }
    }
}

#[async_trait]
impl Tool for VideoGenerateTool {
    fn name(&self) -> &str {
        "video_generate"
    }

    fn description(&self) -> &str {
        "Generates a short video clip (text-to-video or image-to-video) via local MLX (`mlx-gen`) or ComfyUI (`video_default.json`), saving outputs to `.dex/media/video/`."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(VideoGenerateArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: VideoGenerateArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let req = VideoGenRequest {
            prompt: args.prompt,
            image_path: args.image_path,
            width: args.width,
            height: args.height,
            steps: args.steps,
            seed: args.seed,
            duration_secs: args.duration_secs.or(Some(4.0)),
            backend: args.backend,
            workflow: args.workflow,
        };

        let res = generate_video(req, self.supervisor.clone(), self.event_bus.as_ref())
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: e.to_string(),
            })?;

        Ok(format!(
            "Video generated (backend: {}, duration: {:?}s). Saved to: {}",
            res.backend,
            res.duration_secs,
            res.path.display()
        ))
    }
}

/// Arguments for `video_inspect`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VideoInspectArgs {
    /// Path to the video file to inspect.
    pub path: String,
}

/// Inspects video metadata (duration, resolution, FPS, codecs, bitrate) using `ffprobe`.
pub struct VideoInspectTool;

#[async_trait]
impl Tool for VideoInspectTool {
    fn name(&self) -> &str {
        "video_inspect"
    }

    fn description(&self) -> &str {
        "Inspects a video file with `ffprobe` to return duration, width, height, FPS, video/audio codecs, bitrate, and file size as JSON."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(VideoInspectArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: VideoInspectArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let meta = inspect_video(Path::new(&args.path))
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: e.to_string(),
            })?;

        serde_json::to_string_pretty(&meta).map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: format!("Failed to serialize video metadata: {e}"),
        })
    }
}

/// Arguments for `video_extract_frames`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VideoExtractFramesArgs {
    /// Path to the source video file.
    pub path: String,
    /// Optional extraction frame rate (e.g. `1.0` for 1 frame per second). Ignored if `timestamps_secs` is provided.
    #[serde(default)]
    pub fps: Option<f64>,
    /// Optional explicit list of timestamps in seconds to extract individual frames.
    #[serde(default)]
    pub timestamps_secs: Option<Vec<f64>>,
    /// Optional destination folder for extracted PNG frames (defaults to `.dex/media/frames/`).
    #[serde(default)]
    pub output_dir: Option<String>,
}

/// Extracts keyframes from a video into PNG images using `ffmpeg`.
pub struct VideoExtractFramesTool;

#[async_trait]
impl Tool for VideoExtractFramesTool {
    fn name(&self) -> &str {
        "video_extract_frames"
    }

    fn description(&self) -> &str {
        "Extracts keyframes as PNG images from a video file using `ffmpeg` (either at a fixed FPS or specific timestamps) into `.dex/media/frames/` for visual inspection or OCR."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(VideoExtractFramesArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: VideoExtractFramesArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let res = extract_frames(
            Path::new(&args.path),
            args.fps,
            args.timestamps_secs.as_deref(),
            args.output_dir.as_deref().map(Path::new),
        )
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: e.to_string(),
        })?;

        let paths_list: Vec<String> = res
            .frames
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        Ok(format!(
            "Extracted {} frames into {}:\n{}",
            paths_list.len(),
            res.output_dir.display(),
            paths_list.join("\n")
        ))
    }
}

/// Arguments for `video_edit`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VideoEditArgs {
    /// Declarative editing timeline (clips with optional trim windows, audio track, SRT subtitles, scaling, FPS, format).
    pub timeline: TimelineSpec,
    /// Optional output video file path (defaults to `.dex/media/video/`).
    #[serde(default)]
    pub output_path: Option<String>,
}

/// Renders a declarative JSON `TimelineSpec` safely via `ffmpeg` without shell interpolation.
pub struct VideoEditTool {
    event_bus: Option<EventBus>,
}

impl VideoEditTool {
    pub fn new(event_bus: Option<EventBus>) -> Self {
        Self { event_bus }
    }
}

#[async_trait]
impl Tool for VideoEditTool {
    fn name(&self) -> &str {
        "video_edit"
    }

    fn description(&self) -> &str {
        "Declaratively edits and renders video from a typed JSON `TimelineSpec` (trim clips, concatenate, overlay audio track, burn SRT subtitles, resize, change FPS/container) safely via `ffmpeg`."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(VideoEditArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: VideoEditArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let res = render_timeline(
            &args.timeline,
            args.output_path.as_deref().map(Path::new),
            self.event_bus.as_ref(),
        )
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: e.to_string(),
        })?;

        Ok(format!(
            "Rendered {} clips into {} ({}, {} bytes).",
            res.clips_count,
            res.output_path.display(),
            res.format,
            res.size_bytes
        ))
    }
}

/// Registers all 7 visual media tools into `registry` as `ToolVisibility::Deferred` under category `"ImageVideo"`.
pub fn register_media_tools(
    registry: &mut ToolRegistry,
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<EventBus>,
) {
    registry.register_deferred(
        Arc::new(ImageGenerateTool::new(
            supervisor.clone(),
            event_bus.clone(),
        )),
        "ImageVideo",
        vec![
            "image".to_string(),
            "generate".to_string(),
            "flux".to_string(),
            "mlx".to_string(),
            "comfyui".to_string(),
            "imagine".to_string(),
        ],
    );

    registry.register_deferred(
        Arc::new(ImageUpscaleTool::new(
            supervisor.clone(),
            event_bus.clone(),
        )),
        "ImageVideo",
        vec![
            "image".to_string(),
            "upscale".to_string(),
            "superres".to_string(),
            "lanczos".to_string(),
            "enhance".to_string(),
        ],
    );

    registry.register_deferred(
        Arc::new(ImageProcessTool),
        "ImageVideo",
        vec![
            "image".to_string(),
            "resize".to_string(),
            "crop".to_string(),
            "convert".to_string(),
            "webp".to_string(),
            "png".to_string(),
        ],
    );

    registry.register_deferred(
        Arc::new(VideoGenerateTool::new(supervisor, event_bus.clone())),
        "ImageVideo",
        vec![
            "video".to_string(),
            "generate".to_string(),
            "ltx".to_string(),
            "wan".to_string(),
            "comfyui".to_string(),
            "mlx".to_string(),
        ],
    );

    registry.register_deferred(
        Arc::new(VideoInspectTool),
        "ImageVideo",
        vec![
            "video".to_string(),
            "inspect".to_string(),
            "ffprobe".to_string(),
            "metadata".to_string(),
            "codec".to_string(),
        ],
    );

    registry.register_deferred(
        Arc::new(VideoExtractFramesTool),
        "ImageVideo",
        vec![
            "video".to_string(),
            "frames".to_string(),
            "keyframes".to_string(),
            "ffmpeg".to_string(),
            "extract".to_string(),
        ],
    );

    registry.register_deferred(
        Arc::new(VideoEditTool::new(event_bus)),
        "ImageVideo",
        vec![
            "video".to_string(),
            "edit".to_string(),
            "timeline".to_string(),
            "concat".to_string(),
            "trim".to_string(),
            "ffmpeg".to_string(),
            "subtitles".to_string(),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_media_tools_registered_as_deferred_under_image_video_category() {
        let mut registry = ToolRegistry::new();
        register_media_tools(&mut registry, None, None);

        assert!(registry.active_definitions().is_empty());
        assert_eq!(registry.deferred_count(), 7);

        let img_hits = registry.search_deferred("upscale", Some("ImageVideo"));
        assert_eq!(img_hits.len(), 1);
        assert_eq!(img_hits[0].name, "image_upscale");

        let vid_hits = registry.search_deferred("timeline", Some("ImageVideo"));
        assert_eq!(vid_hits.len(), 1);
        assert_eq!(vid_hits[0].name, "video_edit");

        let all_hits = registry.search_deferred("", Some("ImageVideo"));
        assert_eq!(all_hits.len(), 7);
    }
}
