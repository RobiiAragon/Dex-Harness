use std::path::{Path, PathBuf};
use std::time::Duration;
use harness_core::{DexEvent, EventBus};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::error::DocsError;

const OCR_TIMEOUT_SECS: u64 = 60;
const SWIFT_COMPILE_TIMEOUT_SECS: u64 = 60;

/// Single recognized text observation block returned by OCR.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OcrBlock {
    pub text: String,
    pub confidence: f32,
}

/// Structured result of an image OCR pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrResult {
    pub text: String,
    pub blocks: Vec<OcrBlock>,
    pub engine: String,
    pub language_hints: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct VisionJsonOutput {
    #[serde(default)]
    text: String,
    #[serde(default)]
    blocks: Vec<OcrBlock>,
}

const VISION_OCR_SWIFT_SOURCE: &str = r#"
import Foundation
import Vision
import AppKit

struct OcrBlock: Encodable {
    let text: String
    let confidence: Float
}

struct OcrPayload: Encodable {
    let text: String
    let blocks: [OcrBlock]
}

guard CommandLine.arguments.count >= 2 else {
    fputs("Usage: dex-vision-ocr <image_path>\n", stderr)
    exit(2)
}

let imagePath = CommandLine.arguments[1]
guard let nsImage = NSImage(contentsOfFile: imagePath) else {
    fputs("Failed to load image at \(imagePath)\n", stderr)
    exit(3)
}

var proposedRect = NSRect(origin: .zero, size: nsImage.size)
guard let rawImage = nsImage.cgImage(forProposedRect: &proposedRect, context: nil, hints: nil) else {
    fputs("Failed to convert NSImage to CGImage\n", stderr)
    exit(4)
}

// Flatten transparency onto white: dark text on a transparent background (PDF exports,
// screenshots of web pages) is otherwise invisible to Vision.
var cgImage = rawImage
let alpha = rawImage.alphaInfo
if alpha != .none && alpha != .noneSkipFirst && alpha != .noneSkipLast,
   let ctx = CGContext(data: nil, width: rawImage.width, height: rawImage.height, bitsPerComponent: 8,
                       bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(),
                       bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) {
    let rect = CGRect(x: 0, y: 0, width: rawImage.width, height: rawImage.height)
    ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
    ctx.fill(rect)
    ctx.draw(rawImage, in: rect)
    if let flat = ctx.makeImage() { cgImage = flat }
}

let request = VNRecognizeTextRequest()
request.recognitionLevel = .accurate
request.usesLanguageCorrection = true
request.recognitionLanguages = ["es-ES", "en-US"]

let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
do {
    try handler.perform([request])
} catch {
    fputs("Vision OCR error: \(error)\n", stderr)
    exit(5)
}

let observations = request.results ?? []
let sorted = observations.sorted { lhs, rhs in
    if abs(lhs.boundingBox.origin.y - rhs.boundingBox.origin.y) > 0.015 {
        return lhs.boundingBox.origin.y > rhs.boundingBox.origin.y
    }
    return lhs.boundingBox.origin.x < rhs.boundingBox.origin.x
}

var blocks: [OcrBlock] = []
var lines: [String] = []
for obs in sorted {
    if let candidate = obs.topCandidates(1).first {
        blocks.append(OcrBlock(text: candidate.string, confidence: candidate.confidence))
        lines.append(candidate.string)
    }
}

let payload = OcrPayload(text: lines.joined(separator: "\n"), blocks: blocks)
let encoder = JSONEncoder()
if let data = try? encoder.encode(payload), let jsonStr = String(data: data, encoding: .utf8) {
    print(jsonStr)
} else {
    fputs("Failed to encode Vision OCR JSON\n", stderr)
    exit(6)
}
"#;

