use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use image::imageops::FilterType;
use image::{GenericImageView, ImageFormat, ImageReader};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::MediaError;

pub const DEFAULT_IMAGES_DIR: &str = ".dex/media/images";

/// Basic structural metadata of an image file on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInfo {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub size_bytes: u64,
}

/// Rectangular crop region in pixel coordinates.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub struct CropRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Options for pure-Rust image processing (`resize`, `crop`, format conversion).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageProcessOptions {
    pub resize_width: Option<u32>,
    pub resize_height: Option<u32>,
    pub preserve_aspect: bool,
    pub crop: Option<CropRegion>,
    pub target_format: Option<String>,
    pub output_path: Option<PathBuf>,
}

/// Outcome of a pure-Rust image processing or upscaling operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageProcessResult {
    pub output_path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub size_bytes: u64,
    pub backend: String,
}

/// Parses a `"WxH"` dimension string (e.g. `"1024x768"`) into `(width, height)`.
pub fn parse_dimensions_str(raw: &str) -> Result<(u32, u32), MediaError> {
    let lower = raw.trim().to_lowercase();
    let mut parts = lower.split('x');
    let w_str = parts.next().ok_or_else(|| MediaError::InvalidInput {
        reason: format!("Invalid resize format '{raw}'. Expected WxH (e.g. 512x512)"),
    })?;
    let h_str = parts.next().ok_or_else(|| MediaError::InvalidInput {
        reason: format!("Invalid resize format '{raw}'. Expected WxH (e.g. 512x512)"),
    })?;
    if parts.next().is_some() {
        return Err(MediaError::InvalidInput {
            reason: format!("Invalid resize format '{raw}'. Expected WxH"),
        });
    }

    let w = w_str.parse::<u32>().map_err(|_| MediaError::InvalidInput {
        reason: format!("Invalid width '{w_str}' in '{raw}'"),
    })?;
    let h = h_str.parse::<u32>().map_err(|_| MediaError::InvalidInput {
        reason: format!("Invalid height '{h_str}' in '{raw}'"),
    })?;
    if w == 0 || h == 0 {
        return Err(MediaError::InvalidInput {
            reason: "Width and height must be greater than zero".to_string(),
        });
    }
    Ok((w, h))
}

fn parse_image_format(fmt: &str) -> Result<(ImageFormat, &'static str), MediaError> {
    match fmt.trim().to_lowercase().as_str() {
        "png" => Ok((ImageFormat::Png, "png")),
        "jpg" | "jpeg" => Ok((ImageFormat::Jpeg, "jpg")),
        "webp" => Ok((ImageFormat::WebP, "webp")),
        other => Err(MediaError::InvalidInput {
            reason: format!("Unsupported image format '{other}'. Supported: png, jpg, webp"),
        }),
    }
}

fn format_name(fmt: Option<ImageFormat>) -> String {
    match fmt {
        Some(ImageFormat::Png) => "png".to_string(),
        Some(ImageFormat::Jpeg) => "jpg".to_string(),
        Some(ImageFormat::WebP) => "webp".to_string(),
        Some(other) => format!("{other:?}").to_lowercase(),
        None => "unknown".to_string(),
    }
}

/// Inspects an image's dimensions, detected format, and file size without blocking Tokio workers.
pub async fn inspect_image(path: &Path) -> Result<ImageInfo, MediaError> {
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        return Err(MediaError::InvalidInput {
            reason: format!("Image file does not exist: '{}'", path.display()),
        });
    }

    let owned = path.to_path_buf();
    let display = path.to_string_lossy().to_string();

    tokio::task::spawn_blocking(move || {
        let size_bytes = std::fs::metadata(&owned).map(|m| m.len()).unwrap_or(0);
        let reader = ImageReader::open(&owned)
            .map_err(|e| MediaError::Io {
                path: owned.to_string_lossy().to_string(),
                source: e,
            })?
            .with_guessed_format()
            .map_err(|e| MediaError::Io {
                path: owned.to_string_lossy().to_string(),
                source: e,
            })?;

        let fmt = format_name(reader.format());
        let (width, height) = reader.into_dimensions().map_err(|e| MediaError::Image {
            path: owned.to_string_lossy().to_string(),
            message: e.to_string(),
        })?;

        Ok(ImageInfo {
            path: owned.to_string_lossy().to_string(),
            width,
            height,
            format: fmt,
            size_bytes,
        })
    })
    .await
    .map_err(|e| MediaError::Image {
        path: display,
        message: format!("Image inspection worker error: {e}"),
    })?
}

