use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use harness_core::{DexEvent, EventBus};
use harness_extensions::{ExtensionKind, ExtensionSupervisor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::error::ThreeDError;
use crate::mesh_ops::{inspect_mesh, MeshReport, DEFAULT_3D_DIR};

const GEN3D_TIMEOUT_SECS: u64 = 600;

/// Local or HTTP 3D mesh generation backend selector.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Gen3dBackend {
    #[default]
    Auto,
    Trellis,
    Hunyuan3d,
    TripoSr,
    Http,
}

impl Gen3dBackend {
    pub fn from_str_loose(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "trellis" | "trellis2" | "trellis-3d" => Self::Trellis,
            "hunyuan" | "hunyuan3d" | "hunyuan-3d" => Self::Hunyuan3d,
            "triposr" | "tripo" | "tripo-sr" => Self::TripoSr,
            "http" | "custom" => Self::Http,
            _ => Self::Auto,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Trellis => "trellis",
            Self::Hunyuan3d => "hunyuan3d",
            Self::TripoSr => "triposr",
            Self::Http => "http",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::Auto | Self::Trellis | Self::Http => "http://127.0.0.1:8195",
            Self::Hunyuan3d => "http://127.0.0.1:8196",
            Self::TripoSr => "http://127.0.0.1:8197",
        }
    }
}

/// Request specification for generating a 3D `.glb` mesh from an image and/or text prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gen3dRequest {
    pub image_path: Option<String>,
    pub prompt: Option<String>,
    pub backend: Gen3dBackend,
    pub texture_resolution: u32,
    pub remove_background: bool,
}

impl Default for Gen3dRequest {
    fn default() -> Self {
        Self {
            image_path: None,
            prompt: None,
            backend: Gen3dBackend::Auto,
            texture_resolution: 1024,
            remove_background: true,
        }
    }
}

/// Outcome of a 3D generation pass, including the pure-Rust `MeshReport`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gen3dResult {
    pub path: PathBuf,
    pub backend: String,
    pub report: MeshReport,
}

fn current_epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

async fn resolve_endpoint_and_wake(
    backend: Gen3dBackend,
    supervisor: Option<&Arc<ExtensionSupervisor>>,
) -> (String, String) {
    // 1. Explicit environment override wins
    if let Ok(env_url) = std::env::var("DEX_3D_ENDPOINT") {
        let trimmed = env_url.trim().trim_end_matches('/').to_string();
        if !trimmed.is_empty() {
            return (trimmed, backend.label().to_string());
        }
    }

    // 2. Wake `trellis-3d` sidecar via ExtensionSupervisor if registered & enabled
    if matches!(backend, Gen3dBackend::Auto | Gen3dBackend::Trellis) {
        if let Some(sup) = supervisor {
            let reg = sup.load_registry().await;
            if let Some(manifest) = reg.get("trellis-3d") {
                let port = match &manifest.kind {
                    ExtensionKind::LocalSidecar { port, .. } => *port,
                    _ => 8195,
                };
                if manifest.enabled {
                    let _ = sup.start_extension("trellis-3d").await;
                    sup.touch_last_used("trellis-3d").await;
                }
                return (format!("http://127.0.0.1:{port}"), "trellis-3d".to_string());
            }
        }
    }

    (
        backend.default_base_url().to_string(),
        backend.label().to_string(),
    )
}

async fn is_endpoint_reachable(base_url: &str) -> bool {
    let probe = reqwest::Client::builder()
        .timeout(Duration::from_millis(450))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let health = format!("{}/health", base_url.trim_end_matches('/'));
    if probe.get(&health).send().await.is_ok() {
        return true;
    }
    probe.get(base_url).send().await.is_ok()
}

