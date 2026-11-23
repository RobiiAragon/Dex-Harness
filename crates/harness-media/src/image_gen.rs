use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use harness_core::{DexEvent, EventBus};
use harness_extensions::ExtensionSupervisor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::comfyui::{
    inject_workflow_placeholders, load_workflow_template, ComfyUiClient, DEFAULT_WORKFLOWS_DIR,
};
use crate::error::MediaError;
use crate::image_ops::{
    inspect_image, lanczos_upscale, ImageProcessResult, DEFAULT_IMAGES_DIR,
};

const GEN_TIMEOUT_SECS: u64 = 300;

/// Visual generation backend selector.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImageBackend {
    #[default]
    Auto,
    Mlx,
    ComfyUi,
    Http,
}

impl ImageBackend {
    pub fn from_str_loose(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "mlx" | "mflux" | "mlx-gen" => Self::Mlx,
            "comfyui" | "comfy" => Self::ComfyUi,
            "http" | "openai" => Self::Http,
            _ => Self::Auto,
        }
    }
}

/// Request specification for generating an image.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenRequest {
    pub prompt: String,
    pub negative_prompt: Option<String>,
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    pub seed: Option<u64>,
    pub backend: ImageBackend,
    pub workflow: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub output_path: Option<PathBuf>,
}

impl Default for ImageGenRequest {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            negative_prompt: None,
            width: 1024,
            height: 1024,
            steps: 20,
            seed: None,
            backend: ImageBackend::Auto,
            workflow: None,
            model: None,
            output_path: None,
        }
    }
}

/// Outcome of an image generation pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenResult {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub backend: String,
    pub prompt: String,
    pub seed: u64,
}

fn current_epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
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

async fn is_binary_in_path(binary: &str) -> bool {
    harness_core::find_executable_in_enriched_path(binary).is_some()
}

