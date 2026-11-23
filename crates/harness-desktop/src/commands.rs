use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use harness_models::{
    import_local_gguf as import_gguf_impl, parse_quantization, HfClient, HfGgufFile,
    HfModelSummary, ModelDownloader, ModelMetadata, ModelRegistry, ModelSource, OllamaBridge,
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
        other => Err(format!(
            "Unsupported provider '{}'. Supported: ollama, openai",
            other
        )),
    }
}

fn build_tools(
    provider: Arc<dyn LlmProvider>,
    skill_loader: ProgressiveSkillLoader,
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

    // Subagents Orchestrator
    let master_copy = registry.clone();
    registry.register(Arc::new(
        SpawnSubagentTool::new(provider, master_copy).with_event_bus(event_bus),
    ));

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

    let tools = build_tools(
        provider_instance.clone(),
        skill_loader,
        state.event_bus.clone(),
    );

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
            "You are Dex, a high-performance autonomous AI agent harness in Rust with native macOS, terminal, and browser capabilities.\n\nAvailable Skills:\n{}\nUse `load_skill` when relevant.",
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

    tokio::spawn(async move {
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
pub async fn search_hf_models(query: String, limit: Option<usize>) -> Result<Vec<HfModelSummary>, String> {
    let client = HfClient::new(None);
    client.search_models(&query, limit.unwrap_or(15)).await.map_err(|e| e.to_string())
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

    tokio::spawn(async move {
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
    import_gguf_impl(p, name.as_deref(), true, to_ollama, bridge.as_ref())
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
                    size_bytes: tag.size,
                    created_at: 0,
                });
            }
        }
    }

    Ok(models)
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

    tokio::spawn(async move {
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
