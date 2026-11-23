use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use async_trait::async_trait;
use enigo::{Button, Coordinate, Direction, Enigo, Keyboard, Mouse, Settings};
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::{info, instrument};
use xcap::Monitor;

use harness_core::{Tool, ToolError};

/// Helper to get primary monitor resolution.
pub fn get_primary_monitor_resolution() -> Result<(u32, u32), String> {
    let monitors = Monitor::all().map_err(|e| format!("Failed to list monitors: {e}"))?;
    let monitor = monitors
        .into_iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| Monitor::all().ok()?.into_iter().next())
        .ok_or_else(|| "No active monitors detected".to_string())?;

    let width = monitor.width().map_err(|e| format!("Failed to read monitor width: {e}"))?;
    let height = monitor.height().map_err(|e| format!("Failed to read monitor height: {e}"))?;

    Ok((width, height))
}

// =========================================================================
// 1. Desktop Screenshot Tool
// =========================================================================

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DesktopScreenshotArgs {
    /// Optional custom destination path for the screenshot. Defaults to .dex/screenshots/
    pub destination_path: Option<String>,
}

#[derive(Default, Clone)]
pub struct DesktopScreenshotTool;

impl DesktopScreenshotTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for DesktopScreenshotTool {
    fn name(&self) -> &str {
        "desktop_screenshot"
    }

    fn description(&self) -> &str {
        "Captures a screenshot of the primary desktop monitor and saves it to .dex/screenshots/. Returns the file path and image dimensions."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(DesktopScreenshotArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "destination_path": { "type": "string", "description": "Custom file path" }
            }
        }))
    }

    #[instrument(skip(self, arguments))]
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: DesktopScreenshotArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        tokio::task::spawn_blocking(move || -> Result<String, ToolError> {
            let monitors = Monitor::all().map_err(|e| ToolError::ExecutionFailed {
                name: "desktop_screenshot".to_string(),
                message: format!("Failed to query monitors: {e}"),
            })?;

            let monitor = monitors
                .into_iter()
                .find(|m| m.is_primary().unwrap_or(false))
                .or_else(|| Monitor::all().ok()?.into_iter().next())
                .ok_or_else(|| ToolError::ExecutionFailed {
                    name: "desktop_screenshot".to_string(),
                    message: "No active monitors found".to_string(),
                })?;

            let width = monitor.width().map_err(|e| ToolError::ExecutionFailed {
                name: "desktop_screenshot".to_string(),
                message: format!("Failed to read monitor width: {e}"),
            })?;

            let height = monitor.height().map_err(|e| ToolError::ExecutionFailed {
                name: "desktop_screenshot".to_string(),
                message: format!("Failed to read monitor height: {e}"),
            })?;

            let name = monitor.name().unwrap_or_else(|_| "Primary Monitor".to_string());

            let image = monitor.capture_image().map_err(|e| ToolError::ExecutionFailed {
                name: "desktop_screenshot".to_string(),
                message: format!("Failed to capture monitor screen: {e}"),
            })?;

            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let out_path: PathBuf = match args.destination_path {
                Some(p) => harness_core::resolve_safe_path(p),
                None => {
                    let dir = harness_core::resolve_dex_subpath("screenshots");
                    let _ = std::fs::create_dir_all(&dir);
                    dir.join(format!("desktop_{timestamp}.png"))
                }
            };

            if let Some(parent) = out_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            image.save(&out_path).map_err(|e| ToolError::ExecutionFailed {
                name: "desktop_screenshot".to_string(),
                message: format!("Failed to save screenshot image: {e}"),
            })?;

            info!(path = %out_path.display(), width = width, height = height, "Captured desktop screenshot");

            Ok(format!(
                "Desktop screenshot captured successfully:\n- Path: {}\n- Dimensions: {}x{} pixels\n- Monitor: {}",
                out_path.display(),
                width,
                height,
                name
            ))
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: format!("Screenshot task panicked: {e}"),
        })?
    }
}

