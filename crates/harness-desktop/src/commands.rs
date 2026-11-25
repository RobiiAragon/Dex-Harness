use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use harness_3d::{
    blender_convert_mesh, blender_render_preview, check_blender_status as check_blender_impl,
    generate_3d_mesh as generate_3d_impl, inspect_mesh as inspect_mesh_impl,
    list_3d_models as list_3d_models_impl, register_3d_tools, unity_export_asset,
    unity_inspect_project, write_sample_cube_glb, BlenderConvertResult, BlenderPreviewResult,
    BlenderStatus, Gen3dBackend, Gen3dRequest, Gen3dResult, MeshReport, StoredModel3dItem,
    UnityExportResult, UnityProjectReport, DEFAULT_3D_DIR,
};
use harness_audio::{
    builtin_audio_catalog, delete_voice_profile, list_voice_profiles, load_audio_settings,
    record_fixed_duration, register_audio_tools, save_audio_settings, save_voice_profile,
    synthesize_speech_with_options, AudioModelPreset, AudioStudioSettings, AutoSttEngine,
    ClonedVoiceProfile, RecordingResult, SttOptions, TtsOptions, TtsResult, VoiceRecorderHandle,
    DEFAULT_MUSIC_DIR, DEFAULT_RECORDINGS_DIR, DEFAULT_SFX_DIR, DEFAULT_TTS_DIR,
};
use harness_docs::{
    parse_document as parse_doc_impl, register_docs_tools, run_ocr_on_image, DocumentParseResult,
    OcrResult, DEFAULT_MAX_INLINE_CHARS,
};
use harness_extensions::{
    builtin_catalog, register_enabled_mcp_extensions, ExtensionCategory, ExtensionKind,
    ExtensionManifest, ExtensionRuntimeInfo, ExtensionStatus, ExtensionSupervisor,
    DEFAULT_IDLE_TIMEOUT_SECS,
};
use harness_media::{
    generate_image as generate_image_impl, inspect_video as inspect_video_impl,
    list_visual_media_items, list_workflows, process_image as process_image_impl,
    register_media_tools, upscale_image as upscale_image_impl, ImageBackend, ImageGenRequest,
    ImageGenResult, ImageProcessOptions, ImageProcessResult, VideoMetadata, DEFAULT_WORKFLOWS_DIR,
};
use harness_models::{
    build_launch_spec, compute_group_key, detect_local_servers as detect_servers_impl,
    detect_runtime_binary, detect_runtimes as detect_runtimes_impl, import_local_model,
    infer_capabilities, install_runtime as install_runtime_impl, parse_quantization,
    scan_model_dirs, verify_model_path_integrity, ActiveLoadedModel, DiscoveredModel, HfClient,
    HfGgufFile, HfModelSummary, IntegrityStatus, LoadModelOutcome, LocalServerInfo,
    ModelDirsConfig, ModelDownloader, ModelFormat, ModelMetadata, ModelRegistry, ModelSource,
    OllamaBridge, RuntimeKind, RuntimeStatus, DEX_RUNTIME_SIDECAR_ID, LM_STUDIO_DEFAULT_URL,
    LOCAL_MODEL_STARTUP_TIMEOUT_SECS,
};
use tokio_util::sync::CancellationToken;
use harness_computer::{
    BrowserClickTool, BrowserExtractDomTool, BrowserNavigateTool, BrowserScreenshotTool,
    BrowserSession, BrowserTypeTool, DesktopKeyboardTool, DesktopMouseTool, DesktopScreenshotTool,
};
use harness_core::cookbook::{HardwareProfile, MemorySnapshot, ModelRecommendation};
use harness_core::task::{ManageTasksTool, TaskBoard, TaskItem};
use harness_core::{
    Agent, ContextCompactor, DexEvent, EventBus, ExecutionMode, HookRegistry, LlmProvider,
    LoadSkillTool, ProgressiveSkillLoader, ReActAgent, SpawnSubagentTool, ToolRegistry,
};
use harness_llm::{OllamaProvider, OpenAiProvider};
use harness_tools::{
    EditFileTool, ExecuteShellTool, FindFilesTool, GitWorktreeTool, GrepSearchTool,
    PtyShellTool, ReadFileTool, WriteFileTool,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::oneshot;

use crate::projects::{
    classify_attachment_kind, create_project_on_disk, delete_session_in_dir,
    ensure_project_dex_dirs, inspect_attachment_file, list_sessions_in_dir, load_session_in_dir,
    pick_attachment_files_native, pick_file_dialog_native, pick_folder_dialog_native,
    pick_folder_native, rename_session_in_dir, resolve_sessions_dir, save_session_in_dir,
    AttachmentMeta, ChatSession, ChatSessionSummary, ProjectsResponse, ProjectsStore,
};
use crate::state::{DesktopSecurityHook, DesktopState};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotEntry {
    pub filename: String,
    pub path: String,
    pub data_url: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookbookResponse {
    pub profile: HardwareProfile,
    pub recommendations: Vec<ModelRecommendation>,
    pub report: String,
}

fn to_base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn build_provider(
    provider_type: &str,
    model: &str,
    api_base: Option<&str>,
) -> Result<Arc<dyn LlmProvider>, String> {
    match provider_type.to_lowercase().as_str() {
        "dex-local" | "dex_local" | "dex" | "mlx" | "llamacpp" => {
            let mut p = OpenAiProvider::lm_studio(model);
            if let Some(base) = api_base.filter(|s| !s.trim().is_empty()) {
                p = p.with_base_url(base);
            } else {
                p = p.with_base_url("http://127.0.0.1:8088/v1");
            }
            Ok(Arc::new(p))
        }
        "ollama" => {
            let mut p = OllamaProvider::new(model);
            if let Some(base) = api_base.filter(|s| !s.trim().is_empty()) {
                p = p.with_base_url(base);
            }
            Ok(Arc::new(p))
        }
        "openai" => {
            let mut p = OpenAiProvider::new(model);
            if let Some(base) = api_base.filter(|s| !s.trim().is_empty()) {
                p = p.with_base_url(base);
            }
            Ok(Arc::new(p))
        }
        "lmstudio" | "lm_studio" | "bionic" => {
            let mut p = OpenAiProvider::lm_studio(model);
            if let Some(base) = api_base.filter(|s| !s.trim().is_empty()) {
                p = p.with_base_url(base);
            }
            Ok(Arc::new(p))
        }
        other => Err(format!(
            "Unsupported provider '{}'. Supported: dex-local, ollama, lmstudio, openai",
            other
        )),
    }
}

fn build_tools(
    provider: Arc<dyn LlmProvider>,
    skill_loader: ProgressiveSkillLoader,
    ext_supervisor: Arc<ExtensionSupervisor>,
    event_bus: EventBus,
    workspace_dir: &Path,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    // Surgical Code & File Tools (rooted at active project workspace)
    registry.register(Arc::new(ReadFileTool::new().with_workspace_root(workspace_dir)));
    registry.register(Arc::new(WriteFileTool::new().with_workspace_root(workspace_dir)));
    registry.register(Arc::new(EditFileTool::new().with_workspace_root(workspace_dir)));

    // Search & Repository Navigation
    registry.register(Arc::new(FindFilesTool::new().with_workspace_root(workspace_dir)));
    registry.register(Arc::new(GrepSearchTool::new().with_workspace_root(workspace_dir)));

    // Terminal Execution & PTY
    registry.register(Arc::new(ExecuteShellTool::new().with_workspace_root(workspace_dir)));
    registry.register(Arc::new(PtyShellTool::new().with_workspace_root(workspace_dir)));

    // Git Branch & Worktree Management
    registry.register(Arc::new(GitWorktreeTool::new().with_workspace_root(workspace_dir)));

    // Project & Task Graph Management
    let tasks_path = workspace_dir.join(".dex").join("tasks.json");
    registry.register(Arc::new(
        ManageTasksTool::new(tasks_path.to_string_lossy().to_string())
            .with_event_bus(event_bus.clone()),
    ));

    // Progressive Skills
    registry.register(Arc::new(LoadSkillTool::new(skill_loader)));

    // Browser CDP Automation
    let browser_session = BrowserSession::new();
    registry.register(Arc::new(BrowserNavigateTool::new(browser_session.clone())));
    registry.register(Arc::new(BrowserExtractDomTool::new(browser_session.clone())));
    registry.register(Arc::new(BrowserClickTool::new(browser_session.clone())));
    registry.register(Arc::new(BrowserTypeTool::new(browser_session.clone())));
    registry.register(Arc::new(BrowserScreenshotTool::new(browser_session)));

    // Desktop Automation
    registry.register(Arc::new(DesktopScreenshotTool::new()));
    registry.register(Arc::new(DesktopMouseTool::new()));
    registry.register(Arc::new(DesktopKeyboardTool::new()));

    // Subagents Orchestrator (using isolated snapshot to prevent Arc cycles)
    let master_copy = registry.snapshot();
    registry.register(Arc::new(
        SpawnSubagentTool::new(provider, master_copy).with_event_bus(event_bus.clone()),
    ));

    // Deferred Audio (STT, TTS, Music, SFX), Docs/OCR, Visual Studio (Image/Video), and 3D/GameDev tools
    register_audio_tools(
        &mut registry,
        Some(ext_supervisor.clone()),
        Some(event_bus.clone()),
    );
    register_docs_tools(
        &mut registry,
        Some(ext_supervisor.clone()),
        Some(event_bus.clone()),
    );
    register_media_tools(
        &mut registry,
        Some(ext_supervisor.clone()),
        Some(event_bus.clone()),
    );
    register_3d_tools(&mut registry, Some(ext_supervisor), Some(event_bus.clone()));

    // Dynamic deferred tool discovery (`search_tools`)
    registry.register_search_tools(Some(event_bus));

    registry
}

fn runtime_supports_vision_payload(
    provider: &str,
    model: &str,
    active_local_runtime: Option<&str>,
) -> bool {
    // `mlx_lm.server` (used by `dex-local` for MLX folders) is text-only and raises
    // `ValueError("Only 'text' content type is supported.")` on OpenAI `image_url` blocks.
    // `llama-server` without an explicit `--mmproj` flag also rejects `image_url` payloads.
    if provider.eq_ignore_ascii_case("dex-local")
        || provider.eq_ignore_ascii_case("dex_local")
        || matches!(active_local_runtime, Some("mlx") | Some("llamacpp"))
    {
        return false;
    }

    let caps = infer_capabilities(model, None, false);
    if caps.iter().any(|c| c == "Vision") {
        return true;
    }
    let lower = model.to_lowercase();
    provider.eq_ignore_ascii_case("openai")
        || lower.contains("gpt-4o")
        || lower.contains("gpt-4.1")
        || lower.contains("claude-3")
        || lower.contains("claude-4")
        || lower.contains("gemini")
        || lower.contains("vision")
        || lower.contains("-vl")
        || lower.contains("vl-")
        || lower.contains("llava")
        || lower.contains("pixtral")
        || lower.contains("gemma-3")
        || lower.contains("gemma-4")
        || lower.contains("qwen3.5")
}

fn is_multimodal_rejection_error(err_msg: &str) -> bool {
    let lower = err_msg.to_lowercase();
    lower.contains("only 'text' content type")
        || lower.contains("image_url")
        || lower.contains("does not support image")
        || lower.contains("multimodal")
        || lower.contains("vision is not supported")
        || lower.contains("expected string, got array")
}

fn image_mime_for_path(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

async fn prepare_attachments_context(
    attachments: &[String],
    include_vision_payload: bool,
) -> (String, Vec<String>, bool) {
    if attachments.is_empty() {
        return (String::new(), Vec::new(), false);
    }

    const MAX_INLINE_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
    let mut lines = vec!["\n\n[Attached Files]".to_string()];
    let mut image_urls = Vec::new();
    let mut had_image_without_vision = false;

    for raw in attachments {
        let p = PathBuf::from(raw);
        let meta = tokio::fs::metadata(&p).await.ok();
        let size_bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let size_str = if size_bytes >= 1024 * 1024 {
            format!("{:.2} MB", size_bytes as f64 / (1024.0 * 1024.0))
        } else {
            format!("{:.1} KB", (size_bytes as f64 / 1024.0).max(0.1))
        };
        let kind = classify_attachment_kind(&p);
        let hint = match kind {
            "image" if include_vision_payload => {
                "use vision input directly or call `ocr_image` for exact text extraction"
            }
            "image" => {
                "active runtime is text-only; call `ocr_image` (activate via `search_tools`) to extract text/contents from this image"
            }
            "document" => "call `doc_parse` (activate via `search_tools`) to extract structured Markdown",
            "mesh_3d" => "call `mesh_inspect` (activate via `search_tools`) to analyze 3D geometry",
            "audio" => "call `audio_transcribe` (activate via `search_tools`) to transcribe",
            "video" => "call `video_inspect` (activate via `search_tools`) to inspect streams",
            _ => "call `read_file` to inspect contents",
        };
        lines.push(format!("- `{}` ({}, {}) — {}", p.display(), kind, size_str, hint));

        if kind == "image" {
            if include_vision_payload && size_bytes > 0 && size_bytes <= MAX_INLINE_IMAGE_BYTES {
                if let Some(mime) = image_mime_for_path(&p) {
                    if let Ok(bytes) = tokio::fs::read(&p).await {
                        let b64 = to_base64(&bytes);
                        image_urls.push(format!("data:{mime};base64,{b64}"));
                    }
                }
            } else if !include_vision_payload {
                had_image_without_vision = true;
            }
        }
    }

    if had_image_without_vision {
        lines.push(
            "\n[Vision Advisory] The currently loaded local server does not accept raw `image_url` payloads. Do NOT guess image contents — call `search_tools` for `ocr_image` and extract text from the attached image path(s), or advise the user to inspect visually via a vision-capable model."
                .to_string(),
        );
    }

    (lines.join("\n"), image_urls, had_image_without_vision)
}

async fn probe_local_port(port: u16) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_millis(200),
            tokio::net::TcpStream::connect(("127.0.0.1", port)),
        )
        .await,
        Ok(Ok(_))
    )
}