/// Generates an image using Apple Silicon MLX (`mlx-gen` / `mflux`), ComfyUI, or an OpenAI-compatible HTTP endpoint.
pub async fn generate_image(
    req: ImageGenRequest,
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<&EventBus>,
) -> Result<ImageGenResult, MediaError> {
    let prompt = req.prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(MediaError::InvalidInput {
            reason: "Image generation prompt cannot be empty".to_string(),
        });
    }

    // Resolve destination directory safely: honor caller-provided absolute `output_path` without ever touching `/`
    let explicit_dest = req
        .output_path
        .as_ref()
        .filter(|p| !p.as_os_str().is_empty())
        .map(harness_core::resolve_safe_path);
    let out_dir = match explicit_dest.as_ref().and_then(|p| p.parent()) {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => harness_core::resolve_dex_subpath(DEFAULT_IMAGES_DIR),
    };

    let ts = current_epoch_millis();
    let seed = req.seed.unwrap_or((ts % 2_147_483_647) as u64);
    let task_id = format!("img_{ts}");

    // Pre-flight check BEFORE creating output directories when in Auto mode so missing backends fail fast cleanly
    if req.backend == ImageBackend::Auto {
        wake_extension_if_enabled(supervisor.as_ref(), "mlx-gen").await;
        let mlx_url = mlx_base_url();
        let has_mlx = is_http_reachable(&format!("{mlx_url}/health")).await
            || is_binary_in_path("mflux-generate").await;
        let has_comfy = if has_mlx {
            false
        } else {
            wake_extension_if_enabled(supervisor.as_ref(), "comfyui-mcp").await;
            ComfyUiClient::default().is_reachable().await
        };
        let has_http = resolve_http_image_endpoint().is_some();

        if !has_mlx && !has_comfy && !has_http {
            return Err(MediaError::BackendNotInstalled {
                kind: "image".to_string(),
                backend: "mflux / comfyui-mcp".to_string(),
                install_hint: "Install `mflux` (1-click in Models Hub -> Runtimes), start ComfyUI (:8188), or configure `DEX_IMAGE_URL` / `OPENAI_API_KEY`. Do NOT retry `image_generate` in a loop.".to_string(),
            });
        }
    }

    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| MediaError::Io {
            path: out_dir.to_string_lossy().to_string(),
            source: e,
        })?;

    if let Some(bus) = event_bus {
        bus.publish(DexEvent::MediaGenerationProgress {
            task_id: task_id.clone(),
            kind: "image".to_string(),
            step: 0,
            total_steps: req.steps.max(1),
            message: format!("Starting image generation ({:?})...", req.backend),
        });
    }

    let mut result = match req.backend {
        ImageBackend::Mlx => {
            generate_via_mlx(&req, &prompt, seed, &out_dir, &task_id, supervisor.as_ref(), event_bus)
                .await?
        }
        ImageBackend::ComfyUi => {
            generate_via_comfyui(
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
        ImageBackend::Http => {
            let endpoint = resolve_http_image_endpoint().ok_or_else(|| {
                MediaError::BackendNotInstalled {
                    kind: "image".to_string(),
                    backend: "http-images".to_string(),
                    install_hint: "Set `DEX_IMAGE_URL` or `OPENAI_API_KEY`, or install `mflux` (1-click in Models Hub).".to_string(),
                }
            })?;
            generate_via_openai_http(
                &endpoint,
                &req,
                &prompt,
                seed,
                &out_dir,
                &task_id,
                "http-images",
                event_bus,
            )
            .await?
        }
        ImageBackend::Auto => {
            let mlx_url = mlx_base_url();
            if is_http_reachable(&format!("{mlx_url}/health")).await
                || is_binary_in_path("mflux-generate").await
            {
                generate_via_mlx(
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
                let comfy = ComfyUiClient::default();
                if comfy.is_reachable().await {
                    generate_via_comfyui(
                        &req,
                        &prompt,
                        seed,
                        &out_dir,
                        &task_id,
                        supervisor.as_ref(),
                        event_bus,
                    )
                    .await?
                } else if let Some(endpoint) = resolve_http_image_endpoint() {
                    generate_via_openai_http(
                        &endpoint,
                        &req,
                        &prompt,
                        seed,
                        &out_dir,
                        &task_id,
                        "http-images",
                        event_bus,
                    )
                    .await?
                } else {
                    return Err(MediaError::BackendNotInstalled {
                        kind: "image".to_string(),
                        backend: "mflux / comfyui-mcp".to_string(),
                        install_hint: "Install `mflux` (1-click in Models Hub -> Runtimes) or start ComfyUI (:8188). Do NOT retry `image_generate` in a loop.".to_string(),
                    });
                }
            }
        }
    };

    if let Some(target_dest) = explicit_dest {
        if result.path != target_dest && tokio::fs::try_exists(&result.path).await.unwrap_or(false) {
            if tokio::fs::rename(&result.path, &target_dest).await.is_ok() {
                result.path = target_dest;
            } else if tokio::fs::copy(&result.path, &target_dest).await.is_ok() {
                let _ = tokio::fs::remove_file(&result.path).await;
                result.path = target_dest;
            }
        }
    }

    if let Some(bus) = event_bus {
        bus.publish(DexEvent::MediaGenerated {
            kind: "image".to_string(),
            path: result.path.to_string_lossy().to_string(),
            prompt: prompt.chars().take(140).collect(),
            duration_secs: None,
        });
    }

    Ok(result)
}

async fn generate_via_mlx(
    req: &ImageGenRequest,
    prompt: &str,
    seed: u64,
    out_dir: &Path,
    task_id: &str,
    supervisor: Option<&Arc<ExtensionSupervisor>>,
    event_bus: Option<&EventBus>,
) -> Result<ImageGenResult, MediaError> {
    wake_extension_if_enabled(supervisor, "mlx-gen").await;
    let base = mlx_base_url();

    // Prefer HTTP sidecar if live
    if is_http_reachable(&format!("{base}/health")).await {
        let endpoint = format!("{base}/v1/images/generations");
        return generate_via_openai_http(
            &endpoint,
            req,
            prompt,
            seed,
            out_dir,
            task_id,
            "mlx-gen",
            event_bus,
        )
        .await;
    }

    // Fallback to `mflux-generate` CLI with live progress parsing from stdout/stderr
    let Some(mflux_bin) = harness_core::find_executable_in_enriched_path("mflux-generate") else {
        return Err(MediaError::BackendNotInstalled {
            kind: "image".to_string(),
            backend: "mflux".to_string(),
            install_hint: format!(
                "MLX sidecar not reachable at '{base}' and `mflux-generate` CLI is not installed. Install `mflux` with 1 click in Models Hub -> Runtimes."
            ),
        });
    };

    let ts = current_epoch_millis();
    let dest = out_dir.join(format!("mlx_{ts}.png"));
    let model = req
        .model
        .as_deref()
        .unwrap_or("schnell");

    let mut cmd = Command::new(&mflux_bin);
    harness_core::apply_enriched_path(&mut cmd);
    cmd.args([
        "--model",
        model,
        "--prompt",
        prompt,
        "--width",
        &req.width.to_string(),
        "--height",
        &req.height.to_string(),
        "--steps",
        &req.steps.to_string(),
        "--seed",
        &seed.to_string(),
        "--output",
    ])
    .arg(&dest)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    info!(dest = ?dest, model = %model, "Invoking mflux-generate CLI");
    let mut child = cmd.spawn().map_err(|e| MediaError::Subprocess {
        command: "mflux-generate".to_string(),
        message: e.to_string(),
    })?;

    if let Some(stderr) = child.stderr.take() {
        let mut reader = BufReader::new(stderr).lines();
        let total = req.steps.max(1);
        while let Ok(Some(line)) = reader.next_line().await {
            if let Some(current_step) = parse_step_progress(&line, total) {
                if let Some(bus) = event_bus {
                    bus.publish(DexEvent::MediaGenerationProgress {
                        task_id: task_id.to_string(),
                        kind: "image".to_string(),
                        step: current_step,
                        total_steps: total,
                        message: format!("mflux step {current_step}/{total}"),
                    });
                }
            }
        }
    }

    let status = tokio::time::timeout(Duration::from_secs(GEN_TIMEOUT_SECS), child.wait())
        .await
        .map_err(|_| MediaError::Timeout {
            operation: "mflux-generate".to_string(),
            timeout_secs: GEN_TIMEOUT_SECS,
        })?
        .map_err(|e| MediaError::Subprocess {
            command: "mflux-generate".to_string(),
            message: e.to_string(),
        })?;

    if !status.success() {
        return Err(MediaError::Subprocess {
            command: "mflux-generate".to_string(),
            message: format!("Exited with status {status}"),
        });
    }

    let info = inspect_image(&dest).await.ok();
    Ok(ImageGenResult {
        path: dest,
        width: info.as_ref().map(|i| i.width).unwrap_or(req.width),
        height: info.as_ref().map(|i| i.height).unwrap_or(req.height),
        backend: "mlx-mflux".to_string(),
        prompt: prompt.to_string(),
        seed,
    })
}

async fn generate_via_comfyui(
    req: &ImageGenRequest,
    prompt: &str,
    seed: u64,
    out_dir: &Path,
    task_id: &str,
    supervisor: Option<&Arc<ExtensionSupervisor>>,
    event_bus: Option<&EventBus>,
) -> Result<ImageGenResult, MediaError> {
    wake_extension_if_enabled(supervisor, "comfyui-mcp").await;
    let client = ComfyUiClient::default();

    let wf_name = req
        .workflow
        .as_deref()
        .filter(|w| !w.trim().is_empty())
        .unwrap_or("txt2img_default.json");
    let wf_dir = harness_core::resolve_dex_subpath(DEFAULT_WORKFLOWS_DIR);
    let template = load_workflow_template(&wf_dir, wf_name).await?;

    let mut str_vars = HashMap::new();
    str_vars.insert("PROMPT", prompt.to_string());
    str_vars.insert(
        "NEGATIVE_PROMPT",
        req.negative_prompt
            .clone()
            .unwrap_or_else(|| "blurry, low quality, watermark".to_string()),
    );

    let mut num_vars = HashMap::new();
    num_vars.insert("SEED", seed);
    num_vars.insert("WIDTH", u64::from(req.width));
    num_vars.insert("HEIGHT", u64::from(req.height));
    num_vars.insert("STEPS", u64::from(req.steps));

    let workflow = inject_workflow_placeholders(&template, &str_vars, &num_vars)?;
    let prompt_id = client.queue_prompt(workflow, task_id).await?;
    let files = client
        .poll_history_and_download(&prompt_id, out_dir, task_id, "image", req.steps, event_bus)
        .await?;

    let first_path = files.into_iter().next().ok_or_else(|| MediaError::ComfyUi {
        message: format!("ComfyUI prompt {prompt_id} produced no image files"),
    })?;

    let info = inspect_image(&first_path).await.ok();
    Ok(ImageGenResult {
        path: first_path,
        width: info.as_ref().map(|i| i.width).unwrap_or(req.width),
        height: info.as_ref().map(|i| i.height).unwrap_or(req.height),
        backend: "comfyui".to_string(),
        prompt: prompt.to_string(),
        seed,
    })
}

fn resolve_http_image_endpoint() -> Option<String> {
    if let Ok(url) = std::env::var("DEX_IMAGE_URL") {
        let trimmed = url.trim().trim_end_matches('/').to_string();
        if !trimmed.is_empty() {
            return Some(if trimmed.ends_with("/v1/images/generations") {
                trimmed
            } else {
                format!("{trimmed}/v1/images/generations")
            });
        }
    }
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if !key.trim().is_empty() {
            return Some("https://api.openai.com/v1/images/generations".to_string());
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
async fn generate_via_openai_http(
    endpoint: &str,
    req: &ImageGenRequest,
    prompt: &str,
    seed: u64,
    out_dir: &Path,
    task_id: &str,
    backend_label: &str,
    event_bus: Option<&EventBus>,
) -> Result<ImageGenResult, MediaError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(GEN_TIMEOUT_SECS))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let model = req
        .model
        .clone()
        .or_else(|| std::env::var("DEX_IMAGE_MODEL").ok())
        .unwrap_or_else(|| "gpt-image-1".to_string());

    let payload = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "size": format!("{}x{}", req.width, req.height),
        "n": 1,
        "steps": req.steps,
        "seed": seed,
        "response_format": "b64_json",
    });

    debug!(endpoint = %endpoint, model = %model, "Dispatching HTTP image generation");
    let mut http_req = client.post(endpoint).json(&payload);
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if !key.trim().is_empty() {
            http_req = http_req.bearer_auth(key);
        }
    }

    let resp = tokio::time::timeout(Duration::from_secs(GEN_TIMEOUT_SECS), http_req.send())
        .await
        .map_err(|_| MediaError::Timeout {
            operation: "http image generation".to_string(),
            timeout_secs: GEN_TIMEOUT_SECS,
        })?
        .map_err(|e| MediaError::Http {
            url: endpoint.to_string(),
            source: e,
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(MediaError::BackendUnavailable {
            kind: "image".to_string(),
            reason: format!("Endpoint '{endpoint}' returned HTTP {status}: {body}"),
        });
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| MediaError::Http {
        url: endpoint.to_string(),
        source: e,
    })?;

    let bytes = if let Some(b64) = json.pointer("/data/0/b64_json").and_then(|v| v.as_str()) {
        decode_base64(b64)?
    } else if let Some(img_url) = json.pointer("/data/0/url").and_then(|v| v.as_str()) {
        client
            .get(img_url)
            .send()
            .await
            .map_err(|e| MediaError::Http {
                url: img_url.to_string(),
                source: e,
            })?
            .bytes()
            .await
            .map_err(|e| MediaError::Http {
                url: img_url.to_string(),
                source: e,
            })?
            .to_vec()
    } else {
        return Err(MediaError::BackendUnavailable {
            kind: "image".to_string(),
            reason: "HTTP image response lacked `data[0].b64_json` or `data[0].url`".to_string(),
        });
    };

    let ts = current_epoch_millis();
    let dest = out_dir.join(format!("img_{ts}.png"));
    tokio::fs::write(&dest, &bytes)
        .await
        .map_err(|e| MediaError::Io {
            path: dest.to_string_lossy().to_string(),
            source: e,
        })?;

    if let Some(bus) = event_bus {
        bus.publish(DexEvent::MediaGenerationProgress {
            task_id: task_id.to_string(),
            kind: "image".to_string(),
            step: req.steps.max(1),
            total_steps: req.steps.max(1),
            message: "Image saved".to_string(),
        });
    }

    let info = inspect_image(&dest).await.ok();
    Ok(ImageGenResult {
        path: dest,
        width: info.as_ref().map(|i| i.width).unwrap_or(req.width),
        height: info.as_ref().map(|i| i.height).unwrap_or(req.height),
        backend: backend_label.to_string(),
        prompt: prompt.to_string(),
        seed,
    })
}