// =========================================================================
// 2. Desktop Mouse Tool
// =========================================================================

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MouseAction {
    Move,
    Click,
    DoubleClick,
    RightClick,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DesktopMouseArgs {
    pub action: MouseAction,
    /// Target X coordinate on screen
    pub x: i32,
    /// Target Y coordinate on screen
    pub y: i32,
}

#[derive(Default, Clone)]
pub struct DesktopMouseTool;

impl DesktopMouseTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for DesktopMouseTool {
    fn name(&self) -> &str {
        "desktop_mouse"
    }

    fn description(&self) -> &str {
        "Simulates desktop mouse interactions (move, click, double_click, right_click) at specified coordinates. Automatically validates coordinates against monitor boundaries."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(DesktopMouseArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["move", "click", "double_click", "right_click"] },
                "x": { "type": "integer" },
                "y": { "type": "integer" }
            },
            "required": ["action", "x", "y"]
        }))
    }

    #[instrument(skip(self, arguments))]
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: DesktopMouseArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        tokio::task::spawn_blocking(move || -> Result<String, ToolError> {
            // Validate coordinates within screen bounds
            if let Ok((max_x, max_y)) = get_primary_monitor_resolution() {
                if args.x < 0 || args.y < 0 || (args.x as u32) > max_x || (args.y as u32) > max_y {
                    return Err(ToolError::ExecutionFailed {
                        name: "desktop_mouse".to_string(),
                        message: format!(
                            "Coordinates ({}, {}) are out of screen bounds ({}x{})",
                            args.x, args.y, max_x, max_y
                        ),
                    });
                }
            }

            let mut enigo = Enigo::new(&Settings::default()).map_err(|e| {
                ToolError::ExecutionFailed {
                    name: "desktop_mouse".to_string(),
                    message: format!("Failed to initialize input simulator: {e}"),
                }
            })?;

            enigo
                .move_mouse(args.x, args.y, Coordinate::Abs)
                .map_err(|e| ToolError::ExecutionFailed {
                    name: "desktop_mouse".to_string(),
                    message: format!("Failed to move mouse: {e}"),
                })?;

            match args.action {
                MouseAction::Move => Ok(format!("Mouse moved to ({}, {})", args.x, args.y)),
                MouseAction::Click => {
                    enigo.button(Button::Left, Direction::Click).map_err(|e| {
                        ToolError::ExecutionFailed {
                            name: "desktop_mouse".to_string(),
                            message: format!("Failed to click mouse: {e}"),
                        }
                    })?;
                    Ok(format!("Clicked left mouse button at ({}, {})", args.x, args.y))
                }
                MouseAction::DoubleClick => {
                    enigo.button(Button::Left, Direction::Click).map_err(|e| {
                        ToolError::ExecutionFailed {
                            name: "desktop_mouse".to_string(),
                            message: format!("Failed to click mouse: {e}"),
                        }
                    })?;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    enigo.button(Button::Left, Direction::Click).map_err(|e| {
                        ToolError::ExecutionFailed {
                            name: "desktop_mouse".to_string(),
                            message: format!("Failed to click mouse: {e}"),
                        }
                    })?;
                    Ok(format!("Double clicked at ({}, {})", args.x, args.y))
                }
                MouseAction::RightClick => {
                    enigo.button(Button::Right, Direction::Click).map_err(|e| {
                        ToolError::ExecutionFailed {
                            name: "desktop_mouse".to_string(),
                            message: format!("Failed to right-click: {e}"),
                        }
                    })?;
                    Ok(format!("Right-clicked at ({}, {})", args.x, args.y))
                }
            }
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: format!("Mouse task panicked: {e}"),
        })?
    }
}

// =========================================================================
// 3. Desktop Keyboard Tool
// =========================================================================

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DesktopKeyboardArgs {
    /// Text to type directly onto the active window.
    pub text: Option<String>,
    /// Special key to press (e.g. "Return", "Escape", "Tab", "Space").
    pub special_key: Option<String>,
}

#[derive(Default, Clone)]
pub struct DesktopKeyboardTool;

impl DesktopKeyboardTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for DesktopKeyboardTool {
    fn name(&self) -> &str {
        "desktop_keyboard"
    }

    fn description(&self) -> &str {
        "Simulates desktop keyboard typing and special key presses on the active window."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(DesktopKeyboardArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "Text to type" },
                "special_key": { "type": "string", "description": "Special key: Return, Escape, Tab, etc." }
            }
        }))
    }

    #[instrument(skip(self, arguments))]
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: DesktopKeyboardArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        tokio::task::spawn_blocking(move || -> Result<String, ToolError> {
            let mut enigo = Enigo::new(&Settings::default()).map_err(|e| {
                ToolError::ExecutionFailed {
                    name: "desktop_keyboard".to_string(),
                    message: format!("Failed to initialize input simulator: {e}"),
                }
            })?;

            let mut actions_taken = Vec::new();

            if let Some(ref text) = args.text {
                enigo.text(text).map_err(|e| ToolError::ExecutionFailed {
                    name: "desktop_keyboard".to_string(),
                    message: format!("Failed to type text: {e}"),
                })?;
                actions_taken.push(format!("Typed text: '{text}'"));
            }

            if let Some(ref key_str) = args.special_key {
                let key = match key_str.to_lowercase().as_str() {
                    "return" | "enter" => enigo::Key::Return,
                    "escape" | "esc" => enigo::Key::Escape,
                    "tab" => enigo::Key::Tab,
                    "space" => enigo::Key::Space,
                    "backspace" => enigo::Key::Backspace,
                    "up" => enigo::Key::UpArrow,
                    "down" => enigo::Key::DownArrow,
                    "left" => enigo::Key::LeftArrow,
                    "right" => enigo::Key::RightArrow,
                    other => {
                        return Err(ToolError::InvalidArguments {
                            name: "desktop_keyboard".to_string(),
                            reason: format!("Unsupported special key '{other}'"),
                        });
                    }
                };

                enigo.key(key, Direction::Click).map_err(|e| {
                    ToolError::ExecutionFailed {
                        name: "desktop_keyboard".to_string(),
                        message: format!("Failed to press key: {e}"),
                    }
                })?;
                actions_taken.push(format!("Pressed special key: '{key_str}'"));
            }

            if actions_taken.is_empty() {
                return Err(ToolError::InvalidArguments {
                    name: "desktop_keyboard".to_string(),
                    reason: "Neither 'text' nor 'special_key' was provided".to_string(),
                });
            }

            Ok(actions_taken.join("\n"))
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: format!("Keyboard task panicked: {e}"),
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolution_check() {
        // Query monitor resolution or handle headless CI
        if let Ok((w, h)) = get_primary_monitor_resolution() {
            assert!(w > 0);
            assert!(h > 0);
        }
    }
}
