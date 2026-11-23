pub mod comfyui;
pub mod error;
pub mod image_gen;
pub mod image_ops;
pub mod tools;
pub mod video;

use std::path::Path;
use serde::{Deserialize, Serialize};

pub use comfyui::{
    ensure_default_workflows, inject_workflow_placeholders, list_workflows, load_workflow_template,
    ComfyUiClient, DEFAULT_TXT2IMG_WORKFLOW_JSON, DEFAULT_UPSCALE_WORKFLOW_JSON,
    DEFAULT_VIDEO_WORKFLOW_JSON, DEFAULT_WORKFLOWS_DIR,
};
pub use error::MediaError;
pub use image_gen::{generate_image, upscale_image, ImageBackend, ImageGenRequest, ImageGenResult};
pub use image_ops::{
    inspect_image, lanczos_upscale, parse_dimensions_str, process_image, CropRegion, ImageInfo,
    ImageProcessOptions, ImageProcessResult, DEFAULT_IMAGES_DIR,
};
pub use tools::{
    register_media_tools, ImageGenerateArgs, ImageGenerateTool, ImageProcessArgs, ImageProcessTool,
    ImageUpscaleArgs, ImageUpscaleTool, VideoEditArgs, VideoEditTool, VideoExtractFramesArgs,
    VideoExtractFramesTool, VideoGenerateArgs, VideoGenerateTool, VideoInspectArgs,
    VideoInspectTool,
};
pub use video::{
    build_ffmpeg_timeline_args, extract_frames, generate_video, inspect_video, parse_ffprobe_json,
    render_timeline, ExtractedFramesResult, TimelineSpec, VideoClip, VideoEditResult,
    VideoGenRequest, VideoGenResult, VideoMetadata, DEFAULT_FRAMES_DIR, DEFAULT_VIDEO_DIR,
};

/// A stored visual asset (image or video) discovered in `.dex/media/{images,video}/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualMediaItem {
    pub kind: String,
    pub filename: String,
    pub path: String,
    pub format: String,
    pub size_bytes: u64,
}

/// Scans `.dex/media/images/` and `.dex/media/video/` and returns discovered visual media items
/// sorted newest-filename-first.
pub async fn list_visual_media_items() -> Result<Vec<VisualMediaItem>, MediaError> {
    list_visual_media_in_dirs(Path::new(DEFAULT_IMAGES_DIR), Path::new(DEFAULT_VIDEO_DIR)).await
}

/// Scans the given image and video directories for supported visual files.
pub async fn list_visual_media_in_dirs(
    images_dir: &Path,
    videos_dir: &Path,
) -> Result<Vec<VisualMediaItem>, MediaError> {
    let mut items = Vec::new();
    let targets = [
        ("image", images_dir, &["png", "jpg", "jpeg", "webp"][..]),
        ("video", videos_dir, &["mp4", "mov", "webm", "mkv", "gif"][..]),
    ];

    for (kind, dir, allowed_exts) in targets {
        if !tokio::fs::try_exists(dir).await.unwrap_or(false) {
            continue;
        }
        let mut rd = tokio::fs::read_dir(dir).await.map_err(|e| MediaError::Io {
            path: dir.to_string_lossy().to_string(),
            source: e,
        })?;

        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !allowed_exts.contains(&ext.as_str()) {
                continue;
            }
            let size_bytes = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
            let filename = entry.file_name().to_string_lossy().to_string();
            items.push(VisualMediaItem {
                kind: kind.to_string(),
                filename,
                path: path.to_string_lossy().to_string(),
                format: ext,
                size_bytes,
            });
        }
    }

    items.sort_by(|a, b| b.filename.cmp(&a.filename));
    Ok(items)
}
