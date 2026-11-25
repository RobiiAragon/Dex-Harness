use std::path::{Path, PathBuf};
use std::sync::Arc;
use anyhow::{Context, Result};
use harness_core::EventBus;
use harness_extensions::ExtensionSupervisor;
use harness_media::{
    extract_frames, generate_image, inspect_video, list_visual_media_items, parse_dimensions_str,
    process_image, render_timeline, upscale_image, ImageBackend, ImageGenRequest,
    ImageProcessOptions, TimelineSpec,
};

#[allow(clippy::too_many_arguments)]
pub async fn imagine(
    prompt: &str,
    negative: Option<String>,
    width: u32,
    height: u32,
    steps: u32,
    backend: &str,
    workflow: Option<String>,
    event_bus: EventBus,
) -> Result<()> {
    let parsed_backend = ImageBackend::from_str_loose(backend);
    println!(
        "🎨 Generating image ({width}x{height}, {steps} steps, backend: {parsed_backend:?})..."
    );
    let supervisor = Arc::new(ExtensionSupervisor::new(event_bus.clone()));
    let req = ImageGenRequest {
        prompt: prompt.to_string(),
        negative_prompt: negative,
        width,
        height,
        steps,
        seed: None,
        backend: parsed_backend,
        workflow,
        model: None,
        output_path: None,
    };

    let res = generate_image(req, Some(supervisor), Some(&event_bus))
        .await
        .context("Image generation failed")?;

    println!(
        "✅ Image generated ({}x{}, backend: {}, seed: {})",
        res.width, res.height, res.backend, res.seed
    );
    println!("   Saved to: {}", res.path.display());
    Ok(())
}

pub async fn upscale(
    path: &str,
    scale: u32,
    backend: &str,
    out: Option<String>,
    event_bus: EventBus,
) -> Result<()> {
    let parsed_backend = ImageBackend::from_str_loose(backend);
    println!("🔍 Upscaling '{path}' by {scale}x (backend: {parsed_backend:?})...");
    let supervisor = Arc::new(ExtensionSupervisor::new(event_bus.clone()));

    let res = upscale_image(
        Path::new(path),
        scale,
        parsed_backend,
        Some(supervisor),
        Some(&event_bus),
    )
    .await
    .context("Image upscale failed")?;

    println!(
        "✅ Upscaled to {}x{} ({}, {} bytes, backend: {})",
        res.width, res.height, res.format, res.size_bytes, res.backend
    );
    let saved = match out {
        Some(dest) => {
            let dest = std::path::PathBuf::from(dest);
            if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("Failed to create '{}'", parent.display()))?;
            }
            // `rename` fails across volumes; fall back to copy + remove.
            if tokio::fs::rename(&res.output_path, &dest).await.is_err() {
                tokio::fs::copy(&res.output_path, &dest)
                    .await
                    .with_context(|| format!("Failed to write '{}'", dest.display()))?;
                let _ = tokio::fs::remove_file(&res.output_path).await;
            }
            dest
        }
        None => res.output_path.clone(),
    };
    println!("   Saved to: {}", saved.display());
    Ok(())
}

pub async fn process(
    path: &str,
    resize: Option<&str>,
    format: Option<String>,
    out: Option<String>,
) -> Result<()> {
    let (resize_width, resize_height) = match resize {
        Some(spec) => {
            let (w, h) = parse_dimensions_str(spec).context("Invalid --resize dimensions")?;
            (Some(w), Some(h))
        }
        None => (None, None),
    };

    println!("🖼️ Processing image '{path}'...");
    let opts = ImageProcessOptions {
        resize_width,
        resize_height,
        preserve_aspect: false,
        crop: None,
        target_format: format,
        output_path: out.map(PathBuf::from),
    };

    let res = process_image(Path::new(path), opts)
        .await
        .context("Image processing failed")?;

    println!(
        "✅ Processed image ({}x{}, {}, {} bytes)",
        res.width, res.height, res.format, res.size_bytes
    );
    println!("   Saved to: {}", res.output_path.display());
    Ok(())
}

pub async fn inspect_video_cli(path: &str) -> Result<()> {
    println!("🎬 Inspecting video '{path}' via ffprobe...");
    let meta = inspect_video(Path::new(path))
        .await
        .context("Video inspection failed")?;

    let pretty =
        serde_json::to_string_pretty(&meta).context("Failed to format video metadata JSON")?;
    println!("{pretty}");
    Ok(())
}

pub async fn extract_frames_cli(path: &str, fps: f64) -> Result<()> {
    println!("🎞️ Extracting frames from '{path}' at {fps} fps...");
    let res = extract_frames(Path::new(path), Some(fps), None, None)
        .await
        .context("Frame extraction failed")?;

    println!(
        "✅ Extracted {} frame(s) into {}",
        res.frames.len(),
        res.output_dir.display()
    );
    for frame in res.frames.iter().take(10) {
        println!("   - {}", frame.display());
    }
    if res.frames.len() > 10 {
        println!("   ... ({} more)", res.frames.len() - 10);
    }
    Ok(())
}

pub async fn render_timeline_cli(
    timeline_path: &str,
    out: Option<&str>,
    event_bus: EventBus,
) -> Result<()> {
    let raw = tokio::fs::read_to_string(timeline_path)
        .await
        .with_context(|| format!("Failed to read timeline JSON '{timeline_path}'"))?;
    let spec: TimelineSpec = serde_json::from_str(&raw)
        .with_context(|| format!("Invalid TimelineSpec JSON in '{timeline_path}'"))?;

    println!(
        "🎬 Rendering declarative timeline ({} clip(s)) via ffmpeg...",
        spec.clips.len()
    );
    let res = render_timeline(&spec, out.map(Path::new), Some(&event_bus))
        .await
        .context("Timeline render failed")?;

    println!(
        "✅ Rendered {} clip(s) -> {} ({}, {} bytes)",
        res.clips_count,
        res.output_path.display(),
        res.format,
        res.size_bytes
    );
    Ok(())
}

pub async fn list_media() -> Result<()> {
    let items = list_visual_media_items()
        .await
        .context("Failed to scan .dex/media/{images,video}/")?;

    println!("🎨 Visual Media Library (.dex/media/images & .dex/media/video):");
    if items.is_empty() {
        println!("   (No generated images or videos found yet)");
        return Ok(());
    }

    for item in items {
        let icon = if item.kind == "video" { "🎬" } else { "🖼️" };
        let kb = (item.size_bytes as f64) / 1024.0;
        println!(
            "   {icon} [{}] {} ({:.1} KB) -> {}",
            item.kind, item.filename, kb, item.path
        );
    }
    Ok(())
}