/// Runs fast native pure-Rust image operations (crop, resize, convert format) in `spawn_blocking`.
pub async fn process_image(
    path: &Path,
    opts: ImageProcessOptions,
) -> Result<ImageProcessResult, MediaError> {
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        return Err(MediaError::InvalidInput {
            reason: format!("Source image does not exist: '{}'", path.display()),
        });
    }

    let owned_src = path.to_path_buf();
    let display_src = path.to_string_lossy().to_string();

    tokio::task::spawn_blocking(move || {
        let reader = ImageReader::open(&owned_src)
            .map_err(|e| MediaError::Io {
                path: owned_src.to_string_lossy().to_string(),
                source: e,
            })?
            .with_guessed_format()
            .map_err(|e| MediaError::Io {
                path: owned_src.to_string_lossy().to_string(),
                source: e,
            })?;

        let detected_fmt = reader.format().unwrap_or(ImageFormat::Png);
        let mut img = reader.decode().map_err(|e| MediaError::Image {
            path: owned_src.to_string_lossy().to_string(),
            message: e.to_string(),
        })?;

        // 1. Optional crop with strict bounds validation
        if let Some(crop) = opts.crop {
            let (orig_w, orig_h) = img.dimensions();
            if crop.width == 0 || crop.height == 0 {
                return Err(MediaError::InvalidInput {
                    reason: "Crop width and height must be greater than 0".to_string(),
                });
            }
            if crop.x.saturating_add(crop.width) > orig_w
                || crop.y.saturating_add(crop.height) > orig_h
            {
                return Err(MediaError::InvalidInput {
                    reason: format!(
                        "Crop region ({}, {}, {}x{}) exceeds image dimensions ({}x{})",
                        crop.x, crop.y, crop.width, crop.height, orig_w, orig_h
                    ),
                });
            }
            img = img.crop_imm(crop.x, crop.y, crop.width, crop.height);
        }

        // 2. Optional resize (Lanczos3)
        match (opts.resize_width, opts.resize_height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => {
                img = if opts.preserve_aspect {
                    img.resize(w, h, FilterType::Lanczos3)
                } else {
                    img.resize_exact(w, h, FilterType::Lanczos3)
                };
            }
            (Some(w), None) if w > 0 => {
                let (cur_w, cur_h) = img.dimensions();
                let target_h = ((u64::from(cur_h) * u64::from(w)) / u64::from(cur_w.max(1)))
                    .max(1) as u32;
                img = img.resize_exact(w, target_h, FilterType::Lanczos3);
            }
            (None, Some(h)) if h > 0 => {
                let (cur_w, cur_h) = img.dimensions();
                let target_w = ((u64::from(cur_w) * u64::from(h)) / u64::from(cur_h.max(1)))
                    .max(1) as u32;
                img = img.resize_exact(target_w, h, FilterType::Lanczos3);
            }
            _ => {}
        }

        // 3. Determine output format & path
        let (target_fmt, target_ext) = match opts.target_format.as_deref() {
            Some(f) if !f.trim().is_empty() => parse_image_format(f)?,
            _ => match opts
                .output_path
                .as_ref()
                .and_then(|p| p.extension())
                .and_then(|e| e.to_str())
            {
                Some(ext) => parse_image_format(ext)?,
                None => match detected_fmt {
                    ImageFormat::Jpeg => (ImageFormat::Jpeg, "jpg"),
                    ImageFormat::WebP => (ImageFormat::WebP, "webp"),
                    _ => (ImageFormat::Png, "png"),
                },
            },
        };

        let out_path = match opts.output_path {
            Some(p) => harness_core::resolve_safe_path(p),
            None => {
                let dir = harness_core::resolve_dex_subpath(DEFAULT_IMAGES_DIR);
                std::fs::create_dir_all(&dir).map_err(|e| MediaError::Io {
                    path: dir.to_string_lossy().to_string(),
                    source: e,
                })?;
                let stem = owned_src
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("image");
                let ts = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                dir.join(format!("{stem}_proc_{ts}.{target_ext}"))
            }
        };

        if let Some(parent) = out_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| MediaError::Io {
                    path: parent.to_string_lossy().to_string(),
                    source: e,
                })?;
            }
        }

        // JPEG does not support alpha channel: convert to RGB8 before saving
        if target_fmt == ImageFormat::Jpeg {
            let rgb = img.to_rgb8();
            rgb.save_with_format(&out_path, ImageFormat::Jpeg)
                .map_err(|e| MediaError::Image {
                    path: out_path.to_string_lossy().to_string(),
                    message: e.to_string(),
                })?;
        } else {
            img.save_with_format(&out_path, target_fmt)
                .map_err(|e| MediaError::Image {
                    path: out_path.to_string_lossy().to_string(),
                    message: e.to_string(),
                })?;
        }

        let (width, height) = img.dimensions();
        let size_bytes = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
        debug!(path = ?out_path, width, height, format = target_ext, "Processed image natively");

        Ok(ImageProcessResult {
            output_path: out_path,
            width,
            height,
            format: target_ext.to_string(),
            size_bytes,
            backend: "rust-image".to_string(),
        })
    })
    .await
    .map_err(|e| MediaError::Image {
        path: display_src,
        message: format!("Image processing worker error: {e}"),
    })?
}