/// Upscales an image via `mlx-gen` (SeedVR2) or ComfyUI (`upscale_default.json`),
/// falling back seamlessly to deterministic high-quality Lanczos3 in pure Rust.
pub async fn upscale_image(
    path: &Path,
    scale: u32,
    backend: ImageBackend,
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<&EventBus>,
) -> Result<ImageProcessResult, MediaError> {
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        return Err(MediaError::InvalidInput {
            reason: format!("Image to upscale does not exist: '{}'", path.display()),
        });
    }
    if !(2..=8).contains(&scale) {
        return Err(MediaError::InvalidInput {
            reason: format!("Scale factor must be between 2 and 8 (got {scale})"),
        });
    }

    let out_dir = harness_core::resolve_dex_subpath(DEFAULT_IMAGES_DIR);
    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| MediaError::Io {
            path: out_dir.to_string_lossy().to_string(),
            source: e,
        })?;

    let task_id = format!("upscale_{}", current_epoch_millis());

    // 1. Try MLX-Gen neural upscaler if requested or reachable
    if matches!(backend, ImageBackend::Auto | ImageBackend::Mlx) {
        wake_extension_if_enabled(supervisor.as_ref(), "mlx-gen").await;
        let base = mlx_base_url();
        if is_http_reachable(&format!("{base}/health")).await {
            let endpoint = format!("{base}/v1/images/upscale");
            if let Ok(res) = try_mlx_http_upscale(&endpoint, path, scale, &out_dir).await {
                emit_upscaled(event_bus, &res, scale);
                return Ok(res);
            }
        }
    }

    // 2. Try ComfyUI upscale workflow if requested or reachable
    if matches!(backend, ImageBackend::Auto | ImageBackend::ComfyUi) {
        wake_extension_if_enabled(supervisor.as_ref(), "comfyui-mcp").await;
        let comfy = ComfyUiClient::default();
        if comfy.is_reachable().await {
            match try_comfy_upscale(&comfy, path, scale, &out_dir, &task_id, event_bus).await {
                Ok(res) => {
                    emit_upscaled(event_bus, &res, scale);
                    return Ok(res);
                }
                Err(e) => {
                    warn!(error = %e, "ComfyUI upscale failed; falling back to Lanczos3");
                }
            }
        }
    }

    // 3. Deterministic pure-Rust Lanczos3 fallback
    let res = lanczos_upscale(path, scale, None).await?;
    emit_upscaled(event_bus, &res, scale);
    Ok(res)
}