/// Generates a 3D `.glb` mesh from an input image (and/or prompt) via a local 3D sidecar
/// (`trellis-3d`, Hunyuan3D, TripoSR, or custom HTTP endpoint), inspects the saved `.glb`
/// natively in Rust, and emits `MediaGenerationProgress` & `MediaGenerated`.
pub async fn generate_3d_mesh(
    req: Gen3dRequest,
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<&EventBus>,
) -> Result<Gen3dResult, ThreeDError> {
    let clean_image = req
        .image_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let clean_prompt = req
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);

    if clean_image.is_none() && clean_prompt.is_none() {
        return Err(ThreeDError::InvalidInput {
            reason: "Provide at least `image_path` or `prompt` to generate a 3D mesh".to_string(),
        });
    }

    if let Some(ref img) = clean_image {
        if !tokio::fs::try_exists(Path::new(img)).await.unwrap_or(false) {
            return Err(ThreeDError::InvalidInput {
                reason: format!("Conditioning image does not exist: '{img}'"),
            });
        }
    }

    if !(256..=4096).contains(&req.texture_resolution) {
        return Err(ThreeDError::InvalidInput {
            reason: format!(
                "texture_resolution must be between 256 and 4096 (got {})",
                req.texture_resolution
            ),
        });
    }

    let ts = current_epoch_millis();
    let task_id = format!("mesh3d_{ts}");

    let (base_url, resolved_label) =
        resolve_endpoint_and_wake(req.backend, supervisor.as_ref()).await;

    if !is_endpoint_reachable(&base_url).await {
        return Err(ThreeDError::BackendNotInstalled {
            kind: "3d".to_string(),
            backend: format!("{resolved_label} / trellis-3d / hunyuan3d"),
            install_hint: format!(
                "No 3D reconstruction sidecar is reachable at '{base_url}'. Install/start `trellis-3d` in Extensions (or Hunyuan3D/TripoSR), or offer procedural 3D modeling with Blender + `mesh_inspect`. Do NOT retry `mesh_generate_3d`."
            ),
        });
    }

    let out_dir = harness_core::resolve_dex_subpath(DEFAULT_3D_DIR);
    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| ThreeDError::Io {
            path: out_dir.to_string_lossy().to_string(),
            source: e,
        })?;

    if let Some(bus) = event_bus {
        bus.publish(DexEvent::MediaGenerationProgress {
            task_id: task_id.clone(),
            kind: "3d".to_string(),
            step: 0,
            total_steps: 3,
            message: format!("Connecting to 3D backend ({})...", req.backend.label()),
        });
    }

    let endpoint = if base_url.ends_with("/generate") {
        base_url.clone()
    } else {
        format!("{}/v1/3d/generate", base_url.trim_end_matches('/'))
    };

    if let Some(bus) = event_bus {
        bus.publish(DexEvent::MediaGenerationProgress {
            task_id: task_id.clone(),
            kind: "3d".to_string(),
            step: 1,
            total_steps: 3,
            message: format!("Reconstructing 3D mesh via {resolved_label}..."),
        });
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(GEN3D_TIMEOUT_SECS))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    debug!(endpoint = %endpoint, backend = %resolved_label, "Dispatching 3D generation request");

    let send_future = async {
        if let Some(ref img_path) = clean_image {
            let img_bytes = tokio::fs::read(img_path).await.map_err(|e| ThreeDError::Io {
                path: img_path.clone(),
                source: e,
            })?;
            let filename = Path::new(img_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("input.png")
                .to_string();
            let part = reqwest::multipart::Part::bytes(img_bytes).file_name(filename);
            let mut form = reqwest::multipart::Form::new()
                .part("image", part)
                .text("backend", req.backend.label().to_string())
                .text("texture_resolution", req.texture_resolution.to_string())
                .text("remove_background", req.remove_background.to_string());
            if let Some(ref p) = clean_prompt {
                form = form.text("prompt", p.clone());
            }
            client
                .post(&endpoint)
                .multipart(form)
                .send()
                .await
                .map_err(|e| ThreeDError::Http {
                    url: endpoint.clone(),
                    source: e,
                })
        } else {
            let payload = serde_json::json!({
                "prompt": clean_prompt,
                "backend": req.backend.label(),
                "texture_resolution": req.texture_resolution,
                "remove_background": req.remove_background,
            });
            client
                .post(&endpoint)
                .json(&payload)
                .send()
                .await
                .map_err(|e| ThreeDError::Http {
                    url: endpoint.clone(),
                    source: e,
                })
        }
    };

    let resp = tokio::time::timeout(Duration::from_secs(GEN3D_TIMEOUT_SECS), send_future)
        .await
        .map_err(|_| ThreeDError::Timeout {
            operation: "3D mesh generation".to_string(),
            timeout_secs: GEN3D_TIMEOUT_SECS,
        })??;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(ThreeDError::BackendUnavailable {
            kind: "3d".to_string(),
            reason: format!("Endpoint '{endpoint}' returned HTTP {status}: {body}"),
        });
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    let dest = out_dir.join(format!("mesh_{ts}.glb"));
    let glb_bytes = if content_type.contains("application/json") {
        let json: serde_json::Value = resp.json().await.map_err(|e| ThreeDError::Http {
            url: endpoint.clone(),
            source: e,
        })?;
        extract_glb_from_json(&client, &json).await?
    } else {
        resp.bytes()
            .await
            .map_err(|e| ThreeDError::Http {
                url: endpoint.clone(),
                source: e,
            })?
            .to_vec()
    };

    if glb_bytes.len() < 12 {
        return Err(ThreeDError::Gltf {
            path: dest.to_string_lossy().to_string(),
            message: "3D backend returned an empty or truncated GLB payload".to_string(),
        });
    }

    tokio::fs::write(&dest, &glb_bytes)
        .await
        .map_err(|e| ThreeDError::Io {
            path: dest.to_string_lossy().to_string(),
            source: e,
        })?;

    if let Some(bus) = event_bus {
        bus.publish(DexEvent::MediaGenerationProgress {
            task_id: task_id.clone(),
            kind: "3d".to_string(),
            step: 2,
            total_steps: 3,
            message: "Inspecting generated GLB geometry...".to_string(),
        });
    }

    let report = inspect_mesh(&dest).await?;
    info!(
        path = ?dest,
        vertices = report.vertex_count,
        triangles = report.triangle_count,
        "Generated and inspected 3D GLB mesh"
    );

    if let Some(bus) = event_bus {
        bus.publish(DexEvent::MediaGenerationProgress {
            task_id,
            kind: "3d".to_string(),
            step: 3,
            total_steps: 3,
            message: format!("3D mesh ready ({} triangles)", report.triangle_count),
        });
        let label_prompt = clean_prompt
            .or_else(|| clean_image.map(|p| format!("Image-to-3D from {p}")))
            .unwrap_or_else(|| "3D mesh".to_string());
        bus.publish(DexEvent::MediaGenerated {
            kind: "3d".to_string(),
            path: dest.to_string_lossy().to_string(),
            prompt: label_prompt.chars().take(140).collect(),
            duration_secs: None,
        });
    }

    Ok(Gen3dResult {
        path: dest,
        backend: resolved_label,
        report,
    })
}

