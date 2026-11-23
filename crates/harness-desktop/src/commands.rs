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
    register_audio_tools, synthesize_speech, AutoSttEngine, TtsResult, VoiceRecorderHandle,
    DEFAULT_MUSIC_DIR, DEFAULT_RECORDINGS_DIR, DEFAULT_SFX_DIR, DEFAULT_TTS_DIR,
};
use harness_docs::{
    parse_document as parse_doc_impl, register_docs_tools, run_ocr_on_image, DocumentParseResult,
    OcrResult, DEFAULT_MAX_INLINE_CHARS,
};
use harness_extensions::{
    builtin_catalog, register_enabled_mcp_extensions, ExtensionCategory, ExtensionKind,
    ExtensionManifest, ExtensionRuntimeInfo, ExtensionSupervisor, DEFAULT_IDLE_TIMEOUT_SECS,
};
use harness_media::{
    generate_image as generate_image_impl, inspect_video as inspect_video_impl,
    list_visual_media_items, list_workflows, process_image as process_image_impl,
    register_media_tools, upscale_image as upscale_image_impl, ImageBackend, ImageGenRequest,
    ImageGenResult, ImageProcessOptions, ImageProcessResult, VideoMetadata, DEFAULT_WORKFLOWS_DIR,
};
use harness_models::{
    compute_group_key, detect_local_servers as detect_servers_impl, import_local_model,
    infer_capabilities, parse_quantization, scan_model_dirs, DiscoveredModel, HfClient, HfGgufFile,
    HfModelSummary, LocalServerInfo, ModelDirsConfig, ModelDownloader, ModelFormat, ModelMetadata,
    ModelRegistry, ModelSource, OllamaBridge, LM_STUDIO_DEFAULT_URL, MLX_LM_DEFAULT_PORT,
};
use tokio_util::sync::CancellationToken;
use harness_computer::{
    BrowserClickTool, BrowserExtractDomTool, BrowserNavigateTool, BrowserScreenshotTool,
    BrowserSession, BrowserTypeTool, DesktopKeyboardTool, DesktopMouseTool, DesktopScreenshotTool,
};
use harness_core::cookbook::{HardwareProfile, ModelRecommendation};
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
        "ollama" => {
            let mut p = OllamaProvider::new(model);
            if let Some(base) = api_base {
                p = p.with_base_url(base);
            }
            Ok(Arc::new(p))
        }
        "openai" => {
            let mut p = OpenAiProvider::new(model);
            if let Some(base) = api_base {
                p = p.with_base_url(base);
            }
            Ok(Arc::new(p))
        }
        "lmstudio" | "lm_studio" | "bionic" | "mlx" => {
            let mut p = OpenAiProvider::lm_studio(model);
            if let Some(base) = api_base {
                p = p.with_base_url(base);
            }
            Ok(Arc::new(p))
        }
        other => Err(format!(
            "Unsupported provider '{}'. Supported: ollama, openai, lmstudio",
            other
        )),
    }
}