/// Performs offline multilingual OCR on an image using Apple Vision Framework on macOS
/// (compiled once on demand into `.dex/bin/dex-vision-ocr`), with automatic fallback to `tesseract`.
pub async fn run_ocr_on_image(
    image_path: &Path,
    event_bus: Option<&EventBus>,
) -> Result<OcrResult, DocsError> {
    if !tokio::fs::try_exists(image_path).await.unwrap_or(false) {
        return Err(DocsError::InvalidInput {
            reason: format!("Image file does not exist: '{}'", image_path.display()),
        });
    }

    let result = {
        #[cfg(target_os = "macos")]
        {
            match run_apple_vision_ocr(image_path).await {
                Ok(res) => res,
                Err(vision_err) => {
                    warn!(
                        error = %vision_err,
                        "Apple Vision OCR unavailable; trying tesseract fallback"
                    );
                    run_tesseract_ocr(image_path).await.map_err(|tess_err| {
                        DocsError::OcrFailed {
                            path: image_path.to_string_lossy().to_string(),
                            reason: format!(
                                "Apple Vision failed ({vision_err}); tesseract fallback failed ({tess_err})"
                            ),
                        }
                    })?
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            run_tesseract_ocr(image_path).await?
        }
    };

    if let Some(bus) = event_bus {
        let preview: String = result.text.chars().take(120).collect();
        bus.publish(DexEvent::MediaGenerated {
            kind: "ocr".to_string(),
            path: image_path.to_string_lossy().to_string(),
            prompt: preview,
            duration_secs: None,
        });
    }

    Ok(result)
}

#[cfg(target_os = "macos")]
async fn ensure_vision_binary() -> Result<PathBuf, DocsError> {
    let bin_dir = harness_core::home_dex_dir().join("bin");
    // Versioned name: bump when the Swift source changes so stale cached builds are rebuilt.
    let bin_path = bin_dir.join("dex-vision-ocr-v2");

    if tokio::fs::try_exists(&bin_path).await.unwrap_or(false) {
        return Ok(bin_path);
    }

    tokio::fs::create_dir_all(&bin_dir)
        .await
        .map_err(|e| DocsError::Io {
            path: bin_dir.to_string_lossy().to_string(),
            source: e,
        })?;

    let src_path = bin_dir.join("dex_vision_ocr.swift");
    tokio::fs::write(&src_path, VISION_OCR_SWIFT_SOURCE)
        .await
        .map_err(|e| DocsError::Io {
            path: src_path.to_string_lossy().to_string(),
            source: e,
        })?;

    info!(dest = ?bin_path, "Compiling native Apple Vision OCR helper binary once via swiftc");
    let mut swiftc_cmd = Command::new("swiftc");
    harness_core::apply_enriched_path(&mut swiftc_cmd);
    let compile_cmd = swiftc_cmd
        .arg("-O")
        .arg(&src_path)
        .arg("-o")
        .arg(&bin_path)
        .output();

    let output = tokio::time::timeout(
        Duration::from_secs(SWIFT_COMPILE_TIMEOUT_SECS),
        compile_cmd,
    )
    .await
    .map_err(|_| DocsError::Timeout {
        operation: "swiftc vision ocr compilation".to_string(),
        timeout_secs: SWIFT_COMPILE_TIMEOUT_SECS,
    })?
    .map_err(|e| DocsError::OcrFailed {
        path: bin_path.to_string_lossy().to_string(),
        reason: format!("Failed to invoke swiftc: {e}"),
    })?;

    let _ = tokio::fs::remove_file(&src_path).await;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(DocsError::OcrFailed {
            path: bin_path.to_string_lossy().to_string(),
            reason: format!("swiftc compilation failed: {stderr}"),
        });
    }

    Ok(bin_path)
}

#[cfg(target_os = "macos")]
async fn run_apple_vision_ocr(image_path: &Path) -> Result<OcrResult, DocsError> {
    let raw_stdout = match ensure_vision_binary().await {
        Ok(bin_path) => {
            debug!(bin = ?bin_path, image = ?image_path, "Running compiled Apple Vision OCR");
            let out = tokio::time::timeout(
                Duration::from_secs(OCR_TIMEOUT_SECS),
                Command::new(&bin_path).arg(image_path).output(),
            )
            .await
            .map_err(|_| DocsError::Timeout {
                operation: "apple vision ocr".to_string(),
                timeout_secs: OCR_TIMEOUT_SECS,
            })?
            .map_err(|e| DocsError::OcrFailed {
                path: image_path.to_string_lossy().to_string(),
                reason: e.to_string(),
            })?;

            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return Err(DocsError::OcrFailed {
                    path: image_path.to_string_lossy().to_string(),
                    reason: format!("dex-vision-ocr failed ({}): {}", out.status, stderr),
                });
            }
            String::from_utf8_lossy(&out.stdout).to_string()
        }
        Err(compile_err) => {
            // Fallback to interpreted `swift` if `swiftc` failed
            debug!(error = %compile_err, "Falling back to swift interpreter for Vision OCR");
            let temp_script =
                std::env::temp_dir().join(format!("dex_vision_ocr_{}.swift", std::process::id()));
            tokio::fs::write(&temp_script, VISION_OCR_SWIFT_SOURCE)
                .await
                .map_err(|e| DocsError::Io {
                    path: temp_script.to_string_lossy().to_string(),
                    source: e,
                })?;

            let out = tokio::time::timeout(
                Duration::from_secs(OCR_TIMEOUT_SECS),
                Command::new("swift")
                    .arg(&temp_script)
                    .arg(image_path)
                    .output(),
            )
            .await
            .map_err(|_| DocsError::Timeout {
                operation: "swift vision ocr".to_string(),
                timeout_secs: OCR_TIMEOUT_SECS,
            })?
            .map_err(|e| DocsError::OcrFailed {
                path: image_path.to_string_lossy().to_string(),
                reason: e.to_string(),
            })?;

            let _ = tokio::fs::remove_file(&temp_script).await;

            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return Err(DocsError::OcrFailed {
                    path: image_path.to_string_lossy().to_string(),
                    reason: format!("swift OCR failed ({}): {}", out.status, stderr),
                });
            }
            String::from_utf8_lossy(&out.stdout).to_string()
        }
    };

    parse_vision_json(&raw_stdout, image_path)
}