async fn extract_glb_from_json(
    client: &reqwest::Client,
    json: &serde_json::Value,
) -> Result<Vec<u8>, ThreeDError> {
    if let Some(b64) = json
        .get("glb_base64")
        .or_else(|| json.get("b64_glb"))
        .and_then(|v| v.as_str())
    {
        return decode_base64(b64);
    }

    if let Some(url) = json
        .get("url")
        .or_else(|| json.get("glb_url"))
        .and_then(|v| v.as_str())
    {
        let resp = client.get(url).send().await.map_err(|e| ThreeDError::Http {
            url: url.to_string(),
            source: e,
        })?;
        return resp
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| ThreeDError::Http {
                url: url.to_string(),
                source: e,
            });
    }

    if let Some(local_path) = json.get("path").and_then(|v| v.as_str()) {
        return tokio::fs::read(local_path).await.map_err(|e| ThreeDError::Io {
            path: local_path.to_string(),
            source: e,
        });
    }

    Err(ThreeDError::BackendUnavailable {
        kind: "3d".to_string(),
        reason: "3D JSON response lacked `glb_base64`, `url`, or `path`".to_string(),
    })
}

fn decode_base64(input: &str) -> Result<Vec<u8>, ThreeDError> {
    let mut out = Vec::with_capacity((input.len() * 3) / 4);
    let mut buf: u32 = 0;
    let mut bits: u8 = 0;

    for b in input.bytes() {
        if b.is_ascii_whitespace() || b == b'=' {
            continue;
        }
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => {
                return Err(ThreeDError::InvalidInput {
                    reason: "Invalid base64 character in 3D GLB payload".to_string(),
                })
            }
        };
        buf = (buf << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_parsing_and_base64() {
        assert_eq!(Gen3dBackend::from_str_loose("trellis"), Gen3dBackend::Trellis);
        assert_eq!(Gen3dBackend::from_str_loose("Hunyuan3D"), Gen3dBackend::Hunyuan3d);
        assert_eq!(Gen3dBackend::from_str_loose("triposr"), Gen3dBackend::TripoSr);
        assert_eq!(Gen3dBackend::from_str_loose("http"), Gen3dBackend::Http);
        assert_eq!(Gen3dBackend::from_str_loose("unknown"), Gen3dBackend::Auto);
        assert_eq!(decode_base64("Z2xURg==").expect("base64"), b"glTF");
    }
}
