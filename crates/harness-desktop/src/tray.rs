use std::sync::Arc;
use harness_core::ExecutionMode;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::RwLock;

pub fn setup_tray(
    app: &AppHandle,
    mode: Arc<RwLock<ExecutionMode>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let toggle_i = MenuItem::with_id(app, "toggle", "Open / Hide Dex", true, None::<&str>)?;
    let auto_i = MenuItem::with_id(app, "mode_autopilot", "Mode: Autopilot 🚀", true, None::<&str>)?;
    let safe_i = MenuItem::with_id(app, "mode_safe", "Mode: Safe 🛡️", true, None::<&str>)?;
    let plan_i = MenuItem::with_id(app, "mode_plan", "Mode: Plan 📝", true, None::<&str>)?;
    let cookbook_i = MenuItem::with_id(app, "cookbook", "Hardware Cookbook 🖥️", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let quit_i = MenuItem::with_id(app, "quit", "Quit Dex", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &toggle_i,
            &sep1,
            &auto_i,
            &safe_i,
            &plan_i,
            &cookbook_i,
            &PredefinedMenuItem::separator(app)?,
            &quit_i,
        ],
    )?;

    let icon_bytes = include_bytes!("../icons/32x32.png");
    let tray_icon = tauri::image::Image::from_bytes(icon_bytes)?;

    let app_handle_for_menu = app.clone();
    let mode_for_menu = mode.clone();

    let _tray = TrayIconBuilder::new()
        .icon(tray_icon)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Dex - Autonomous Agent Harness")
        .on_menu_event(move |_app, event| {
            let id = event.id.as_ref();
            let app_h = app_handle_for_menu.clone();
            let mode_h = mode_for_menu.clone();

            match id {
                "quit" => {
                    tracing::info!("Quit clicked from System Tray");
                    app_h.exit(0);
                }
                "toggle" => {
                    toggle_main_window(&app_h);
                }
                "mode_autopilot" => {
                    tauri::async_runtime::spawn(async move {
                        let mut m = mode_h.write().await;
                        *m = ExecutionMode::Autopilot;
                        let _ = app_h.emit("dex-mode-changed", serde_json::json!({ "mode": "Autopilot" }));
                    });
                }
                "mode_safe" => {
                    tauri::async_runtime::spawn(async move {
                        let mut m = mode_h.write().await;
                        *m = ExecutionMode::Safe;
                        let _ = app_h.emit("dex-mode-changed", serde_json::json!({ "mode": "Safe" }));
                    });
                }
                "mode_plan" => {
                    tauri::async_runtime::spawn(async move {
                        let mut m = mode_h.write().await;
                        *m = ExecutionMode::Plan;
                        let _ = app_h.emit("dex-mode-changed", serde_json::json!({ "mode": "Plan" }));
                    });
                }
                "cookbook" => {
                    show_main_window(&app_h);
                    let _ = app_h.emit("dex-open-tab", serde_json::json!({ "tab": "cookbook" }));
                }
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

pub fn toggle_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        match window.is_visible() {
            Ok(true) => {
                let _ = window.hide();
            }
            _ => {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }
    }
}

pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}
