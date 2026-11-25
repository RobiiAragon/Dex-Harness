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
                install_hint: "Install `mflux` (1-click in Models Hub -> Runtimes), start ComfyUI (:8188), or configure `DEX_IMAGE_URL` / `OPENAI_API_KEY`. Do NOT retry `image_generate` in a loop. [dex-install:mflux]".to_string(),
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
                    install_hint: "Set `DEX_IMAGE_URL` or `OPENAI_API_KEY`, or install `mflux` (1-click in Models Hub). [dex-install:mflux]".to_string(),
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
                        install_hint: "Install `mflux` (1-click in Models Hub -> Runtimes) or start ComfyUI (:8188). Do NOT retry `image_generate` in a loop. [dex-install:mflux]".to_string(),
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

/// Resolved CLI binary and argument vector for an `mflux` generation pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MfluxInvocation {
    pub binary: &'static str,
    pub args: Vec<String>,
    pub display_model: String,
}

/// Default local image model for this Mac: Z-Image Turbo (best quality, ~4.5 GB peak) when
/// there are at least 16 GB of RAM, otherwise FLUX.1-schnell 4-bit.
pub async fn default_mflux_model() -> &'static str {
    let total_gb = tokio::task::spawn_blocking(harness_core::HardwareProfile::detect_live_memory)
        .await
        .map(|s| s.total_memory_gb)
        .unwrap_or(0.0);
    if total_gb >= 15.5 {
        "z-image-turbo"
    } else {
        "flux-schnell-4bit"
    }
}

/// Peak memory estimate (MB) of an `mflux` run, used to unload other models first if needed.
pub fn mflux_ram_estimate_mb(display_model: &str) -> u64 {
    let lower = display_model.to_lowercase();
    if lower.contains("qwen-image") {
        24 * 1024
    } else if lower.contains("dev") || lower.contains("flux.2") || lower.contains("klein") {
        12 * 1024
    } else {
        7 * 1024
    }
}

/// Distilled models need only a few steps; more just costs time (and can hurt quality).
fn distilled_step_cap(display_model: &str) -> Option<u32> {
    let lower = display_model.to_lowercase();
    if lower.contains("schnell") {
        Some(4)
    } else if lower.contains("z-image turbo") {
        Some(9)
    } else {
        None
    }
}