fn build_tools(
    provider: Arc<dyn LlmProvider>,
    skill_loader: ProgressiveSkillLoader,
    ext_supervisor: Arc<ExtensionSupervisor>,
    event_bus: EventBus,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    // Surgical Code & File Tools
    registry.register(Arc::new(ReadFileTool::new()));
    registry.register(Arc::new(WriteFileTool::new()));
    registry.register(Arc::new(EditFileTool::new()));

    // Search & Repository Navigation
    registry.register(Arc::new(FindFilesTool::new()));
    registry.register(Arc::new(GrepSearchTool::new()));

    // Terminal Execution & PTY
    registry.register(Arc::new(ExecuteShellTool::new()));
    registry.register(Arc::new(PtyShellTool::new()));

    // Git Branch & Worktree Management
    registry.register(Arc::new(GitWorktreeTool::new()));

    // Project & Task Graph Management
    registry.register(Arc::new(
        ManageTasksTool::new(".dex/tasks.json").with_event_bus(event_bus.clone()),
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

#[tauri::command]
pub async fn run_goal(
    app: AppHandle,
    state: State<'_, DesktopState>,
    goal: String,
    mode: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    api_base: Option<String>,
) -> Result<String, String> {
    if goal.trim().is_empty() {
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

    let provider_name = provider.unwrap_or_else(|| "ollama".to_string());
    let model_name = model.unwrap_or_else(|| "qwen2.5:7b".to_string());
    let provider_instance = build_provider(&provider_name, &model_name, api_base.as_deref())?;

    // Load progressive skills
    let mut skill_loader = ProgressiveSkillLoader::new();
    skill_loader.scan_standard_locations().await;
    let skills_summary = skill_loader.generate_prompt_index();

    let mut tools = build_tools(
        provider_instance.clone(),
        skill_loader,
        state.extension_supervisor.clone(),
        state.event_bus.clone(),
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

    if !skills_summary.is_empty() {
        let prompt = format!(
            "You are Dex, a high-performance autonomous AI agent harness in Rust with native macOS, terminal, browser, and external MCP extension capabilities.\nUse `search_tools` to discover and activate specialized external extension tools.\n\nAvailable Skills:\n{}\nUse `load_skill` when relevant.",
            skills_summary
        );
        agent = agent.with_system_prompt(prompt);
    }

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
    let goal_cloned = goal.clone();

    tauri::async_runtime::spawn(async move {
        tracing::info!(goal = %goal_cloned, "Starting autonomous execution in Dex Desktop");

        let run_future = agent.run(&goal_cloned);
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
                        let err_msg = e.to_string();
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
    let profile = HardwareProfile::detect();
    let recommendations = profile.recommend_models();
    let report = profile.generate_report();

    Ok(CookbookResponse {
        profile,
        recommendations,
        report,
    })
}

#[tauri::command]
pub async fn get_tasks() -> Result<Vec<TaskItem>, String> {
    let board = TaskBoard::load_or_default(TaskBoard::DEFAULT_PATH).await;
    Ok(board.tasks)
}

#[tauri::command]
pub async fn get_screenshots() -> Result<Vec<ScreenshotEntry>, String> {
    let dir = Path::new(".dex/screenshots");
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    let mut read_dir = tokio::fs::read_dir(dir)
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
pub async fn activate_mlx_model(
    state: State<'_, DesktopState>,
    model_path: String,
    model_name: String,
) -> Result<ActivatedModelResponse, String> {
    // 1. If LM Studio / Bionic (:1234) is already serving a matching model, attach directly!
    let servers = detect_servers_impl().await;
    if let Some(lm) = servers.iter().find(|s| s.id == "lmstudio" && s.reachable) {
        let needle = model_name.to_lowercase();
        let path_leaf = Path::new(&model_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase();
        if let Some(matched) = lm.loaded_models.iter().find(|loaded| {
            let l = loaded.to_lowercase();
            l.contains(&needle) || (!path_leaf.is_empty() && l.contains(&path_leaf))
        }) {
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

    // 2. Otherwise register & launch on-demand `mlx_lm.server` via ExtensionSupervisor (with idle auto-unload)
    if !tokio::fs::try_exists(Path::new(&model_path))
        .await
        .unwrap_or(false)
    {
        return Err(format!("MLX model folder does not exist: '{model_path}'"));
    }

    let port = MLX_LM_DEFAULT_PORT;
    let sidecar_id = "mlx-lm-active".to_string();
    let base_url = format!("http://127.0.0.1:{port}/v1");

    // Stop previous MLX-LM server if another model was mounted
    let _ = state.extension_supervisor.stop_extension(&sidecar_id).await;

    let manifest = ExtensionManifest {
        id: sidecar_id.clone(),
        name: format!("MLX-LM ({model_name})"),
        category: ExtensionCategory::Custom,
        description: format!("On-demand Apple Silicon MLX inference server for {model_name}"),
        kind: ExtensionKind::LocalSidecar {
            command: "python3".to_string(),
            args: vec![
                "-m".to_string(),
                "mlx_lm.server".to_string(),
                "--model".to_string(),
                model_path.clone(),
                "--port".to_string(),
                port.to_string(),
            ],
            port,
            healthcheck_url: format!("{base_url}/models"),
            env: HashMap::new(),
        },
        estimated_ram_mb: 8192,
        idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
        install_hint: "Install Apple MLX LM server: `pip install mlx-lm` or serve the model in LM Studio on port 1234.".to_string(),
        enabled: true,
        tags: vec!["mlx".to_string(), "llm".to_string(), "apple-silicon".to_string()],
    };

    state
        .extension_supervisor
        .add_custom_extension(manifest)
        .await
        .map_err(|e| e.to_string())?;

    match state.extension_supervisor.start_extension(&sidecar_id).await {
        Ok(_) => Ok(ActivatedModelResponse {
            provider: "openai".to_string(),
            model: model_path,
            api_base: Some(base_url.clone()),
            status_message: format!(
                "Started on-demand mlx_lm.server on {base_url} (auto-unloads after {}s idle)",
                DEFAULT_IDLE_TIMEOUT_SECS
            ),
            sidecar_id: Some(sidecar_id),
        }),
        Err(e) => Err(format!(
            "Could not launch `python3 -m mlx_lm.server` ({e}). Install `mlx-lm` (`pip install mlx-lm`) or load '{model_name}' in LM Studio (:1234)."
        )),
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
    pub transcript: Option<String>,
    pub backend: Option<String>,
    pub stt_warning: Option<String>,
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
pub async fn start_voice_recording(state: State<'_, DesktopState>) -> Result<String, String> {
    let mut lock = state.active_recorder.lock().await;
    if lock.is_some() {
        return Err("Voice recording is already in progress".to_string());
    }

    let handle = VoiceRecorderHandle::start(DEFAULT_RECORDINGS_DIR, Some(state.event_bus.clone()))
        .await
        .map_err(|e| e.to_string())?;
    *lock = Some(handle);
    Ok("Recording started".to_string())
}

#[tauri::command]
pub async fn stop_and_transcribe(
    state: State<'_, DesktopState>,
) -> Result<VoiceTranscribeResponse, String> {
    let handle = {
        let mut lock = state.active_recorder.lock().await;
        lock.take()
            .ok_or_else(|| "No active voice recording to stop".to_string())?
    };

    let rec = handle.stop().await.map_err(|e| e.to_string())?;
    let path_str = rec.path.to_string_lossy().to_string();

    match AutoSttEngine::transcribe(&rec.path, false).await {
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
                transcript: Some(stt.text),
                backend: Some(stt.backend),
                stt_warning: None,
            })
        }
        Err(err) => Ok(VoiceTranscribeResponse {
            audio_path: path_str,
            duration_secs: rec.duration_secs,
            transcript: None,
            backend: None,
            stt_warning: Some(err.to_string()),
        }),
    }
}

#[tauri::command]
pub async fn speak_text(
    state: State<'_, DesktopState>,
    text: String,
    voice: Option<String>,
) -> Result<TtsResult, String> {
    synthesize_speech(
        &text,
        voice.as_deref(),
        None,
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
        let dir = Path::new(dir_str);
        if !tokio::fs::try_exists(dir).await.unwrap_or(false) {
            continue;
        }
        let mut rd = match tokio::fs::read_dir(dir).await {
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
    list_workflows(Path::new(DEFAULT_WORKFLOWS_DIR))
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
    let dest = PathBuf::from(DEFAULT_3D_DIR).join("sample_cube.glb");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_base64_standard() {
        assert_eq!(to_base64(b""), "");
        assert_eq!(to_base64(b"f"), "Zg==");
        assert_eq!(to_base64(b"fo"), "Zm8=");
        assert_eq!(to_base64(b"foo"), "Zm9v");
        assert_eq!(to_base64(b"Dex Autonomous Agent"), "RGV4IEF1dG9ub21vdXMgQWdlbnQ=");
    }
}