/// Deterministic high-quality Lanczos3 upscaler used as a native pure-Rust fallback
/// when no neural upscaling sidecar (`mlx-gen` / ComfyUI) is active.
pub async fn lanczos_upscale(
    path: &Path,
    scale: u32,
    output_path: Option<&Path>,
) -> Result<ImageProcessResult, MediaError> {
    if !(2..=8).contains(&scale) {
        return Err(MediaError::InvalidInput {
            reason: format!("Upscale factor must be between 2 and 8 (got {scale})"),
        });
    }

    let info = inspect_image(path).await?;
    let target_w = info.width.saturating_mul(scale);
    let target_h = info.height.saturating_mul(scale);

    let default_out = if let Some(p) = output_path {
        harness_core::resolve_safe_path(p)
    } else {
        let dir = harness_core::resolve_dex_subpath(DEFAULT_IMAGES_DIR);
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| MediaError::Io {
                path: dir.to_string_lossy().to_string(),
                source: e,
            })?;
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("image");
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        dir.join(format!("{stem}_up{scale}x_{ts}.png"))
    };

    let mut res = process_image(
        path,
        ImageProcessOptions {
            resize_width: Some(target_w),
            resize_height: Some(target_h),
            preserve_aspect: false,
            crop: None,
            target_format: Some("png".to_string()),
            output_path: Some(default_out),
        },
    )
    .await?;
    res.backend = "lanczos3-rust".to_string();
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    #[tokio::test]
    async fn test_image_process_crop_resize_convert_and_lanczos_upscale() {
        let temp_dir =
            std::env::temp_dir().join(format!("dex_media_img_test_{}", std::process::id()));
        tokio::fs::create_dir_all(&temp_dir)
            .await
            .expect("create temp dir");

        let src_path = temp_dir.join("synthetic.png");
        let mut img = RgbImage::new(64, 48);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            *pixel = Rgb([(x * 4) as u8, (y * 5) as u8, 128]);
        }
        img.save(&src_path).expect("save synthetic png");

        // 1. Inspect
        let info = inspect_image(&src_path).await.expect("inspect image");
        assert_eq!(info.width, 64);
        assert_eq!(info.height, 48);
        assert_eq!(info.format, "png");

        // 2. Crop + Resize + Convert to WebP
        let webp_out = temp_dir.join("processed.webp");
        let proc_res = process_image(
            &src_path,
            ImageProcessOptions {
                resize_width: Some(32),
                resize_height: Some(32),
                preserve_aspect: false,
                crop: Some(CropRegion {
                    x: 8,
                    y: 4,
                    width: 40,
                    height: 40,
                }),
                target_format: Some("webp".to_string()),
                output_path: Some(webp_out.clone()),
            },
        )
        .await
        .expect("process image to webp");
        assert_eq!(proc_res.width, 32);
        assert_eq!(proc_res.height, 32);
        assert_eq!(proc_res.format, "webp");
        assert!(tokio::fs::try_exists(&webp_out).await.unwrap_or(false));

        // 3. Lanczos3 2x Upscale
        let up_out = temp_dir.join("upscaled_2x.png");
        let up_res = lanczos_upscale(&src_path, 2, Some(&up_out))
            .await
            .expect("lanczos 2x upscale");
        assert_eq!(up_res.width, 128);
        assert_eq!(up_res.height, 96);
        assert_eq!(up_res.backend, "lanczos3-rust");

        // 4. Dimension parser check
        assert_eq!(parse_dimensions_str("1024x768").expect("parse"), (1024, 768));
        assert!(parse_dimensions_str("0x100").is_err());

        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }
}