/// Builds a live Host Hardware & Multimodal Capabilities Manifest injected into the Agent system prompt
/// so the LLM knows exact hardware specs and which creative engines are READY vs NOT INSTALLED.
async fn build_host_capabilities_manifest(workspace_dir: &Path) -> String {
    let profile = tokio::task::spawn_blocking(HardwareProfile::detect)
        .await
        .unwrap_or_else(|_| HardwareProfile::detect());

    let (blender_status, has_mlx_gen, has_comfy, has_3d_sidecar, has_audiogen_sidecar) = tokio::join!(
        check_blender_impl(),
        probe_local_port(8190),
        probe_local_port(8188),
        probe_local_port(8195),
        probe_local_port(harness_core::DEX_AUDIOGEN_PORT),
    );
    // MusicGen runtime installed in the managed venv => sidecar auto-launches on first use.
    let has_audiogen_runtime = harness_core::detect_audiogen_python().is_some();

    let has_mflux = harness_core::find_executable_in_enriched_path("mflux-generate").is_some();
    let has_http_img = std::env::var("DEX_IMAGE_URL")
        .ok()
        .is_some_and(|s| !s.trim().is_empty())
        || std::env::var("OPENAI_API_KEY")
            .ok()
            .is_some_and(|s| !s.trim().is_empty());
    let has_mlx_whisper = harness_core::find_executable_in_enriched_path("mlx_whisper").is_some();
    let has_parakeet = harness_core::find_executable_in_enriched_path("parakeet-mlx").is_some();
    let has_whisper_cli = harness_core::find_executable_in_enriched_path("whisper-cli").is_some();
    let has_mlx_audio = harness_core::find_executable_in_enriched_path("mlx_audio.tts.generate").is_some();
    let has_3d_env = std::env::var("DEX_3D_ENDPOINT")
        .ok()
        .is_some_and(|s| !s.trim().is_empty());
    let has_audio_env = std::env::var("DEX_AUDIO_GEN_URL")
        .ok()
        .is_some_and(|s| !s.trim().is_empty());

    let blender_line = if blender_status.available {
        format!(
            "READY ({})",
            blender_status.version.as_deref().unwrap_or("Blender installed")
        )
    } else {
        "NOT INSTALLED (install via `brew install --cask blender`)".to_string()
    };

    let image_gen_line = if has_mflux {
        "READY (`mflux-generate` Apple Silicon MLX)".to_string()
    } else if has_mlx_gen {
        "READY (`mlx-gen` local server :8190)".to_string()
    } else if has_comfy {
        "READY (ComfyUI :8188)".to_string()
    } else if has_http_img {
        "READY (HTTP image endpoint)".to_string()
    } else {
        "NOT INSTALLED (install `mflux` with 1 click in Media Studio or Models Hub -> Runtimes, or start ComfyUI :8188)".to_string()
    };

    let stt_line = if has_mlx_whisper && has_parakeet {
        "READY (`mlx_whisper` + `parakeet-mlx`)".to_string()
    } else if has_mlx_whisper {
        "READY (`mlx_whisper`)".to_string()
    } else if has_parakeet {
        "READY (`parakeet-mlx`)".to_string()
    } else if has_whisper_cli {
        "READY (`whisper-cli`)".to_string()
    } else if cfg!(target_os = "macos") {
        "READY (Apple Speech fallback; `mlx-whisper` / `parakeet-mlx` available 1-click in Audio & Docs)".to_string()
    } else {
        "NOT INSTALLED (`mlx-whisper` / `whisper-cli`)".to_string()
    };

    let gen3d_line = if has_3d_sidecar || has_3d_env {
        "READY (TRELLIS / 3D sidecar)".to_string()
    } else {
        "NOT INSTALLED (`trellis-3d` / Hunyuan3D sidecar in Extensions)".to_string()
    };

    let music_line = if has_audio_env {
        "READY (external music/SFX server via `DEX_AUDIO_GEN_URL`)".to_string()
    } else if has_audiogen_sidecar {
        "READY (`dex-audiogen` MusicGen music+SFX sidecar running on :8189; instrumental only, lyrics are not sung)".to_string()
    } else if has_audiogen_runtime {
        "READY (`dex-audiogen` MusicGen music+SFX; sidecar auto-starts on first call, first run downloads weights; instrumental only, lyrics are not sung)".to_string()
    } else {
        "NOT INSTALLED (install 'Music & SFX Gen (MusicGen)' with 1 click in Models Hub -> Runtimes, runtime id `audiogen`)".to_string()
    };

    let ocr_line = if cfg!(target_os = "macos") {
        "READY (Apple Vision native OCR)"
    } else {
        "Tesseract (if installed)"
    };

    let tts_line = if has_mlx_audio {
        "READY (`mlx-audio` neural TTS + Voice Cloning + macOS `say`)"
    } else if cfg!(target_os = "macos") {
        "READY (macOS native `say` TTS; `mlx-audio` neural TTS + Voice Cloning available 1-click in Audio & Docs)"
    } else {
        "HTTP TTS sidecar"
    };

    format!(
        "\
### Host & Capabilities Manifest (Live Ground Truth — Do NOT run shell commands just to guess host specs)
- Chip / CPU: {chip} ({cores} logical cores, {arch})
- OS: {os_ver}
- Memory: {total:.1} GB Total ({used:.1} GB Used, {avail:.1} GB Available)
- Active Workspace Root: `{ws}`
- Dex Writable Storage Root: `{storage}`

### Installed Capabilities Status
- Core Code, Files, Search, Shell, PTY, Git, TaskBoard: READY
- Pure-Rust 3D GLB Inspector (`mesh_inspect`) & Sample GLB: READY
- Pure-Rust Image Processing & Lanczos Upscale (`image_process`, `image_upscale`): READY
- Blender Headless 3D Modeling / Conversion / Preview: {blender_line}
- Document Parser & OCR (`doc_parse`, `ocr_image`): {ocr_line}
- Speech Synthesis (`audio_speak`): {tts_line}
- Voice Transcription (`audio_transcribe`): {stt_line}
- AI Image Generation (`image_generate`): {image_gen_line}
- AI Image-to-3D Reconstruction (`mesh_generate_3d`): {gen3d_line}
- AI Music & SFX Generation (`audio_generate_music`, `audio_generate_sfx`): {music_line}

### Capability Routing Rules (MANDATORY)
1. Never invent hardware specs — rely directly on the Host Manifest above.
2. If the user asks for an image, video, AI music/SFX, or AI image-to-3D reconstruction whose backend is listed as `NOT INSTALLED` (or if a tool returns `BackendNotInstalled`), DO NOT retry that tool in a loop.
3. Instead, transparently tell the user which backend is needed (and how to install it with 1 click in Dex's Models Hub / Extensions) AND proactively offer concrete alternatives that ARE `READY` right now (for example: building procedural 3D meshes in Blender via Python + exporting `.glb` and inspecting with `mesh_inspect`, or creating SVG/HTML visual prototypes).
4. Always reply in the same language the user writes in (Spanish in, Spanish out), including tables and headings.",
        chip = profile.cpu_brand,
        cores = profile.cpu_cores,
        arch = profile.arch,
        os_ver = profile.os_version,
        total = profile.total_memory_gb,
        used = profile.used_memory_gb,
        avail = profile.available_memory_gb,
        ws = workspace_dir.display(),
        storage = harness_core::dex_storage_root().display(),
    )
}

/// One prior chat turn sent by the UI so each new run continues the same conversation.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatHistoryTurn {
    pub role: String,
    pub content: String,
}

/// Maximum number of prior turns replayed into a new agent run (older turns are dropped first;
/// the context compactor handles the rest).
const MAX_HISTORY_TURNS: usize = 40;

