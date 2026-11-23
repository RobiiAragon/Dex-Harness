use std::sync::Arc;
use async_trait::async_trait;
use harness_audio::VoiceRecorderHandle;
use harness_core::{AgentEvent, CoreError, EventBus, ExecutionMode, Hook, HookAction};
use harness_extensions::ExtensionSupervisor;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::sync::{oneshot, Mutex, RwLock};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolApprovalPayload {
    pub tool: String,
    pub arguments: serde_json::Value,
}

pub struct DesktopState {
    pub event_bus: EventBus,
    pub mode: Arc<RwLock<ExecutionMode>>,
    pub cancel_sender: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    pub approval_sender: Arc<Mutex<Option<oneshot::Sender<bool>>>>,
    pub active_goal: Arc<RwLock<Option<String>>>,
    pub download_cancel: Arc<Mutex<Option<CancellationToken>>>,
    pub extension_supervisor: Arc<ExtensionSupervisor>,
    pub reaper_cancel: CancellationToken,
    pub active_recorder: Arc<Mutex<Option<VoiceRecorderHandle>>>,
}

impl DesktopState {
    pub fn new(event_bus: EventBus) -> Self {
        let extension_supervisor = Arc::new(ExtensionSupervisor::new(event_bus.clone()));
        Self {
            event_bus,
            mode: Arc::new(RwLock::new(ExecutionMode::Autopilot)),
            cancel_sender: Arc::new(Mutex::new(None)),
            approval_sender: Arc::new(Mutex::new(None)),
            active_goal: Arc::new(RwLock::new(None)),
            download_cancel: Arc::new(Mutex::new(None)),
            extension_supervisor,
            reaper_cancel: CancellationToken::new(),
            active_recorder: Arc::new(Mutex::new(None)),
        }
    }
}

pub fn is_sensitive_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "write_file"
            | "edit_file"
            | "execute_shell"
            | "pty_shell"
            | "git_worktree"
            | "desktop_mouse"
            | "desktop_keyboard"
            | "browser_click"
            | "browser_type"
    )
}

pub struct DesktopSecurityHook {
    pub app_handle: AppHandle,
    pub mode: Arc<RwLock<ExecutionMode>>,
    pub approval_sender: Arc<Mutex<Option<oneshot::Sender<bool>>>>,
}

#[async_trait]
impl Hook for DesktopSecurityHook {
    async fn on_event(&self, event: &AgentEvent) -> Result<HookAction, CoreError> {
        if let AgentEvent::PreToolUse {
            ref tool_name,
            ref arguments,
        } = event
        {
            if is_sensitive_tool(tool_name) {
                let current_mode = *self.mode.read().await;
                match current_mode {
                    ExecutionMode::Autopilot => Ok(HookAction::Continue),
                    ExecutionMode::Plan => {
                        let reason = format!(
                            "Tool '{}' execution blocked in Plan mode (read-only mode active)",
                            tool_name
                        );
                        tracing::warn!("{}", reason);
                        Ok(HookAction::Halt(reason))
                    }
                    ExecutionMode::Safe => {
                        tracing::info!(tool = %tool_name, "Safe mode requesting UI approval");
                        let (tx, rx) = oneshot::channel();
                        {
                            let mut lock = self.approval_sender.lock().await;
                            *lock = Some(tx);
                        }

                        let payload = ToolApprovalPayload {
                            tool: tool_name.clone(),
                            arguments: arguments.clone(),
                        };

                        if let Err(e) = self.app_handle.emit("dex-approval-request", payload) {
                            tracing::warn!("Failed to emit dex-approval-request to webview: {}", e);
                        }

                        match rx.await {
                            Ok(true) => {
                                tracing::info!(tool = %tool_name, "Tool execution approved by user");
                                Ok(HookAction::Continue)
                            }
                            Ok(false) => {
                                let reason = format!("User rejected execution of tool '{}'", tool_name);
                                tracing::warn!("{}", reason);
                                Ok(HookAction::Halt(reason))
                            }
                            Err(_) => {
                                let reason = format!(
                                    "Approval channel dropped or cancelled for tool '{}'",
                                    tool_name
                                );
                                tracing::warn!("{}", reason);
                                Ok(HookAction::Halt(reason))
                            }
                        }
                    }
                }
            } else {
                Ok(HookAction::Continue)
            }
        } else {
            Ok(HookAction::Continue)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_sensitive_tool_classification() {
        assert!(is_sensitive_tool("write_file"));
        assert!(is_sensitive_tool("edit_file"));
        assert!(is_sensitive_tool("execute_shell"));
        assert!(is_sensitive_tool("pty_shell"));
        assert!(is_sensitive_tool("desktop_mouse"));
        assert!(is_sensitive_tool("desktop_keyboard"));
        assert!(is_sensitive_tool("browser_click"));
        assert!(is_sensitive_tool("browser_type"));
        assert!(is_sensitive_tool("git_worktree"));

        // Read-only tools must not be classified as sensitive
        assert!(!is_sensitive_tool("read_file"));
        assert!(!is_sensitive_tool("grep_search"));
        assert!(!is_sensitive_tool("find_files"));
        assert!(!is_sensitive_tool("browser_navigate"));
        assert!(!is_sensitive_tool("browser_extract_dom"));
        assert!(!is_sensitive_tool("browser_screenshot"));
        assert!(!is_sensitive_tool("desktop_screenshot"));
    }

    #[tokio::test]
    async fn test_desktop_state_initialization() {
        let bus = EventBus::new(16);
        let state = DesktopState::new(bus);
        let mode = *state.mode.read().await;
        assert_eq!(mode, ExecutionMode::Autopilot);
    }
}