/// Builds an `mflux` CLI invocation aligned with `mflux 0.23+` capabilities:
/// - Defaults to `dhairyashil/FLUX.1-schnell-mflux-4bit` (`--base-model schnell`, open ungated 4-bit, zero HF token required).
/// - Never forwards `--negative-prompt` to FLUX / Z-Image Turbo models where `mflux` ignores it.
/// - Passes `--negative-prompt` only for models whose CLI honors it (e.g. `qwen-image`).
pub fn resolve_mflux_invocation(
    req: &ImageGenRequest,
    prompt: &str,
    seed: u64,
    dest: &Path,
) -> MfluxInvocation {
    let raw_model = req
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("flux-schnell-4bit");
    let lower = raw_model.to_lowercase();

    let (binary, model_arg, base_model, quantize, supports_negative, display_model) =
        match lower.as_str() {
            "schnell"
            | "flux-schnell-4bit"
            | "flux.1-schnell"
            | "flux1-schnell"
            | "dhairyashil/flux.1-schnell-mflux-4bit" => (
                "mflux-generate",
                "dhairyashil/FLUX.1-schnell-mflux-4bit".to_string(),
                Some("schnell"),
                None,
                false,
                "FLUX.1-schnell (4-bit)".to_string(),
            ),
            "z-image-turbo"
            | "z-image"
            | "zimage-turbo"
            | "filipstrand/z-image-turbo-mflux-4bit" => (
                "mflux-generate-z-image-turbo",
                "filipstrand/Z-Image-Turbo-mflux-4bit".to_string(),
                Some("z-image-turbo"),
                None,
                false,
                "Z-Image Turbo (4-bit)".to_string(),
            ),
            "flux2-klein-4b" | "klein-4b" | "flux2" => (
                "mflux-generate-flux2",
                "flux2-klein-4b".to_string(),
                None,
                Some("4"),
                false,
                "FLUX.2 Klein 4B (4-bit)".to_string(),
            ),
            "qwen-image" | "qwen" | "qwen-image-4bit" => (
                "mflux-generate-qwen",
                "qwen-image".to_string(),
                None,
                Some("4"),
                true,
                "Qwen-Image (4-bit)".to_string(),
            ),
            "dev" | "flux-dev-4bit" | "flux.1-dev" | "flux1-dev" => (
                "mflux-generate",
                "dev".to_string(),
                None,
                Some("4"),
                false,
                "FLUX.1-dev (4-bit)".to_string(),
            ),
            _ => {
                let already_quantized = lower.contains("4bit")
                    || lower.contains("8bit")
                    || lower.contains("6bit")
                    || lower.contains("-q4")
                    || lower.contains("-q8")
                    || lower.contains("fp8")
                    || lower.contains("mflux");
                let is_qwen = lower.contains("qwen");
                let is_z_turbo = lower.contains("z-image-turbo");
                let bin = if is_z_turbo {
                    "mflux-generate-z-image-turbo"
                } else if is_qwen {
                    "mflux-generate-qwen"
                } else {
                    "mflux-generate"
                };
                (
                    bin,
                    raw_model.to_string(),
                    None,
                    if already_quantized { None } else { Some("4") },
                    is_qwen,
                    raw_model.to_string(),
                )
            }
        };

    let mut args = vec!["--model".to_string(), model_arg];
    if let Some(bm) = base_model {
        args.push("--base-model".to_string());
        args.push(bm.to_string());
    }
    if let Some(q) = quantize {
        args.push("--quantize".to_string());
        args.push(q.to_string());
    }
    args.push("--prompt".to_string());
    args.push(prompt.to_string());
    if supports_negative {
        if let Some(neg) = req
            .negative_prompt
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            args.push("--negative-prompt".to_string());
            args.push(neg.to_string());
        }
    }
    args.push("--width".to_string());
    args.push(req.width.to_string());
    args.push("--height".to_string());
    args.push(req.height.to_string());
    let steps = match distilled_step_cap(&display_model) {
        Some(cap) => req.steps.clamp(1, cap),
        None => req.steps.max(1),
    };
    args.push("--steps".to_string());
    args.push(steps.to_string());
    args.push("--seed".to_string());
    args.push(seed.to_string());
    args.push("--output".to_string());
    args.push(dest.to_string_lossy().to_string());

    MfluxInvocation {
        binary,
        args,
        display_model,
    }
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

    // No model picked: use the best one this Mac can run.
    let defaulted;
    let req = if req.model.as_deref().map(str::trim).filter(|m| !m.is_empty()).is_none() {
        let mut r = req.clone();
        r.model = Some(default_mflux_model().await.to_string());
        defaulted = r;
        &defaulted
    } else {
        req
    };

    let ts = current_epoch_millis();
    let dest = out_dir.join(format!("mlx_{ts}.png"));
    let spec = resolve_mflux_invocation(req, prompt, seed, &dest);

    // Unload idle models (e.g. the chat model) if this one would not fit in memory alongside them.
    if let Some(sup) = supervisor {
        sup.make_room("mflux", &spec.display_model, mflux_ram_estimate_mb(&spec.display_model))
            .await;
    }

    let Some(mflux_bin) = harness_core::find_executable_in_enriched_path(spec.binary)
        .or_else(|| harness_core::find_executable_in_enriched_path("mflux-generate"))
    else {
        return Err(MediaError::BackendNotInstalled {
            kind: "image".to_string(),
            backend: "mflux".to_string(),
            install_hint: format!(
                "MLX sidecar not reachable at '{base}' and `mflux-generate` CLI is not installed. Install `mflux` with 1 click right here or in Models Hub -> Runtimes. [dex-install:mflux]"
            ),
        });
    };

    let mut cmd = Command::new(&mflux_bin);
    harness_core::apply_enriched_path(&mut cmd);
    cmd.args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    info!(
        bin = ?mflux_bin,
        dest = ?dest,
        model = %spec.display_model,
        "Invoking mflux CLI"
    );
    let mut child = cmd.spawn().map_err(|e| MediaError::Subprocess {
        command: spec.binary.to_string(),
        message: e.to_string(),
    })?;

    let total_steps = spec
        .args
        .iter()
        .position(|a| a == "--steps")
        .and_then(|i| spec.args.get(i + 1))
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or_else(|| req.steps.max(1));
    let bus_out = event_bus.cloned();
    let bus_err = event_bus.cloned();
    let tid_out = task_id.to_string();
    let tid_err = task_id.to_string();

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let stdout_task = tokio::spawn(async move {
        let mut lines_buf = Vec::new();
        if let Some(pipe) = stdout_pipe {
            let mut reader = BufReader::new(pipe).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let clean = line.trim().to_string();
                if clean.is_empty() {
                    continue;
                }
                if let Some(step) = parse_step_progress(&clean, total_steps) {
                    if let Some(ref bus) = bus_out {
                        bus.publish(DexEvent::MediaGenerationProgress {
                            task_id: tid_out.clone(),
                            kind: "image".to_string(),
                            step,
                            total_steps,
                            message: format!("mflux step {step}/{total_steps}"),
                        });
                    }
                } else if clean.to_lowercase().contains("downloading") {
                    if let Some(ref bus) = bus_out {
                        bus.publish(DexEvent::MediaGenerationProgress {
                            task_id: tid_out.clone(),
                            kind: "image".to_string(),
                            step: 0,
                            total_steps,
                            message: clean.clone(),
                        });
                    }
                }
                if lines_buf.len() >= 60 {
                    lines_buf.remove(0);
                }
                lines_buf.push(clean);
            }
        }
        lines_buf
    });

    let stderr_task = tokio::spawn(async move {
        let mut lines_buf = Vec::new();
        if let Some(pipe) = stderr_pipe {
            let mut reader = BufReader::new(pipe).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let clean = line.trim().to_string();
                if clean.is_empty() {
                    continue;
                }
                if let Some(step) = parse_step_progress(&clean, total_steps) {
                    if let Some(ref bus) = bus_err {
                        bus.publish(DexEvent::MediaGenerationProgress {
                            task_id: tid_err.clone(),
                            kind: "image".to_string(),
                            step,
                            total_steps,
                            message: format!("mflux step {step}/{total_steps}"),
                        });
                    }
                }
                if lines_buf.len() >= 60 {
                    lines_buf.remove(0);
                }
                lines_buf.push(clean);
            }
        }
        lines_buf
    });

    let wait_res =
        tokio::time::timeout(Duration::from_secs(GEN_TIMEOUT_SECS), child.wait()).await;
    let status = match wait_res {
        Ok(Ok(st)) => st,
        Ok(Err(e)) => {
            return Err(MediaError::Subprocess {
                command: spec.binary.to_string(),
                message: e.to_string(),
            });
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(MediaError::Timeout {
                operation: spec.binary.to_string(),
                timeout_secs: GEN_TIMEOUT_SECS,
            });
        }
    };

    let stdout_lines = stdout_task.await.unwrap_or_default();
    let stderr_lines = stderr_task.await.unwrap_or_default();

    if !status.success() {
        let mut combined = String::new();
        if !stderr_lines.is_empty() {
            combined.push_str("stderr:\n");
            combined.push_str(&stderr_lines.join("\n"));
        }
        if !stdout_lines.is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str("stdout:\n");
            combined.push_str(&stdout_lines.join("\n"));
        }
        let lower_err = combined.to_lowercase();
        let auth_note = if lower_err.contains("401 unauthorized") || lower_err.contains("gated") {
            "Hugging Face 401 Unauthorized (gated repo). Use an open 4-bit model like 'FLUX.1-schnell (4-bit · No Token)' or 'Z-Image Turbo (4-bit)', or run `hf auth login`.\n"
        } else {
            ""
        };
        let detail = if combined.is_empty() {
            format!("Exited with status {status}")
        } else {
            format!("{auth_note}Exited with status {status}:\n{combined}")
        };
        return Err(MediaError::Subprocess {
            command: spec.binary.to_string(),
            message: detail,
        });
    }

    let info = inspect_image(&dest).await.ok();
    Ok(ImageGenResult {
        path: dest,
        width: info.as_ref().map(|i| i.width).unwrap_or(req.width),
        height: info.as_ref().map(|i| i.height).unwrap_or(req.height),
        backend: format!("mlx-mflux ({})", spec.display_model),
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

    #[test]
    fn test_resolve_mflux_invocation_presets_and_negative_prompt_gating() {
        let dest = Path::new("/tmp/out.png");

        // 1. Default FLUX.1-schnell (4-bit, open ungated, MUST NOT pass --negative-prompt)
        let req_schnell = ImageGenRequest {
            prompt: "a futuristic studio".to_string(),
            negative_prompt: Some("blurry".to_string()),
            width: 512,
            height: 512,
            steps: 4,
            model: Some("flux-schnell-4bit".to_string()),
            ..ImageGenRequest::default()
        };
        let inv = resolve_mflux_invocation(&req_schnell, &req_schnell.prompt, 42, dest);
        assert_eq!(inv.binary, "mflux-generate");
        assert!(inv.args.contains(&"dhairyashil/FLUX.1-schnell-mflux-4bit".to_string()));
        assert!(inv.args.contains(&"--base-model".to_string()));
        assert!(inv.args.contains(&"schnell".to_string()));
        assert!(!inv.args.contains(&"--negative-prompt".to_string()));
        assert!(!inv.args.contains(&"--quantize".to_string()));

        // 2. Qwen-Image (4-bit quantized, DOES pass --negative-prompt)
        let req_qwen = ImageGenRequest {
            prompt: "cyberpunk street".to_string(),
            negative_prompt: Some("watermark".to_string()),
            steps: 20,
            model: Some("qwen-image".to_string()),
            ..ImageGenRequest::default()
        };
        let inv_q = resolve_mflux_invocation(&req_qwen, &req_qwen.prompt, 7, dest);
        assert_eq!(inv_q.binary, "mflux-generate-qwen");
        assert!(inv_q.args.contains(&"--quantize".to_string()));
        assert!(inv_q.args.contains(&"4".to_string()));
        assert!(inv_q.args.contains(&"--negative-prompt".to_string()));
        assert!(inv_q.args.contains(&"watermark".to_string()));

        // 3. Z-Image Turbo (open 4-bit, dedicated binary, no --negative-prompt)
        let req_z = ImageGenRequest {
            prompt: "neon owl".to_string(),
            negative_prompt: Some("blurry".to_string()),
            steps: 9,
            model: Some("z-image-turbo".to_string()),
            ..ImageGenRequest::default()
        };
        let inv_z = resolve_mflux_invocation(&req_z, &req_z.prompt, 11, dest);
        assert_eq!(inv_z.binary, "mflux-generate-z-image-turbo");
        assert!(inv_z.args.contains(&"filipstrand/Z-Image-Turbo-mflux-4bit".to_string()));
        assert!(!inv_z.args.contains(&"--negative-prompt".to_string()));

        // 4. Distilled models cap the default 20 steps (Z-Image Turbo → 9, schnell → 4).
        let req_z20 = ImageGenRequest {
            steps: 20,
            model: Some("z-image-turbo".to_string()),
            ..ImageGenRequest::default()
        };
        let inv_z20 = resolve_mflux_invocation(&req_z20, "owl", 1, dest);
        let steps_at = inv_z20.args.iter().position(|a| a == "--steps").expect("steps flag");
        assert_eq!(inv_z20.args[steps_at + 1], "9");
        assert_eq!(mflux_ram_estimate_mb(&inv_z20.display_model), 7 * 1024);
    }
}