fn replay_history(agent: &mut ReActAgent, history: &[ChatHistoryTurn]) {
    let start = history.len().saturating_sub(MAX_HISTORY_TURNS);
    for turn in &history[start..] {
        let content = turn.content.trim();
        if content.is_empty() {
            continue;
        }
        match turn.role.as_str() {
            "user" => agent.context_mut().push(harness_core::Message::user(content)),
            "assistant" => agent
                .context_mut()
                .push(harness_core::Message::assistant(Some(content.to_string()), None)),
            _ => {}
        }
    }
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn run_goal(
    app: AppHandle,
    state: State<'_, DesktopState>,
    goal: String,
    mode: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    api_base: Option<String>,
    attachments: Option<Vec<String>>,
    history: Option<Vec<ChatHistoryTurn>>,
) -> Result<String, String> {
    let attachment_list = attachments.unwrap_or_default();
    let history = history.unwrap_or_default();
    if goal.trim().is_empty() && attachment_list.is_empty() {
        return Err("Goal cannot be empty".to_string());
    }

    if let Some(m_str) = mode {
        let m = match m_str.to_lowercase().as_str() {
            "safe" => ExecutionMode::Safe,
            "plan" => ExecutionMode::Plan,
            _ => ExecutionMode::Autopilot,
        };
        let mut mode_lock = state.mode.write().await;
        *mode_lock = m;
    }

    let mut provider_name = provider.unwrap_or_else(|| "dex-local".to_string());
    let mut model_name = model.unwrap_or_default();
    let mut effective_api_base = api_base.filter(|s| !s.trim().is_empty());
    let mut active_local_runtime: Option<String> = None;
    let mut active_sidecar_id: Option<String> = None;

    // If using Dex Native Runtime (`dex-local`), resolve live mounted model endpoint & refresh idle timer
    if provider_name.eq_ignore_ascii_case("dex-local") || provider_name.eq_ignore_ascii_case("dex_local") {
        let active_opt = state.active_loaded_model.read().await.clone();
        if let Some(active) = active_opt {
            active_local_runtime = Some(active.runtime.clone());
            active_sidecar_id = Some(active.sidecar_id.clone());
            state
                .extension_supervisor
                .touch_last_used(&active.sidecar_id)
                .await;
            if effective_api_base.is_none() {
                effective_api_base = Some(active.base_url.clone());
            }
            if model_name.trim().is_empty() || model_name == active.name || model_name == active.model_id {
                model_name = if active.runtime == "mlx" {
                    active.path.clone()
                } else {
                    Path::new(&active.path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&active.name)
                        .to_string()
                };
            }
        } else if effective_api_base.is_none() {
            // Nothing mounted yet: load the best local model for this Mac into Dex's own
            // runtime (no LM Studio / Ollama needed). Already-running servers are a fallback.
            let models = harness_models::discover_native_chat_models().await;
            let preferred = harness_models::find_local_model(&models, &model_name).cloned();
            let ram = harness_models::total_ram_bytes().await;
            let chosen =
                preferred.or_else(|| harness_models::pick_default_chat_model(&models, ram).cloned());
            if let Some(chosen) = chosen {
                match harness_models::load_native_chat_model(
                    &state.extension_supervisor,
                    &chosen,
                    Some(&state.event_bus),
                )
                .await
                {
                    Ok(active) => {
                        active_local_runtime = Some(active.runtime.clone());
                        active_sidecar_id = Some(active.sidecar_id.clone());
                        effective_api_base = Some(active.base_url.clone());
                        model_name = if active.runtime == "mlx" {
                            active.path.clone()
                        } else {
                            Path::new(&active.path)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(&active.name)
                                .to_string()
                        };
                        *state.active_loaded_model.write().await = Some(active);
                    }
                    Err(e) => tracing::warn!(error = %e, model = %chosen.name, "Auto-loading the local chat model failed"),
                }
            }
        }
        if effective_api_base.is_none() {
            let servers = harness_models::detect_local_servers().await;
            let preferred = (!model_name.trim().is_empty()).then(|| model_name.clone());
            match harness_models::pick_chat_endpoint(&servers, preferred.as_deref()) {
                Some((srv, picked)) => {
                    provider_name = if srv.provider == "ollama" { "ollama" } else { "lmstudio" }.to_string();
                    model_name = picked;
                    effective_api_base = Some(srv.base_url.clone());
                }
                None => {
                    return Err(
                        "RuntimeMissing (llm): No local chat model found on this Mac. Click 'Load Recommended Local Model' to download one, or pick one in the Model Picker."
                            .to_string(),
                    );
                }
            }
        }
    } else if model_name.trim().is_empty() && effective_api_base.is_none() {
        // Local-first: pick whatever chat model a running local server (LM Studio on
        // :1234/:1235, Ollama, MLX, llama.cpp) is serving instead of silently using a cloud API.
        let servers = harness_models::detect_local_servers().await;
        let wanted = provider_name.to_lowercase();
        let candidates: Vec<_> = servers
            .into_iter()
            .filter(|s| match wanted.as_str() {
                "ollama" => s.id == "ollama",
                "lmstudio" | "lm_studio" => s.id == "lmstudio",
                _ => true,
            })
            .collect();
        match harness_models::pick_chat_endpoint(&candidates, None) {
            Some((srv, picked)) => {
                provider_name = if srv.provider == "ollama" { "ollama" } else { "lmstudio" }.to_string();
                model_name = picked;
                effective_api_base = Some(srv.base_url.clone());
            }
            None => {
                return Err(
                    "RuntimeMissing (llm): No local model server is running. Load a model in the Model Picker, open LM Studio (`lms server start`), or run `ollama serve`."
                        .to_string(),
                );
            }
        }
    }

    // A model was picked for LM Studio / Ollama without an explicit base URL: use the port the
    // server is really listening on (LM Studio moves to :1235/:1236 when :1234 is taken).
    if effective_api_base.is_none() {
        let wanted_id = match provider_name.to_lowercase().as_str() {
            "lmstudio" | "lm_studio" | "bionic" => Some("lmstudio"),
            "ollama" => Some("ollama"),
            _ => None,
        };
        if let Some(id) = wanted_id {
            effective_api_base = harness_models::detect_local_servers()
                .await
                .into_iter()
                .find(|s| s.id == id && s.reachable)
                .map(|s| s.base_url);
        }
    }

    let mut provider_instance =
        build_provider(&provider_name, &model_name, effective_api_base.as_deref())?;
    // Dex's own runtime: reload the model by itself if it was unloaded (idle, or to make room
    // for an image / music / 3D model).
    if let Some(ref sidecar) = active_sidecar_id {
        provider_instance = Arc::new(harness_models::NativeReloadProvider::new(
            provider_instance,
            state.extension_supervisor.clone(),
            sidecar.clone(),
        ));
    }

    let workspace_dir = state.active_workspace_dir.read().await.clone();

    // Load progressive skills
    let mut skill_loader = ProgressiveSkillLoader::new();
    skill_loader.scan_standard_locations().await;
    let skills_summary = skill_loader.generate_prompt_index();

    let mut tools = build_tools(
        provider_instance.clone(),
        skill_loader,
        state.extension_supervisor.clone(),
        state.event_bus.clone(),
        &workspace_dir,
    );

    // Connect enabled external MCP extensions as Deferred tools (discoverable via `search_tools`)
    let _ = register_enabled_mcp_extensions(state.extension_supervisor.clone(), &mut tools).await;

    // Setup Hooks with Desktop Security Hook
    let mut hooks = HookRegistry::new();
    let security_hook = DesktopSecurityHook {
        app_handle: app.clone(),
        mode: state.mode.clone(),
        approval_sender: state.approval_sender.clone(),
    };
    hooks.register(Arc::new(security_hook));

    let mut agent = ReActAgent::new(provider_instance, tools)
        .with_event_bus(state.event_bus.clone())
        .with_hooks(hooks)
        .with_compactor(ContextCompactor::new(8000, 10));

    let host_manifest = build_host_capabilities_manifest(&workspace_dir).await;
    let mut sys_prompt = format!(
        "You are Dex, a high-performance autonomous AI agent harness in Rust with native macOS, terminal, browser, multimodal, and external MCP extension capabilities.\nAll relative file paths, searches, shell commands, git operations, and TaskBoard actions execute rooted inside the active workspace.\nUse `search_tools` to discover and activate specialized deferred tools (OCR, document parsing, image/video studio, 3D/Blender/Unity, audio, MCP).\nIf a tool error contains `[dex-install:<id>]`, tell the user in one short sentence which component is missing; the app shows an Install button, so never list manual install steps.\nPrefer Dex built-in tools (e.g. `image_generate`, `audio_*`, `mesh_*`, `docs_*`) over MCP extension tools, which only work while their app (Unity Editor, ComfyUI, Blender) is running.\n\n{}",
        host_manifest
    );
    if !skills_summary.is_empty() {
        sys_prompt.push_str(&format!(
            "\n\nAvailable Skills:\n{}\nUse `load_skill` when relevant.",
            skills_summary
        ));
    }
    agent = agent.with_system_prompt(sys_prompt);
    replay_history(&mut agent, &history);

    let vision_enabled = runtime_supports_vision_payload(
        &provider_name,
        &model_name,
        active_local_runtime.as_deref(),
    );
    let (att_block, image_data_urls, had_image_without_vision) =
        prepare_attachments_context(&attachment_list, vision_enabled).await;
    if had_image_without_vision {
        let _ = app.emit(
            "dex-vision-advisory",
            serde_json::json!({
                "message": "Active local model server is text-only (mlx_lm.server). Attached image(s) will be inspected via Apple Vision OCR (`ocr_image`) instead of raw `image_url` payloads."
            }),
        );
    }

    let effective_goal = if att_block.is_empty() {
        goal.clone()
    } else if goal.trim().is_empty() {
        format!("Please inspect and analyze the attached files.{}", att_block)
    } else {
        format!("{}{}", goal, att_block)
    };

    // Cancellation channel
    let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
    {
        let mut cancel_lock = state.cancel_sender.lock().await;
        *cancel_lock = Some(cancel_tx);
    }

    {
        let mut goal_lock = state.active_goal.write().await;
        *goal_lock = Some(goal.clone());
    }

    let event_bus = state.event_bus.clone();
    let app_handle_for_task = app.clone();
    let supervisor_for_task = state.extension_supervisor.clone();
    let active_goal_ref = state.active_goal.clone();
    let cancel_sender_ref = state.cancel_sender.clone();

    tauri::async_runtime::spawn(async move {
        tracing::info!(
            workspace = %workspace_dir.display(),
            attachments = attachment_list.len(),
            images = image_data_urls.len(),
            vision_enabled = vision_enabled,
            "Starting autonomous execution in Dex Desktop"
        );

        let run_future = async {
            if image_data_urls.is_empty() {
                agent.run(&effective_goal).await
            } else {
                match agent.run_with_images(&effective_goal, image_data_urls).await {
                    Ok(ans) => Ok(ans),
                    Err(err) if is_multimodal_rejection_error(&err.to_string()) => {
                        tracing::warn!(
                            error = %err,
                            "Model endpoint rejected image_url payload; automatically falling back to text + ocr_image"
                        );
                        let _ = app_handle_for_task.emit(
                            "dex-vision-advisory",
                            serde_json::json!({
                                "message": "Model rejected direct vision payload; automatically falling back to text mode with Apple Vision OCR (`ocr_image`)."
                            }),
                        );
                        let fallback_goal = format!(
                            "{}\n\n[Automatic Vision Fallback] The LLM endpoint rejected raw `image_url` input. Call `ocr_image` (via `search_tools`) on the attached image path(s) to read their text, or tell the user this model is text-only.",
                            effective_goal
                        );
                        agent.run(&fallback_goal).await
                    }
                    Err(err) => Err(err),
                }
            }
        };
        tokio::pin!(run_future);

        tokio::select! {
            result = &mut run_future => {
                match result {
                    Ok(answer) => {
                        let _ = event_bus.publish(DexEvent::TaskUpdated {
                            task_id: "agent_orchestrator".to_string(),
                            status: "completed".to_string(),
                        });
                        let _ = app_handle_for_task.emit("dex-goal-finished", serde_json::json!({
                            "status": "success",
                            "answer": answer,
                        }));
                    }
                    Err(e) => {
                        let mut err_msg = e.to_string();
                        if let Some(ref sidecar_id) = active_sidecar_id {
                            let recent = supervisor_for_task.recent_logs(sidecar_id, 12).await;
                            if !recent.is_empty() {
                                err_msg.push_str("\n\n[dex-local-runtime stderr tail]\n");
                                err_msg.push_str(&recent.join("\n"));
                            }
                        }
                        tracing::error!("Agent execution failed: {}", err_msg);
                        let _ = app_handle_for_task.emit("dex-goal-finished", serde_json::json!({
                            "status": "error",
                            "error": err_msg,
                        }));
                    }
                }
            }
            _ = &mut cancel_rx => {
                tracing::warn!("Agent execution cancelled by user");
                let _ = app_handle_for_task.emit("dex-goal-finished", serde_json::json!({
                    "status": "cancelled",
                    "error": "Goal execution was stopped by user",
                }));
            }
        }

        // Always clear active goal & cancellation handle so state returns cleanly to Idle
        {
            let mut goal_lock = active_goal_ref.write().await;
            *goal_lock = None;
        }
        {
            let mut cancel_lock = cancel_sender_ref.lock().await;
            *cancel_lock = None;
        }
    });

    Ok(format!("Agent started for goal: {}", goal))
}

#[tauri::command]
pub async fn stop_goal(state: State<'_, DesktopState>) -> Result<String, String> {
    let mut cancel_lock = state.cancel_sender.lock().await;
    if let Some(tx) = cancel_lock.take() {
        let _ = tx.send(());
        let mut goal_lock = state.active_goal.write().await;
        *goal_lock = None;
        Ok("Active goal cancelled".to_string())
    } else {
        Ok("No active goal running".to_string())
    }
}

#[tauri::command]
pub async fn set_mode(
    app: AppHandle,
    state: State<'_, DesktopState>,
    mode: String,
) -> Result<String, String> {
    let parsed_mode = match mode.to_lowercase().as_str() {
        "safe" => ExecutionMode::Safe,
        "plan" => ExecutionMode::Plan,
        _ => ExecutionMode::Autopilot,
    };

    {
        let mut mode_lock = state.mode.write().await;
        *mode_lock = parsed_mode;
    }

    let mode_str = format!("{:?}", parsed_mode);
    let _ = app.emit("dex-mode-changed", serde_json::json!({ "mode": mode_str }));
    Ok(mode_str)
}

#[tauri::command]
pub async fn get_mode(state: State<'_, DesktopState>) -> Result<String, String> {
    let mode_lock = state.mode.read().await;
    Ok(format!("{:?}", *mode_lock))
}

#[tauri::command]
pub async fn resolve_approval(
    state: State<'_, DesktopState>,
    approved: bool,
) -> Result<bool, String> {
    let mut lock = state.approval_sender.lock().await;
    if let Some(tx) = lock.take() {
        let _ = tx.send(approved);
        Ok(approved)
    } else {
        Err("No pending tool approval request found".to_string())
    }
}

#[tauri::command]
pub async fn get_cookbook() -> Result<CookbookResponse, String> {
    let profile = tokio::task::spawn_blocking(HardwareProfile::detect)
        .await
        .map_err(|e| format!("Hardware detection task failed: {e}"))?;
    let recommendations = profile.recommend_models();
    let report = profile.generate_report();

    Ok(CookbookResponse {
        profile,
        recommendations,
        report,
    })
}

#[tauri::command]
pub async fn get_live_memory() -> Result<MemorySnapshot, String> {
    tokio::task::spawn_blocking(HardwareProfile::detect_live_memory)
        .await
        .map_err(|e| format!("Live memory probe failed: {e}"))
}

#[tauri::command]
pub async fn get_tasks(state: State<'_, DesktopState>) -> Result<Vec<TaskItem>, String> {
    let ws = state.active_workspace_dir.read().await.clone();
    let tasks_path = ws.join(".dex").join("tasks.json");
    let board = TaskBoard::load_or_default(&tasks_path).await;
    Ok(board.tasks)
}

#[tauri::command]
pub async fn get_screenshots(
    state: State<'_, DesktopState>,
) -> Result<Vec<ScreenshotEntry>, String> {
    let ws = state.active_workspace_dir.read().await.clone();
    let dir = ws.join(".dex").join("screenshots");
    if !tokio::fs::try_exists(&dir).await.unwrap_or(false) {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    let mut read_dir = tokio::fs::read_dir(&dir)
        .await
        .map_err(|e| format!("Failed to read screenshots directory: {}", e))?;

    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("png") {
            let filename = entry.file_name().to_string_lossy().to_string();
            let metadata = entry.metadata().await.ok();
            let size_bytes = metadata.map(|m| m.len()).unwrap_or(0);

            if let Ok(bytes) = tokio::fs::read(&path).await {
                let base64_str = to_base64(&bytes);
                let data_url = format!("data:image/png;base64,{}", base64_str);
                entries.push(ScreenshotEntry {
                    filename,
                    path: path.to_string_lossy().to_string(),
                    data_url,
                    size_bytes,
                });
            }
        }
    }

    // Sort newest first
    entries.sort_by(|a, b| b.filename.cmp(&a.filename));
    Ok(entries)
}

#[tauri::command]
pub async fn search_hf_models(
    query: String,
    format: Option<String>,
    sort: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<HfModelSummary>, String> {
    let client = HfClient::new(None);
    client
        .search_models_filtered(
            &query,
            format.as_deref(),
            sort.as_deref(),
            limit.unwrap_or(20),
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_hf_model_readme(repo_id: String) -> Result<String, String> {
    let client = HfClient::new(None);
    client
        .fetch_readme(&repo_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_hf_files(repo_id: String) -> Result<Vec<HfGgufFile>, String> {
    let client = HfClient::new(None);
    let profile = HardwareProfile::detect();
    client.inspect_gguf_files(&repo_id, &profile).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn download_model(
    state: State<'_, DesktopState>,
    repo_id: String,
    filename: String,
    download_url: String,
) -> Result<String, String> {
    let cancel = CancellationToken::new();
    {
        let mut lock = state.download_cancel.lock().await;
        *lock = Some(cancel.clone());
    }

    let models_dir = ModelRegistry::default_dir();
    let dest_path = models_dir.join(&filename);
    let event_bus = state.event_bus.clone();
    let filename_for_task = filename.clone();

    tauri::async_runtime::spawn(async move {
        let downloader = ModelDownloader::new(None);
        match downloader.download(&download_url, &dest_path, &repo_id, &filename_for_task, Some(&event_bus), cancel).await {
            Ok(saved_path) => {
                let quantization = parse_quantization(&filename_for_task);
                let meta = tokio::fs::metadata(&saved_path).await.ok();
                let size_bytes = meta.map(|m| m.len()).unwrap_or(0);
                let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                let model_id = format!("hf:{}", filename_for_task.trim_end_matches(".gguf"));

                let metadata = ModelMetadata {
                    id: model_id,
                    name: filename_for_task.clone(),
                    source: ModelSource::HuggingFace,
                    file_path: Some(saved_path.to_string_lossy().to_string()),
                    repo_id: Some(repo_id),
                    filename: Some(filename_for_task),
                    quantization: Some(quantization),
                    format: Some("gguf".to_string()),
                    size_bytes,
                    created_at: now,
                };

                let mut registry = ModelRegistry::load_or_default().await;
                let _ = registry.add(metadata).await;
            }
            Err(e) => {
                tracing::error!("Download error: {}", e);
            }
        }
    });

    Ok(format!("Download started for {}", filename))
}

#[tauri::command]
pub async fn cancel_download(state: State<'_, DesktopState>) -> Result<String, String> {
    let mut lock = state.download_cancel.lock().await;
    if let Some(token) = lock.take() {
        token.cancel();
        Ok("Download cancellation requested".to_string())
    } else {
        Ok("No active download running".to_string())
    }
}

#[tauri::command]
pub async fn import_local_gguf(
    path: String,
    name: Option<String>,
    to_ollama: bool,
) -> Result<ModelMetadata, String> {
    let p = PathBuf::from(&path);
    let bridge = if to_ollama { Some(OllamaBridge::default()) } else { None };
    import_local_model(p, name.as_deref(), true, to_ollama, bridge.as_ref())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_installed_models() -> Result<Vec<ModelMetadata>, String> {
    let registry = ModelRegistry::load_or_default().await;
    let mut models = registry.list().to_vec();

    // Query Ollama models and merge
    let bridge = OllamaBridge::default();
    if let Ok(tags) = bridge.list_models().await {
        for tag in tags {
            let id = format!("ollama:{}", tag.name);
            if !models.iter().any(|m| m.id == id || m.name == tag.name) {
                let quant = tag.details.as_ref().and_then(|d| d.quantization_level.clone());
                models.push(ModelMetadata {
                    id,
                    name: tag.name.clone(),
                    source: ModelSource::Ollama,
                    file_path: None,
                    repo_id: None,
                    filename: None,
                    quantization: quant,
                    format: Some("ollama".to_string()),
                    size_bytes: tag.size,
                    created_at: 0,
                });
            }
        }
    }

    Ok(models)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDirEntry {
    pub path: String,
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivatedModelResponse {
    pub provider: String,
    pub model: String,
    pub api_base: Option<String>,
    pub status_message: String,
    pub sidecar_id: Option<String>,
}

async fn build_model_dir_entries(cfg: &ModelDirsConfig) -> Vec<ModelDirEntry> {
    let mut entries = Vec::with_capacity(cfg.model_dirs.len());
    for dir in &cfg.model_dirs {
        let expanded = ModelDirsConfig::expand_tilde(dir);
        let exists = tokio::fs::try_exists(&expanded).await.unwrap_or(false);
        entries.push(ModelDirEntry {
            path: dir.clone(),
            exists,
        });
    }
    entries
}

#[tauri::command]
pub async fn list_model_dirs() -> Result<Vec<ModelDirEntry>, String> {
    let cfg = ModelDirsConfig::load_or_default().await;
    Ok(build_model_dir_entries(&cfg).await)
}

#[tauri::command]
pub async fn add_model_dir(path: String) -> Result<Vec<ModelDirEntry>, String> {
    let mut cfg = ModelDirsConfig::load_or_default().await;
    cfg.add_dir(&path).await.map_err(|e| e.to_string())?;
    Ok(build_model_dir_entries(&cfg).await)
}

#[tauri::command]
pub async fn remove_model_dir(path: String) -> Result<Vec<ModelDirEntry>, String> {
    let mut cfg = ModelDirsConfig::load_or_default().await;
    cfg.remove_dir(&path).await.map_err(|e| e.to_string())?;
    Ok(build_model_dir_entries(&cfg).await)
}

#[tauri::command]
pub async fn scan_local_models() -> Result<Vec<DiscoveredModel>, String> {
    let cfg = ModelDirsConfig::load_or_default().await;
    let mut discovered = scan_model_dirs(&cfg.resolved_paths())
        .await
        .map_err(|e| e.to_string())?;

    let mut known_paths: std::collections::HashSet<String> = discovered
        .iter()
        .filter_map(|m| m.path.clone())
        .collect();

    // Merge referenced entries from Dex ModelRegistry if outside scanned dirs
    let registry = ModelRegistry::load_or_default().await;
    for m in registry.list() {
        if m.source == ModelSource::Ollama {
            continue;
        }
        if let Some(ref p) = m.file_path {
            if !known_paths.insert(p.clone()) {
                continue;
            }
            if !tokio::fs::try_exists(Path::new(p)).await.unwrap_or(false) {
                continue;
            }
            let fmt = match m.format.as_deref() {
                Some("mlx") => ModelFormat::Mlx,
                Some("pt") => ModelFormat::PyTorch,
                _ => ModelFormat::Gguf,
            };
            let fmt_str = match fmt {
                ModelFormat::Mlx => "mlx",
                ModelFormat::PyTorch => "pt",
                _ => "gguf",
            };
            let p_check = PathBuf::from(p);
            let (integrity_status, integrity_note) = tokio::task::spawn_blocking(move || {
                verify_model_path_integrity(fmt_str, &p_check)
            })
            .await
            .unwrap_or((IntegrityStatus::Valid, None));

            discovered.push(DiscoveredModel {
                id: m.id.clone(),
                name: m.name.clone(),
                group_key: compute_group_key(&m.name),
                format: fmt,
                quantization: m.quantization.clone(),
                size_bytes: m.size_bytes,
                path: Some(p.clone()),
                source_dir: None,
                repo_hint: m.repo_id.clone(),
                architecture: None,
                context_length: None,
                capabilities: infer_capabilities(&m.name, None, false),
                modified_at: m.created_at,
                is_reference: true,
                integrity_status,
                integrity_note,
            });
        }
    }

    // Merge live Ollama daemon models
    let bridge = OllamaBridge::default();
    if let Ok(tags) = bridge.list_models().await {
        for tag in tags {
            let quant = tag
                .details
                .as_ref()
                .and_then(|d| d.quantization_level.clone())
                .filter(|q| !q.trim().is_empty());
            let arch = tag.details.as_ref().and_then(|d| d.family.clone());
            let caps = infer_capabilities(&tag.name, arch.as_deref(), false);
            discovered.push(DiscoveredModel {
                id: format!("ollama:{}", tag.name),
                name: tag.name.clone(),
                group_key: compute_group_key(&tag.name),
                format: ModelFormat::Ollama,
                quantization: quant,
                size_bytes: tag.size,
                path: None,
                source_dir: Some("ollama://127.0.0.1:11434".to_string()),
                repo_hint: Some(format!("ollama/{}", tag.name)),
                architecture: arch,
                context_length: None,
                capabilities: caps,
                modified_at: 0,
                is_reference: false,
                integrity_status: IntegrityStatus::Valid,
                integrity_note: None,
            });
        }
    }

    Ok(discovered)
}

#[tauri::command]
pub async fn detect_local_servers() -> Result<Vec<LocalServerInfo>, String> {
    Ok(detect_servers_impl().await)
}

#[tauri::command]
pub async fn detect_runtimes() -> Result<Vec<RuntimeStatus>, String> {
    tokio::task::spawn_blocking(detect_runtimes_impl)
        .await
        .map_err(|e| format!("Runtime detection task failed: {e}"))
}

#[tauri::command]
pub async fn install_runtime(
    state: State<'_, DesktopState>,
    runtime: String,
) -> Result<RuntimeStatus, String> {
    let kind = RuntimeKind::from_str_loose(&runtime).ok_or_else(|| {
        format!(
            "Unknown runtime '{runtime}'. Supported: {}.",
            harness_models::ALL_RUNTIMES
                .iter()
                .map(|k| k.as_id())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    install_runtime_impl(kind, Some(&state.event_bus))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_active_loaded_model(
    state: State<'_, DesktopState>,
) -> Result<Option<ActiveLoadedModel>, String> {
    let current = state.active_loaded_model.read().await.clone();
    if let Some(ref model) = current {
        let status = state
            .extension_supervisor
            .status_of(&model.sidecar_id)
            .await;
        // A model unloaded to free memory (or after idling) stays mounted: Dex reloads it
        // on the next message. Forget it only if its runtime entry is gone or broken.
        let registered = state
            .extension_supervisor
            .load_registry()
            .await
            .get(&model.sidecar_id)
            .is_some();
        if status == ExtensionStatus::Error || !registered {
            let mut lock = state.active_loaded_model.write().await;
            *lock = None;
            return Ok(None);
        }
    }
    Ok(current)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn load_local_model(
    state: State<'_, DesktopState>,
    model_id: String,
    model_name: String,
    format: String,
    model_path: String,
    quantization: Option<String>,
    size_bytes: Option<u64>,
) -> Result<LoadModelOutcome, String> {
    let fmt_lower = format.trim().to_lowercase();
    let kind = match fmt_lower.as_str() {
        "mlx" => RuntimeKind::Mlx,
        "gguf" => RuntimeKind::LlamaCpp,
        other => {
            return Err(format!(
                "Format '{other}' cannot be launched directly by Dex Native Runtime. Supported: mlx, gguf."
            ));
        }
    };

    // Fast path: if exact same local model is already loaded and running, reuse immediately
    {
        let current = state.active_loaded_model.read().await.clone();
        if let Some(active) = current {
            if active.path == model_path
                && state
                    .extension_supervisor
                    .status_of(&active.sidecar_id)
                    .await
                    == ExtensionStatus::Running
            {
                state
                    .extension_supervisor
                    .touch_last_used(&active.sidecar_id)
                    .await;
                return Ok(LoadModelOutcome::Loaded {
                    message: format!(
                        "Model '{}' is already loaded in Dex Native Runtime ({})",
                        active.name, active.base_url
                    ),
                    model: active,
                });
            }
        }
    }

    // 1. Detect autonomous runtime binary (without touching system Python or requiring external apps)
    let bin_opt = tokio::task::spawn_blocking(move || detect_runtime_binary(kind))
        .await
        .map_err(|e| format!("Runtime lookup failed: {e}"))?;

    let bin_path = match bin_opt {
        Some(p) => p,
        None => {
            return Ok(LoadModelOutcome::RuntimeMissing {
                runtime: kind.as_id().to_string(),
                install_hint: kind.install_hint().to_string(),
                message: format!(
                    "{} is not installed yet. Click 'Install {} (1-click)' to set it up automatically.",
                    kind.display_name(),
                    if kind == RuntimeKind::Mlx {
                        "MLX Runtime"
                    } else {
                        "llama.cpp"
                    }
                ),
            });
        }
    };

    // 2. Verify model path exists on disk and passes weight integrity verification
    if !tokio::fs::try_exists(Path::new(&model_path))
        .await
        .unwrap_or(false)
    {
        return Err(format!("Local model path does not exist: '{model_path}'"));
    }

    let fmt_for_check = fmt_lower.clone();
    let path_for_check = PathBuf::from(&model_path);
    let (integrity_status, integrity_note) = tokio::task::spawn_blocking(move || {
        verify_model_path_integrity(&fmt_for_check, &path_for_check)
    })
    .await
    .map_err(|e| format!("Model integrity check failed: {e}"))?;

    if integrity_status == IntegrityStatus::Incomplete {
        let reason = integrity_note
            .unwrap_or_else(|| "Model weights are missing or corrupted".to_string());
        return Err(format!(
            "ModelIntegrityError: Cannot load '{}' because its local weights are incomplete ({reason}). Re-download the model before loading.",
            model_name
        ));
    }

    let spec = build_launch_spec(&fmt_lower, &model_path, &bin_path).map_err(|e| e.to_string())?;
    let sidecar_id = DEX_RUNTIME_SIDECAR_ID.to_string();

    // 3. Unload any previous local model first to free Unified RAM cleanly
    let _ = state.extension_supervisor.stop_extension(&sidecar_id).await;
    let _ = state.extension_supervisor.stop_extension("mlx-lm-active").await;
    {
        let mut lock = state.active_loaded_model.write().await;
        *lock = None;
    }

    let bytes = size_bytes.unwrap_or(0);
    let est_ram_mb = if bytes > 0 {
        ((bytes as f64 / (1024.0 * 1024.0)) * 1.15).round() as u64
    } else {
        8192
    };

    let manifest = ExtensionManifest {
        id: sidecar_id.clone(),
        name: format!("Dex Native Runtime ({model_name})"),
        category: ExtensionCategory::Custom,
        description: format!(
            "Autonomous local {} inference server for {}",
            fmt_lower.to_uppercase(),
            model_name
        ),
        kind: ExtensionKind::LocalSidecar {
            command: spec.command.clone(),
            args: spec.args.clone(),
            port: spec.port,
            healthcheck_url: spec.healthcheck_url.clone(),
            env: HashMap::new(),
        },
        estimated_ram_mb: est_ram_mb,
        idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
        install_hint: kind.install_hint().to_string(),
        enabled: true,
        tags: vec![
            "dex-local".to_string(),
            fmt_lower.clone(),
            "llm".to_string(),
        ],
    };

    state
        .extension_supervisor
        .add_custom_extension(manifest)
        .await
        .map_err(|e| e.to_string())?;

    let _ = state.event_bus.publish(DexEvent::RuntimeProgress {
        runtime: sidecar_id.clone(),
        stage: "loading".to_string(),
        message: format!(
            "Loading {} ({}) into Unified Memory (timeout {}s)...",
            model_name,
            fmt_lower.to_uppercase(),
            LOCAL_MODEL_STARTUP_TIMEOUT_SECS
        ),
        done: false,
        is_error: false,
    });

    let rt_info = state
        .extension_supervisor
        .start_extension_wait_ready(
            &sidecar_id,
            Duration::from_secs(LOCAL_MODEL_STARTUP_TIMEOUT_SECS),
        )
        .await
        .map_err(|e| e.to_string())?;

    let active = ActiveLoadedModel {
        model_id,
        name: model_name.clone(),
        format: fmt_lower,
        quantization,
        path: model_path,
        runtime: kind.as_id().to_string(),
        base_url: spec.base_url.clone(),
        port: spec.port,
        sidecar_id,
        size_bytes: bytes,
        pid: rt_info.pid,
    };

    {
        let mut lock = state.active_loaded_model.write().await;
        *lock = Some(active.clone());
    }

    Ok(LoadModelOutcome::Loaded {
        message: format!(
            "Loaded '{}' via {} on {} (auto-unloads after {}s idle)",
            model_name,
            kind.display_name(),
            spec.base_url,
            DEFAULT_IDLE_TIMEOUT_SECS
        ),
        model: active,
    })
}

#[tauri::command]
pub async fn unload_local_model(state: State<'_, DesktopState>) -> Result<String, String> {
    let prev = {
        let mut lock = state.active_loaded_model.write().await;
        lock.take()
    };
    let _ = state
        .extension_supervisor
        .stop_extension(DEX_RUNTIME_SIDECAR_ID)
        .await;
    let _ = state
        .extension_supervisor
        .stop_extension("mlx-lm-active")
        .await;

    if let Some(model) = prev {
        Ok(format!(
            "Unloaded '{}' and reclaimed Unified Memory",
            model.name
        ))
    } else {
        Ok("No local model was loaded in Dex Native Runtime".to_string())
    }
}

#[tauri::command]
pub async fn activate_mlx_model(
    state: State<'_, DesktopState>,
    model_path: String,
    model_name: String,
) -> Result<ActivatedModelResponse, String> {
    // Prefer autonomous Dex Native MLX Runtime first; fall back to LM Studio (:1234) only if already serving it
    match load_local_model(
        state,
        format!("mlx:{model_name}"),
        model_name.clone(),
        "mlx".to_string(),
        model_path.clone(),
        Some(parse_quantization(&model_path)).filter(|q| q != "UNKNOWN"),
        None,
    )
    .await?
    {
        LoadModelOutcome::Loaded { model, message } => Ok(ActivatedModelResponse {
            provider: "dex-local".to_string(),
            model: model.path,
            api_base: Some(model.base_url),
            status_message: message,
            sidecar_id: Some(model.sidecar_id),
        }),
        LoadModelOutcome::RuntimeMissing {
            install_hint,
            message,
            ..
        } => {
            // Check if LM Studio (:1234) already happens to be serving this exact model
            let servers = detect_servers_impl().await;
            if let Some(lm) = servers.iter().find(|s| s.id == "lmstudio" && s.reachable) {
                let needle = model_name.to_lowercase();
                if let Some(matched) = lm
                    .loaded_models
                    .iter()
                    .find(|loaded| loaded.to_lowercase().contains(&needle))
                {
                    return Ok(ActivatedModelResponse {
                        provider: "lmstudio".to_string(),
                        model: matched.clone(),
                        api_base: Some(LM_STUDIO_DEFAULT_URL.to_string()),
                        status_message: format!(
                            "Connected to LM Studio ({LM_STUDIO_DEFAULT_URL}) serving '{matched}'"
                        ),
                        sidecar_id: None,
                    });
                }
            }
            Err(format!("{message} {install_hint}"))
        }
    }
}

#[tauri::command]
pub async fn open_external_url(url: String) -> Result<(), String> {
    let trimmed = url.trim();
    if !trimmed.starts_with("https://") && !trimmed.starts_with("http://") {
        return Err("Only http:// or https:// URLs can be opened externally".to_string());
    }
    if trimmed.contains('\0') || trimmed.contains('\n') {
        return Err("Invalid URL characters".to_string());
    }

    #[cfg(target_os = "macos")]
    {
        let status = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new("open").arg(trimmed).status(),
        )
        .await
        .map_err(|_| "Timed out opening URL".to_string())?
        .map_err(|e| format!("Failed to open URL: {e}"))?;
        if !status.success() {
            return Err(format!("open exited with status {status}"));
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let status = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new("xdg-open")
                .arg(trimmed)
                .status(),
        )
        .await
        .map_err(|_| "Timed out opening URL".to_string())?
        .map_err(|e| format!("Failed to open URL: {e}"))?;
        if !status.success() {
            return Err(format!("xdg-open exited with status {status}"));
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn delete_model(model_id: String) -> Result<String, String> {
    if model_id.starts_with("ollama:") {
        let name = model_id.trim_start_matches("ollama:");
        let bridge = OllamaBridge::default();
        let _ = bridge.delete_model(name).await;
    }

    let mut registry = ModelRegistry::load_or_default().await;
    let removed = registry.remove(&model_id, true).await.map_err(|e| e.to_string())?;
    if let Some(m) = removed {
        Ok(format!("Deleted model '{}'", m.name))
    } else {
        Ok(format!("Model '{}' removed", model_id))
    }
}

#[tauri::command]
pub async fn pull_ollama_model(
    state: State<'_, DesktopState>,
    model_name: String,
) -> Result<String, String> {
    let cancel = CancellationToken::new();
    {
        let mut lock = state.download_cancel.lock().await;
        *lock = Some(cancel.clone());
    }

    let event_bus = state.event_bus.clone();
    let name = model_name.clone();

    tauri::async_runtime::spawn(async move {
        let bridge = OllamaBridge::default();
        match bridge.pull_model(&name, Some(&event_bus), cancel).await {
            Ok(()) => {
                let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                let mut registry = ModelRegistry::load_or_default().await;
                let _ = registry.add(ModelMetadata {
                    id: format!("ollama:{}", name),
                    name: name.clone(),
                    source: ModelSource::Ollama,
                    file_path: None,
                    repo_id: None,
                    filename: None,
                    quantization: None,
                    format: Some("ollama".to_string()),
                    size_bytes: 0,
                    created_at: now,
                }).await;
            }
            Err(e) => {
                tracing::error!("Ollama pull error: {}", e);
            }
        }
    });

    Ok(format!("Pull started for Ollama model {}", model_name))
}

#[tauri::command]
pub async fn list_extension_catalog() -> Result<Vec<ExtensionManifest>, String> {
    Ok(builtin_catalog())
}

#[tauri::command]
pub async fn list_installed_extensions(
    state: State<'_, DesktopState>,
) -> Result<Vec<ExtensionRuntimeInfo>, String> {
    Ok(state.extension_supervisor.list_with_status().await)
}

#[tauri::command]
pub async fn install_catalog_extension(
    state: State<'_, DesktopState>,
    catalog_id: String,
) -> Result<ExtensionManifest, String> {
    state
        .extension_supervisor
        .install_from_catalog(&catalog_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn add_custom_mcp_extension(
    state: State<'_, DesktopState>,
    id: String,
    name: String,
    category: Option<String>,
    description: Option<String>,
    transport: String,
    command: Option<String>,
    args: Option<String>,
    url: Option<String>,
    estimated_ram_mb: Option<u64>,
    idle_timeout_secs: Option<u64>,
) -> Result<ExtensionManifest, String> {
    let kind = match transport.to_lowercase().as_str() {
        "sse" | "mcp_sse" => {
            let sse_url = url
                .filter(|u| !u.trim().is_empty())
                .ok_or_else(|| "SSE URL is required for SSE transport".to_string())?;
            ExtensionKind::McpSse { url: sse_url }
        }
        _ => {
            let cmd = command
                .filter(|c| !c.trim().is_empty())
                .ok_or_else(|| "Command is required for stdio transport".to_string())?;
            let parsed_args: Vec<String> = args
                .map(|a| a.split_whitespace().map(|s| s.to_string()).collect())
                .unwrap_or_default();
            ExtensionKind::McpStdio {
                command: cmd,
                args: parsed_args,
                env: HashMap::new(),
            }
        }
    };

    let cat = category
        .as_deref()
        .map(ExtensionCategory::from_str_loose)
        .unwrap_or(ExtensionCategory::Custom);

    let manifest = ExtensionManifest {
        id: id.trim().to_string(),
        name: name.trim().to_string(),
        category: cat,
        description: description
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| format!("Custom MCP server '{}'", name.trim())),
        kind,
        estimated_ram_mb: estimated_ram_mb.unwrap_or(512),
        idle_timeout_secs: idle_timeout_secs.unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS),
        install_hint: "Custom MCP extension registered via Dex.app".to_string(),
        enabled: true,
        tags: vec![id.trim().to_string(), cat.as_str().to_string(), "mcp".to_string()],
    };

    state
        .extension_supervisor
        .add_custom_extension(manifest)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn toggle_extension(
    state: State<'_, DesktopState>,
    id: String,
    enabled: bool,
) -> Result<ExtensionManifest, String> {
    state
        .extension_supervisor
        .set_enabled(&id, enabled)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn start_extension(
    state: State<'_, DesktopState>,
    id: String,
) -> Result<ExtensionRuntimeInfo, String> {
    state
        .extension_supervisor
        .start_extension(&id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn stop_extension(
    state: State<'_, DesktopState>,
    id: String,
) -> Result<ExtensionRuntimeInfo, String> {
    state
        .extension_supervisor
        .stop_extension(&id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn remove_extension(
    state: State<'_, DesktopState>,
    id: String,
) -> Result<String, String> {
    let removed = state
        .extension_supervisor
        .remove_extension(&id)
        .await
        .map_err(|e| e.to_string())?;
    match removed {
        Some(m) => Ok(format!("Removed extension '{}'", m.name)),
        None => Ok(format!("Extension '{}' not found", id)),
    }
}

#[tauri::command]
pub async fn update_extension_timeout(
    state: State<'_, DesktopState>,
    id: String,
    idle_timeout_secs: u64,
) -> Result<ExtensionManifest, String> {
    state
        .extension_supervisor
        .set_idle_timeout(&id, idle_timeout_secs)
        .await
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceTranscribeResponse {
    pub audio_path: String,
    pub duration_secs: f64,
    pub peak_db: f32,
    pub transcript: Option<String>,
    pub backend: Option<String>,
    pub language: Option<String>,
    pub stt_warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioStudioStateResponse {
    pub settings: AudioStudioSettings,
    pub voices: Vec<ClonedVoiceProfile>,
    pub catalog: Vec<AudioModelPreset>,
    pub runtimes: Vec<RuntimeStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioMediaEntry {
    pub kind: String,
    pub filename: String,
    pub path: String,
    pub data_url: String,
    pub size_bytes: u64,
}

#[tauri::command]
pub async fn get_audio_studio_state() -> Result<AudioStudioStateResponse, String> {
    let (settings, voices, runtimes) = tokio::join!(
        load_audio_settings(),
        list_voice_profiles(),
        async {
            tokio::task::spawn_blocking(detect_runtimes_impl)
                .await
                .unwrap_or_default()
        }
    );
    Ok(AudioStudioStateResponse {
        settings,
        voices,
        catalog: builtin_audio_catalog(),
        runtimes,
    })
}

#[tauri::command]
pub async fn save_audio_settings_cmd(
    settings: AudioStudioSettings,
) -> Result<AudioStudioSettings, String> {
    save_audio_settings(&settings)
        .await
        .map_err(|e| e.to_string())?;
    Ok(settings)
}

#[tauri::command]
pub async fn list_cloned_voices() -> Result<Vec<ClonedVoiceProfile>, String> {
    Ok(list_voice_profiles().await)
}

#[tauri::command]
pub async fn save_cloned_voice(
    name: String,
    language: String,
    ref_audio_path: String,
    ref_text: Option<String>,
) -> Result<ClonedVoiceProfile, String> {
    save_voice_profile(
        &name,
        &language,
        Path::new(&ref_audio_path),
        ref_text.as_deref(),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_cloned_voice(id: String) -> Result<bool, String> {
    delete_voice_profile(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn record_voice_sample(
    state: State<'_, DesktopState>,
    seconds: Option<u64>,
) -> Result<RecordingResult, String> {
    let dur = seconds.unwrap_or(5).clamp(2, 20);
    let rec_dir = harness_core::resolve_dex_subpath(DEFAULT_RECORDINGS_DIR);
    record_fixed_duration(dur, &rec_dir, Some(state.event_bus.clone()))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn test_mic_and_stt(
    state: State<'_, DesktopState>,
    seconds: Option<u64>,
    language: Option<String>,
    engine: Option<String>,
    model: Option<String>,
) -> Result<VoiceTranscribeResponse, String> {
    let dur = seconds.unwrap_or(3).clamp(2, 10);
    let rec_dir = harness_core::resolve_dex_subpath(DEFAULT_RECORDINGS_DIR);
    let rec = record_fixed_duration(dur, &rec_dir, Some(state.event_bus.clone()))
        .await
        .map_err(|e| e.to_string())?;
    let path_str = rec.path.to_string_lossy().to_string();
    let opts = SttOptions {
        language: language.filter(|s| !s.trim().is_empty()),
        engine: engine.filter(|s| !s.trim().is_empty()),
        model: model.filter(|s| !s.trim().is_empty()),
        include_timestamps: false,
        // Recording already ran VAD + soft peak normalization inside `run_capture_thread`
        preprocess: false,
    };

    match AutoSttEngine::transcribe_with_options(&rec.path, &opts).await {
        Ok(stt) => Ok(VoiceTranscribeResponse {
            audio_path: path_str,
            duration_secs: rec.duration_secs,
            peak_db: rec.peak_db,
            transcript: Some(stt.text),
            backend: Some(stt.backend),
            language: stt.language,
            stt_warning: None,
        }),
        Err(err) => Ok(VoiceTranscribeResponse {
            audio_path: path_str,
            duration_secs: rec.duration_secs,
            peak_db: rec.peak_db,
            transcript: None,
            backend: None,
            language: None,
            stt_warning: Some(err.to_string()),
        }),
    }
}

#[tauri::command]
pub async fn start_voice_recording(state: State<'_, DesktopState>) -> Result<String, String> {
    let mut lock = state.active_recorder.lock().await;
    if lock.is_some() {
        return Err("Voice recording is already in progress".to_string());
    }

    let rec_dir = harness_core::resolve_dex_subpath(DEFAULT_RECORDINGS_DIR);
    let handle = VoiceRecorderHandle::start(&rec_dir, Some(state.event_bus.clone()))
        .await
        .map_err(|e| e.to_string())?;
    *lock = Some(handle);
    Ok("Recording started".to_string())
}

#[tauri::command]
pub async fn stop_and_transcribe(
    state: State<'_, DesktopState>,
    language: Option<String>,
    engine: Option<String>,
    model: Option<String>,
) -> Result<VoiceTranscribeResponse, String> {
    let handle = {
        let mut lock = state.active_recorder.lock().await;
        lock.take()
            .ok_or_else(|| "No active voice recording to stop".to_string())?
    };

    let rec = handle.stop().await.map_err(|e| e.to_string())?;
    let path_str = rec.path.to_string_lossy().to_string();

    let opts = SttOptions {
        language: language.filter(|s| !s.trim().is_empty()),
        engine: engine.filter(|s| !s.trim().is_empty()),
        model: model.filter(|s| !s.trim().is_empty()),
        include_timestamps: false,
        // Recording already ran VAD + soft peak normalization on capture stop
        preprocess: false,
    };

    match AutoSttEngine::transcribe_with_options(&rec.path, &opts).await {
        Ok(stt) => {
            let preview: String = stt.text.chars().take(120).collect();
            state.event_bus.publish(DexEvent::MediaGenerated {
                kind: "transcript".to_string(),
                path: path_str.clone(),
                prompt: preview,
                duration_secs: Some(rec.duration_secs),
            });
            Ok(VoiceTranscribeResponse {
                audio_path: path_str,
                duration_secs: rec.duration_secs,
                peak_db: rec.peak_db,
                transcript: Some(stt.text),
                backend: Some(stt.backend),
                language: stt.language,
                stt_warning: None,
            })
        }
        Err(err) => Ok(VoiceTranscribeResponse {
            audio_path: path_str,
            duration_secs: rec.duration_secs,
            peak_db: rec.peak_db,
            transcript: None,
            backend: None,
            language: None,
            stt_warning: Some(err.to_string()),
        }),
    }
}

/// Generates instrumental music or a sound effect locally (MusicGen sidecar, sized to this Mac).
#[tauri::command]
pub async fn generate_audio_clip(
    state: State<'_, DesktopState>,
    kind: String,
    prompt: String,
    duration_secs: Option<f64>,
) -> Result<harness_audio::GeneratedAudio, String> {
    let sup = Some(state.extension_supervisor.clone());
    let result = if kind.eq_ignore_ascii_case("sfx") {
        harness_audio::generate_sfx(&prompt, duration_secs.unwrap_or(4.0), None, sup, Some(&state.event_bus))
            .await
    } else {
        harness_audio::generate_music(&prompt, None, duration_secs.unwrap_or(15.0), None, sup, Some(&state.event_bus))
            .await
    };
    result.map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn speak_text(
    state: State<'_, DesktopState>,
    text: String,
    voice: Option<String>,
    language: Option<String>,
    engine: Option<String>,
    model: Option<String>,
    ref_audio: Option<String>,
    ref_text: Option<String>,
    sidecar_url: Option<String>,
) -> Result<TtsResult, String> {
    let opts = TtsOptions {
        voice: voice.filter(|s| !s.trim().is_empty()),
        language: language.filter(|s| !s.trim().is_empty()),
        engine: engine.filter(|s| !s.trim().is_empty()),
        model: model.filter(|s| !s.trim().is_empty()),
        ref_audio: ref_audio
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from),
        ref_text: ref_text.filter(|s| !s.trim().is_empty()),
        sidecar_url: sidecar_url.filter(|s| !s.trim().is_empty()),
        output_path: None,
    };
    synthesize_speech_with_options(
        &text,
        opts,
        Some(state.extension_supervisor.clone()),
        Some(&state.event_bus),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_audio_media() -> Result<Vec<AudioMediaEntry>, String> {
    let dirs_to_scan = [
        ("tts", DEFAULT_TTS_DIR),
        ("music", DEFAULT_MUSIC_DIR),
        ("sfx", DEFAULT_SFX_DIR),
        ("recording", DEFAULT_RECORDINGS_DIR),
    ];

    let mut entries = Vec::new();
    const MAX_INLINE_AUDIO_BYTES: u64 = 25 * 1024 * 1024;

    for (kind, dir_str) in dirs_to_scan {
        let dir = harness_core::resolve_dex_subpath(dir_str);
        if !tokio::fs::try_exists(&dir).await.unwrap_or(false) {
            continue;
        }
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(r) => r,
            Err(_) => continue,
        };

        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            let mime = match ext.as_str() {
                "wav" => "audio/wav",
                "mp3" => "audio/mpeg",
                "m4a" => "audio/mp4",
                "ogg" => "audio/ogg",
                _ => continue,
            };

            let filename = entry.file_name().to_string_lossy().to_string();
            let size_bytes = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
            if size_bytes == 0 || size_bytes > MAX_INLINE_AUDIO_BYTES {
                continue;
            }

            if let Ok(bytes) = tokio::fs::read(&path).await {
                let data_url = format!("data:{mime};base64,{}", to_base64(&bytes));
                entries.push(AudioMediaEntry {
                    kind: kind.to_string(),
                    filename,
                    path: path.to_string_lossy().to_string(),
                    data_url,
                    size_bytes,
                });
            }
        }
    }

    entries.sort_by(|a, b| b.filename.cmp(&a.filename));
    Ok(entries)
}

#[tauri::command]
pub async fn run_ocr_image(
    state: State<'_, DesktopState>,
    path: String,
) -> Result<OcrResult, String> {
    run_ocr_on_image(Path::new(&path), Some(&state.event_bus))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn parse_document(
    state: State<'_, DesktopState>,
    path: String,
) -> Result<DocumentParseResult, String> {
    parse_doc_impl(
        Path::new(&path),
        DEFAULT_MAX_INLINE_CHARS,
        Some(state.extension_supervisor.clone()),
        Some(&state.event_bus),
    )
    .await
    .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualMediaEntry {
    pub kind: String,
    pub filename: String,
    pub path: String,
    pub format: String,
    pub size_bytes: u64,
    pub data_url: Option<String>,
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn generate_image(
    state: State<'_, DesktopState>,
    prompt: String,
    negative_prompt: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    steps: Option<u32>,
    backend: Option<String>,
    workflow: Option<String>,
    model: Option<String>,
) -> Result<ImageGenResult, String> {
    let parsed_backend = backend
        .as_deref()
        .map(ImageBackend::from_str_loose)
        .unwrap_or(ImageBackend::Auto);

    let req = ImageGenRequest {
        prompt,
        negative_prompt: negative_prompt.filter(|s| !s.trim().is_empty()),
        width: width.unwrap_or(1024),
        height: height.unwrap_or(1024),
        steps: steps.unwrap_or(20),
        seed: None,
        backend: parsed_backend,
        workflow: workflow.filter(|s| !s.trim().is_empty()),
        model: model.filter(|s| !s.trim().is_empty()),
        output_path: None,
    };

    generate_image_impl(
        req,
        Some(state.extension_supervisor.clone()),
        Some(&state.event_bus),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn upscale_image(
    state: State<'_, DesktopState>,
    path: String,
    scale: Option<u32>,
    backend: Option<String>,
) -> Result<ImageProcessResult, String> {
    let parsed_backend = backend
        .as_deref()
        .map(ImageBackend::from_str_loose)
        .unwrap_or(ImageBackend::Auto);

    upscale_image_impl(
        Path::new(&path),
        scale.unwrap_or(2),
        parsed_backend,
        Some(state.extension_supervisor.clone()),
        Some(&state.event_bus),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn process_image(
    path: String,
    resize_width: Option<u32>,
    resize_height: Option<u32>,
    format: Option<String>,
    output_path: Option<String>,
) -> Result<ImageProcessResult, String> {
    let opts = ImageProcessOptions {
        resize_width,
        resize_height,
        preserve_aspect: false,
        crop: None,
        target_format: format.filter(|s| !s.trim().is_empty()),
        output_path: output_path
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from),
    };

    process_image_impl(Path::new(&path), opts)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_visual_media() -> Result<Vec<VisualMediaEntry>, String> {
    const MAX_INLINE_VISUAL_BYTES: u64 = 25 * 1024 * 1024;
    let items = list_visual_media_items().await.map_err(|e| e.to_string())?;
    let mut entries = Vec::with_capacity(items.len());

    for item in items {
        let mime = match item.format.as_str() {
            "png" => Some("image/png"),
            "jpg" | "jpeg" => Some("image/jpeg"),
            "webp" => Some("image/webp"),
            "gif" => Some("image/gif"),
            "mp4" => Some("video/mp4"),
            "webm" => Some("video/webm"),
            "mov" => Some("video/quicktime"),
            _ => None,
        };

        let data_url = if item.size_bytes > 0 && item.size_bytes <= MAX_INLINE_VISUAL_BYTES {
            if let Some(m) = mime {
                tokio::fs::read(&item.path)
                    .await
                    .ok()
                    .map(|bytes| format!("data:{m};base64,{}", to_base64(&bytes)))
            } else {
                None
            }
        } else {
            None
        };

        entries.push(VisualMediaEntry {
            kind: item.kind,
            filename: item.filename,
            path: item.path,
            format: item.format,
            size_bytes: item.size_bytes,
            data_url,
        });
    }

    Ok(entries)
}

#[tauri::command]
pub async fn inspect_video_file(path: String) -> Result<VideoMetadata, String> {
    inspect_video_impl(Path::new(&path))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_comfyui_workflows() -> Result<Vec<String>, String> {
    let wf_dir = harness_core::resolve_dex_subpath(DEFAULT_WORKFLOWS_DIR);
    list_workflows(&wf_dir)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn reveal_in_finder(path: String) -> Result<String, String> {
    if path.trim().is_empty() || path.contains('\0') {
        return Err("Invalid path".to_string());
    }
    let target = PathBuf::from(&path);
    if !tokio::fs::try_exists(&target).await.unwrap_or(false) {
        return Err(format!("Path does not exist: '{path}'"));
    }

    let resolved = tokio::fs::canonicalize(&target).await.unwrap_or(target);

    #[cfg(target_os = "macos")]
    {
        let status = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new("open")
                .arg("-R")
                .arg(&resolved)
                .status(),
        )
        .await
        .map_err(|_| "Timed out revealing file in Finder".to_string())?
        .map_err(|e| format!("Failed to invoke Finder (`open -R`): {e}"))?;

        if !status.success() {
            return Err(format!("Finder command exited with status {status}"));
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let parent = resolved.parent().unwrap_or(resolved.as_path());
        let status = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new("xdg-open")
                .arg(parent)
                .status(),
        )
        .await
        .map_err(|_| "Timed out opening file manager".to_string())?
        .map_err(|e| format!("Failed to invoke file manager: {e}"))?;
        if !status.success() {
            return Err(format!("File manager exited with status {status}"));
        }
    }

    Ok(resolved.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn list_3d_models() -> Result<Vec<StoredModel3dItem>, String> {
    list_3d_models_impl().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn inspect_3d_model(path: String) -> Result<MeshReport, String> {
    inspect_mesh_impl(Path::new(&path))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn read_glb_base64(path: String) -> Result<String, String> {
    const MAX_GLB_VIEWER_BYTES: u64 = 64 * 1024 * 1024;
    if path.trim().is_empty() || path.contains('\0') {
        return Err("Invalid GLB file path".to_string());
    }
    let target = Path::new(&path);
    if !tokio::fs::try_exists(target).await.unwrap_or(false) {
        return Err(format!("GLB file does not exist: '{path}'"));
    }
    let ext = target
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext != "glb" {
        return Err(format!(
            "Offline WebGL2 viewer requires a binary .glb file (got '.{ext}'). Convert with Blender first."
        ));
    }
    let meta = tokio::fs::metadata(target)
        .await
        .map_err(|e| format!("Failed to read metadata for '{path}': {e}"))?;
    if meta.len() > MAX_GLB_VIEWER_BYTES {
        return Err(format!(
            "GLB file ({:.1} MB) exceeds the 64 MB inline WebGL viewer limit",
            (meta.len() as f64) / (1024.0 * 1024.0)
        ));
    }
    let bytes = tokio::fs::read(target)
        .await
        .map_err(|e| format!("Failed to read GLB file '{path}': {e}"))?;
    Ok(to_base64(&bytes))
}

#[tauri::command]
pub async fn create_sample_3d_model(state: State<'_, DesktopState>) -> Result<MeshReport, String> {
    let dest = harness_core::resolve_dex_subpath(DEFAULT_3D_DIR).join("sample_cube.glb");
    write_sample_cube_glb(&dest)
        .await
        .map_err(|e| e.to_string())?;
    let report = inspect_mesh_impl(&dest).await.map_err(|e| e.to_string())?;
    state.event_bus.publish(DexEvent::MediaGenerated {
        kind: "3d".to_string(),
        path: dest.to_string_lossy().to_string(),
        prompt: "Sample glTF 2.0 cube (12 triangles)".to_string(),
        duration_secs: None,
    });
    Ok(report)
}

#[tauri::command]
pub async fn generate_3d_model(
    state: State<'_, DesktopState>,
    image_path: Option<String>,
    prompt: Option<String>,
    backend: Option<String>,
    texture_resolution: Option<u32>,
    remove_background: Option<bool>,
) -> Result<Gen3dResult, String> {
    let parsed_backend = backend
        .as_deref()
        .map(Gen3dBackend::from_str_loose)
        .unwrap_or(Gen3dBackend::Auto);
    let req = Gen3dRequest {
        image_path: image_path.filter(|s| !s.trim().is_empty()),
        prompt: prompt.filter(|s| !s.trim().is_empty()),
        backend: parsed_backend,
        texture_resolution: texture_resolution.unwrap_or(1024),
        remove_background: remove_background.unwrap_or(true),
    };
    generate_3d_impl(
        req,
        Some(state.extension_supervisor.clone()),
        Some(&state.event_bus),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn convert_3d_model(
    state: State<'_, DesktopState>,
    input_path: String,
    output_path: String,
    decimate_ratio: Option<f32>,
    apply_transforms: Option<bool>,
) -> Result<BlenderConvertResult, String> {
    blender_convert_mesh(
        Path::new(&input_path),
        Path::new(&output_path),
        decimate_ratio,
        apply_transforms.unwrap_or(true),
        Some(&state.event_bus),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn render_3d_preview(
    state: State<'_, DesktopState>,
    input_path: String,
    output_png: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<BlenderPreviewResult, String> {
    blender_render_preview(
        Path::new(&input_path),
        output_png
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(Path::new),
        width.unwrap_or(768),
        height.unwrap_or(768),
        Some(&state.event_bus),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn inspect_unity_project(project_dir: String) -> Result<UnityProjectReport, String> {
    unity_inspect_project(Path::new(&project_dir))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn export_model_to_unity(
    mesh_path: String,
    project_dir: String,
    subdir: Option<String>,
    overwrite: Option<bool>,
) -> Result<UnityExportResult, String> {
    unity_export_asset(
        Path::new(&mesh_path),
        Path::new(&project_dir),
        subdir.as_deref().filter(|s| !s.trim().is_empty()),
        overwrite.unwrap_or(false),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn check_blender_status() -> Result<BlenderStatus, String> {
    Ok(check_blender_impl().await)
}

// =========================================================================
// Phase 12: Projects / Workspaces, Sessions & File Attachments
// =========================================================================

async fn build_projects_response(state: &DesktopState, store: &ProjectsStore) -> ProjectsResponse {
    let active_ws = state.active_workspace_dir.read().await.clone();
    ProjectsResponse {
        active_project_id: store.active_project_id.clone(),
        active_workspace_dir: active_ws.to_string_lossy().to_string(),
        projects: store.projects.clone(),
    }
}

async fn sync_storage_root_for_store(state: &DesktopState, store: &ProjectsStore) {
    if let Some(active) = store.active_project() {
        let active_path = PathBuf::from(&active.path);
        if !harness_core::is_unwritable_system_root(&active_path)
            && tokio::fs::try_exists(&active_path).await.unwrap_or(false)
        {
            {
                let mut ws = state.active_workspace_dir.write().await;
                *ws = active_path.clone();
            }
            harness_core::set_default_storage_root(active_path.join(".dex"));
            return;
        }
    }
    let home_ws = dirs::home_dir().unwrap_or_else(harness_core::home_dex_dir);
    {
        let mut ws = state.active_workspace_dir.write().await;
        if harness_core::is_unwritable_system_root(&ws) {
            *ws = home_ws;
        }
    }
    harness_core::set_default_storage_root(harness_core::home_dex_dir());
}

async fn active_sessions_dir_for_state() -> PathBuf {
    let store = ProjectsStore::load_or_default().await;
    let active_proj_path = store
        .active_project()
        .map(|p| PathBuf::from(&p.path))
        .filter(|p| !harness_core::is_unwritable_system_root(p));
    resolve_sessions_dir(active_proj_path.as_deref())
}

#[tauri::command]
pub async fn list_projects(state: State<'_, DesktopState>) -> Result<ProjectsResponse, String> {
    let store = ProjectsStore::load_or_default().await;
    sync_storage_root_for_store(&state, &store).await;
    Ok(build_projects_response(&state, &store).await)
}

#[tauri::command]
pub async fn create_project(
    state: State<'_, DesktopState>,
    name: String,
    parent_dir: Option<String>,
    init_git: Option<bool>,
) -> Result<ProjectsResponse, String> {
    let (clean_name, project_path) =
        create_project_on_disk(&name, parent_dir.as_deref(), init_git.unwrap_or(true)).await?;
    let mut store = ProjectsStore::load_or_default().await;
    store.upsert_and_activate(clean_name, project_path.clone());
    store.save().await?;

    sync_storage_root_for_store(&state, &store).await;
    Ok(build_projects_response(&state, &store).await)
}

#[tauri::command]
pub async fn open_project_dialog(
    state: State<'_, DesktopState>,
) -> Result<Option<ProjectsResponse>, String> {
    let picked = match pick_folder_native().await? {
        Some(p) => p,
        None => return Ok(None),
    };
    ensure_project_dex_dirs(&picked).await?;
    let name = picked
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Workspace")
        .to_string();

    let mut store = ProjectsStore::load_or_default().await;
    store.upsert_and_activate(name, picked.clone());
    store.save().await?;

    sync_storage_root_for_store(&state, &store).await;
    Ok(Some(build_projects_response(&state, &store).await))
}

#[tauri::command]
pub async fn select_project(
    state: State<'_, DesktopState>,
    id: String,
) -> Result<ProjectsResponse, String> {
    let mut store = ProjectsStore::load_or_default().await;
    let entry = store
        .select_by_id(&id)
        .ok_or_else(|| format!("Project '{id}' not found"))?;
    let proj_path = PathBuf::from(&entry.path);
    ensure_project_dex_dirs(&proj_path).await?;
    store.save().await?;

    sync_storage_root_for_store(&state, &store).await;
    Ok(build_projects_response(&state, &store).await)
}

#[tauri::command]
pub async fn remove_project(
    state: State<'_, DesktopState>,
    id: String,
) -> Result<ProjectsResponse, String> {
    let mut store = ProjectsStore::load_or_default().await;
    store.remove_by_id(&id);
    store.save().await?;

    sync_storage_root_for_store(&state, &store).await;
    Ok(build_projects_response(&state, &store).await)
}

#[tauri::command]
pub async fn pick_attachment_files() -> Result<Vec<AttachmentMeta>, String> {
    pick_attachment_files_native().await
}

#[tauri::command]
pub async fn inspect_attachment_paths(paths: Vec<String>) -> Result<Vec<AttachmentMeta>, String> {
    let mut results = Vec::new();
    for raw in paths {
        if let Ok(meta) = inspect_attachment_file(&raw).await {
            results.push(meta);
        }
    }
    Ok(results)
}

#[tauri::command]
pub async fn list_project_sessions() -> Result<Vec<ChatSessionSummary>, String> {
    let dir = active_sessions_dir_for_state().await;
    list_sessions_in_dir(&dir).await
}

#[tauri::command]
pub async fn save_project_session(session: ChatSession) -> Result<ChatSession, String> {
    let dir = active_sessions_dir_for_state().await;
    save_session_in_dir(&dir, session).await
}

#[tauri::command]
pub async fn load_project_session(session_id: String) -> Result<ChatSession, String> {
    let dir = active_sessions_dir_for_state().await;
    load_session_in_dir(&dir, &session_id).await
}

#[tauri::command]
pub async fn rename_project_session(
    session_id: String,
    title: String,
) -> Result<ChatSession, String> {
    let dir = active_sessions_dir_for_state().await;
    rename_session_in_dir(&dir, &session_id, &title).await
}

#[tauri::command]
pub async fn delete_project_session(session_id: String) -> Result<(), String> {
    let dir = active_sessions_dir_for_state().await;
    delete_session_in_dir(&dir, &session_id).await
}

// =========================================================================
// Phase 14: Reusable Native Pickers & Automated QA E2E Walkthrough Hooks
// =========================================================================

#[tauri::command]
pub async fn pick_file_dialog(
    title: Option<String>,
    extensions: Option<Vec<String>>,
) -> Result<Option<String>, String> {
    let picked = pick_file_dialog_native(title.as_deref(), extensions.as_deref()).await?;
    Ok(picked.map(|p| p.to_string_lossy().to_string()))
}

#[tauri::command]
pub async fn pick_folder_dialog(title: Option<String>) -> Result<Option<String>, String> {
    let picked = pick_folder_dialog_native(title.as_deref()).await?;
    Ok(picked.map(|p| p.to_string_lossy().to_string()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QaWalkthroughConfig {
    pub enabled: bool,
    pub output_dir: String,
}

fn qa_output_dir() -> PathBuf {
    std::env::var("DEX_QA_OUT_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/dex-qa"))
}

#[tauri::command]
pub async fn get_qa_walkthrough_config() -> Result<QaWalkthroughConfig, String> {
    let enabled = std::env::var("DEX_QA_WALKTHROUGH")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    Ok(QaWalkthroughConfig {
        enabled,
        output_dir: qa_output_dir().to_string_lossy().to_string(),
    })
}

#[tauri::command]
pub async fn qa_capture_screenshot(
    app: AppHandle,
    filename: String,
) -> Result<String, String> {
    let enabled = std::env::var("DEX_QA_WALKTHROUGH")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    if !enabled {
        return Err("QA screenshot capture is only enabled when DEX_QA_WALKTHROUGH=1".to_string());
    }

    let safe_name: String = filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    if safe_name.is_empty() || safe_name.contains("..") {
        return Err("Invalid screenshot filename".to_string());
    }
    let final_name = if safe_name.ends_with(".png") {
        safe_name
    } else {
        format!("{safe_name}.png")
    };

    let out_dir = qa_output_dir();
    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| format!("Failed to create QA dir '{}': {e}", out_dir.display()))?;
    let dest = out_dir.join(final_name);

    #[cfg(target_os = "macos")]
    {
        use tauri::Manager;
        let region_arg = app.get_webview_window("main").and_then(|win| {
            let _ = win.show();
            let _ = win.unminimize();
            let _ = win.center();
            let _ = win.set_focus();
            let scale = win.scale_factor().ok().filter(|s| *s > 0.0).unwrap_or(1.0);
            let pos = win.outer_position().ok()?;
            let size = win.outer_size().ok()?;
            let x = (f64::from(pos.x) / scale).round() as i64;
            let y = (f64::from(pos.y) / scale).round() as i64;
            let w = (f64::from(size.width) / scale).round() as i64;
            let h = (f64::from(size.height) / scale).round() as i64;
            if w > 100 && h > 100 {
                Some(format!("{x},{y},{w},{h}"))
            } else {
                None
            }
        });

        // Brief settle after focusing window
        tokio::time::sleep(Duration::from_millis(120)).await;

        let mut cmd = tokio::process::Command::new("/usr/sbin/screencapture");
        cmd.arg("-x");
        if let Some(ref rect) = region_arg {
            cmd.args(["-R", rect]);
        }
        cmd.arg(&dest);

        let direct_ok = match tokio::time::timeout(Duration::from_secs(10), cmd.output()).await {
            Ok(Ok(out)) => out.status.success(),
            _ => false,
        };

        if !direct_ok {
            // Unsigned local .app bundles may lack macOS Screen Recording TCC; coordinate with external QA runner if active
            let req_path = out_dir.join(".capture_req.json");
            let req_payload = serde_json::json!({
                "dest": dest.to_string_lossy().to_string(),
                "rect": region_arg,
            });
            if let Ok(req_json) = serde_json::to_string(&req_payload) {
                let _ = tokio::fs::write(&req_path, req_json).await;
                let deadline = std::time::Instant::now() + Duration::from_millis(3500);
                while std::time::Instant::now() < deadline {
                    if tokio::fs::metadata(&dest)
                        .await
                        .map(|m| m.len() > 0)
                        .unwrap_or(false)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(80)).await;
                }
                let _ = tokio::fs::remove_file(&req_path).await;
            }

            if !tokio::fs::metadata(&dest)
                .await
                .map(|m| m.len() > 0)
                .unwrap_or(false)
            {
                return Err(
                    "screencapture failed (unsigned .app lacks macOS Screen Recording permission and no QA capture helper responded)"
                        .to_string(),
                );
            }
        }
    }

    Ok(dest.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn qa_write_report(report: serde_json::Value) -> Result<String, String> {
    let enabled = std::env::var("DEX_QA_WALKTHROUGH")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    if !enabled {
        return Err("QA report write is only enabled when DEX_QA_WALKTHROUGH=1".to_string());
    }
    let out_dir = qa_output_dir();
    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| format!("Failed to create QA dir '{}': {e}", out_dir.display()))?;
    let dest = out_dir.join("qa_report.json");
    let json = serde_json::to_string_pretty(&report)
        .map_err(|e| format!("Failed to serialize QA report: {e}"))?;
    tokio::fs::write(&dest, json)
        .await
        .map_err(|e| format!("Failed to write QA report: {e}"))?;
    Ok(dest.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn qa_exit_app(app: AppHandle) -> Result<(), String> {
    let enabled = std::env::var("DEX_QA_WALKTHROUGH")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    if !enabled {
        return Err("qa_exit_app is only enabled when DEX_QA_WALKTHROUGH=1".to_string());
    }
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_history_restores_prior_turns_in_order() {
        let provider: Arc<dyn LlmProvider> = Arc::new(OpenAiProvider::lm_studio("m"));
        let mut agent = ReActAgent::new(provider, ToolRegistry::new());
        let turn = |role: &str, content: &str| ChatHistoryTurn {
            role: role.to_string(),
            content: content.to_string(),
        };
        let history = vec![
            turn("user", "me llamo Robii"),
            turn("assistant", "¡Hola Robii!"),
            turn("system", "ignored"),
            turn("user", "   "),
        ];
        replay_history(&mut agent, &history);
        let msgs = agent.context().messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content.as_deref(), Some("me llamo Robii"));
        assert_eq!(msgs[1].content.as_deref(), Some("¡Hola Robii!"));

        let long: Vec<_> = (0..100).map(|i| turn("user", &format!("t{i}"))).collect();
        let mut agent2 = ReActAgent::new(Arc::new(OpenAiProvider::lm_studio("m")), ToolRegistry::new());
        replay_history(&mut agent2, &long);
        assert_eq!(agent2.context().messages().len(), MAX_HISTORY_TURNS);
        assert_eq!(agent2.context().messages()[0].content.as_deref(), Some("t60"));
    }

    #[test]
    fn test_to_base64_standard() {
        assert_eq!(to_base64(b""), "");
        assert_eq!(to_base64(b"f"), "Zg==");
        assert_eq!(to_base64(b"fo"), "Zm8=");
        assert_eq!(to_base64(b"foo"), "Zm9v");
        assert_eq!(to_base64(b"Dex Autonomous Agent"), "RGV4IEF1dG9ub21vdXMgQWdlbnQ=");
    }

    #[test]
    fn test_runtime_vision_capability_blocks_mlx_lm_server() {
        assert!(!runtime_supports_vision_payload("dex-local", "gemma-4-vision", Some("mlx")));
        assert!(!runtime_supports_vision_payload("dex-local", "qwen3.5-9b", Some("llamacpp")));
        assert!(runtime_supports_vision_payload("openai", "gpt-4o", None));
        assert!(runtime_supports_vision_payload("ollama", "qwen3.5-vl", None));
        assert!(is_multimodal_rejection_error(
            "ValueError: Only 'text' content type is supported."
        ));
    }
}