pub(crate) fn parse_vision_json(raw: &str, image_path: &Path) -> Result<OcrResult, DocsError> {
    let parsed: VisionJsonOutput =
        serde_json::from_str(raw.trim()).map_err(|e| DocsError::OcrFailed {
            path: image_path.to_string_lossy().to_string(),
            reason: format!("Malformed Vision OCR JSON output: {e}"),
        })?;

    Ok(OcrResult {
        text: parsed.text.trim().to_string(),
        blocks: parsed.blocks,
        engine: "apple-vision".to_string(),
        language_hints: vec!["es-ES".to_string(), "en-US".to_string()],
    })
}

async fn run_tesseract_ocr(image_path: &Path) -> Result<OcrResult, DocsError> {
    let mut tess_cmd = Command::new("tesseract");
    harness_core::apply_enriched_path(&mut tess_cmd);
    let cmd = tess_cmd
        .arg(image_path)
        .arg("stdout")
        .args(["-l", "spa+eng"])
        .output();

    let output = tokio::time::timeout(Duration::from_secs(OCR_TIMEOUT_SECS), cmd)
        .await
        .map_err(|_| DocsError::Timeout {
            operation: "tesseract ocr".to_string(),
            timeout_secs: OCR_TIMEOUT_SECS,
        })?
        .map_err(|e| DocsError::OcrFailed {
            path: image_path.to_string_lossy().to_string(),
            reason: format!("Failed to invoke `tesseract`: {e}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(DocsError::OcrFailed {
            path: image_path.to_string_lossy().to_string(),
            reason: format!("`tesseract` exited with {}: {}", output.status, stderr),
        });
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let blocks = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| OcrBlock {
            text: l.trim().to_string(),
            confidence: 1.0,
        })
        .collect();

    Ok(OcrResult {
        text,
        blocks,
        engine: "tesseract".to_string(),
        language_hints: vec!["spa".to_string(), "eng".to_string()],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_vision_json_response() {
        let json = r#"{
            "text": "Factura #2026\nTotal: $150.00",
            "blocks": [
                { "text": "Factura #2026", "confidence": 0.98 },
                { "text": "Total: $150.00", "confidence": 0.95 }
            ]
        }"#;

        let res = parse_vision_json(json, Path::new("invoice.png")).expect("parse vision json");
        assert_eq!(res.engine, "apple-vision");
        assert_eq!(res.blocks.len(), 2);
        assert!(res.text.contains("Factura #2026"));
        assert!(res.text.contains("$150.00"));
    }
}