fn emit_upscaled(event_bus: Option<&EventBus>, res: &ImageProcessResult, scale: u32) {
    if let Some(bus) = event_bus {
        bus.publish(DexEvent::MediaGenerated {
            kind: "image".to_string(),
            path: res.output_path.to_string_lossy().to_string(),
            prompt: format!("Upscaled {scale}x via {}", res.backend),
            duration_secs: None,
        });
    }
}

async fn try_mlx_http_upscale(
    endpoint: &str,
    path: &Path,
    scale: u32,
    out_dir: &Path,
) -> Result<ImageProcessResult, MediaError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(GEN_TIMEOUT_SECS))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let payload = serde_json::json!({
        "image_path": path.to_string_lossy(),
        "scale": scale,
    });
    let resp = client
        .post(endpoint)
        .json(&payload)
        .send()
        .await
        .map_err(|e| MediaError::Http {
            url: endpoint.to_string(),
            source: e,
        })?;
    if !resp.status().is_success() {
        return Err(MediaError::BackendUnavailable {
            kind: "upscale".to_string(),
            reason: format!("HTTP {}", resp.status()),
        });
    }

    let bytes = resp.bytes().await.map_err(|e| MediaError::Http {
        url: endpoint.to_string(),
        source: e,
    })?;
    let dest = out_dir.join(format!("mlx_up{scale}x_{}.png", current_epoch_millis()));
    tokio::fs::write(&dest, &bytes)
        .await
        .map_err(|e| MediaError::Io {
            path: dest.to_string_lossy().to_string(),
            source: e,
        })?;
    let info = inspect_image(&dest).await?;
    Ok(ImageProcessResult {
        output_path: dest,
        width: info.width,
        height: info.height,
        format: info.format,
        size_bytes: info.size_bytes,
        backend: "mlx-gen".to_string(),
    })
}

async fn try_comfy_upscale(
    client: &ComfyUiClient,
    path: &Path,
    scale: u32,
    out_dir: &Path,
    task_id: &str,
    event_bus: Option<&EventBus>,
) -> Result<ImageProcessResult, MediaError> {
    let wf_dir = harness_core::resolve_dex_subpath(DEFAULT_WORKFLOWS_DIR);
    let template = load_workflow_template(&wf_dir, "upscale_default.json").await?;
    let mut str_vars = HashMap::new();
    str_vars.insert("IMAGE_PATH", path.to_string_lossy().to_string());
    let mut num_vars = HashMap::new();
    num_vars.insert("SCALE", u64::from(scale));

    let workflow = inject_workflow_placeholders(&template, &str_vars, &num_vars)?;
    let prompt_id = client.queue_prompt(workflow, task_id).await?;
    let files = client
        .poll_history_and_download(&prompt_id, out_dir, task_id, "upscale", 10, event_bus)
        .await?;
    let dest = files.into_iter().next().ok_or_else(|| MediaError::ComfyUi {
        message: "ComfyUI upscale returned no output files".to_string(),
    })?;
    let info = inspect_image(&dest).await?;
    Ok(ImageProcessResult {
        output_path: dest,
        width: info.width,
        height: info.height,
        format: info.format,
        size_bytes: info.size_bytes,
        backend: "comfyui".to_string(),
    })
}

fn parse_step_progress(line: &str, total_steps: u32) -> Option<u32> {
    // Parse patterns like "5/20" or "50%"
    for token in line.split_whitespace() {
        if let Some((a, b)) = token.split_once('/') {
            let clean_a = a.trim_matches(|c: char| !c.is_ascii_digit());
            let clean_b = b.trim_matches(|c: char| !c.is_ascii_digit());
            if let (Ok(step), Ok(tot)) = (clean_a.parse::<u32>(), clean_b.parse::<u32>()) {
                if tot > 0 && step <= tot {
                    return Some(step);
                }
            }
        }
        if let Some(pct_str) = token.strip_suffix('%') {
            let clean = pct_str.trim_matches(|c: char| !c.is_ascii_digit());
            if let Ok(pct) = clean.parse::<u32>() {
                if pct <= 100 {
                    return Some(((pct * total_steps) / 100).min(total_steps));
                }
            }
        }
    }
    None
}

fn decode_base64(input: &str) -> Result<Vec<u8>, MediaError> {
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
                return Err(MediaError::InvalidInput {
                    reason: "Invalid base64 character in image payload".to_string(),
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
    fn test_parse_step_progress_and_base64_decode() {
        assert_eq!(parse_step_progress("Denoising 7/20 [===>]", 20), Some(7));
        assert_eq!(parse_step_progress("Progress: 50%", 20), Some(10));
        assert_eq!(
            decode_base64("RGV4IE1lZGlh").expect("valid base64"),
            b"Dex Media"
        );
    }
}
