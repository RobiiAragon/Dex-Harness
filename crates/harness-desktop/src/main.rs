#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod shortcuts;
mod state;
mod tray;

use std::error::Error;
use harness_core::EventBus;
use state::DesktopState;
use tauri::Emitter;
use tauri_plugin_global_shortcut::ShortcutState;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,harness_desktop=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("Initializing Dex Desktop (macOS Native Agent Harness)");

    let event_bus = EventBus::new(512);
    let mut rx = event_bus.subscribe();

    let desktop_state = DesktopState::new(event_bus.clone());
    let mode_for_tray = desktop_state.mode.clone();

    let builder = tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        tray::toggle_main_window(app);
                    }
                })
                .build(),
        )
        .manage(desktop_state)
        .invoke_handler(tauri::generate_handler![
            commands::run_goal,
            commands::stop_goal,
            commands::set_mode,
            commands::get_mode,
            commands::resolve_approval,
            commands::get_cookbook,
            commands::get_tasks,
            commands::get_screenshots,
            commands::search_hf_models,
            commands::list_hf_files,
            commands::download_model,
            commands::cancel_download,
            commands::import_local_gguf,
            commands::list_installed_models,
            commands::delete_model,
            commands::pull_ollama_model,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();

            // Setup macOS Menu Bar System Tray
            if let Err(e) = tray::setup_tray(&handle, mode_for_tray) {
                tracing::warn!("Failed to setup system tray: {}", e);
            }

            // Register Option+Space global shortcut
            if let Err(e) = shortcuts::register_global_shortcuts(&handle) {
                tracing::warn!("Failed to register global shortcuts: {}", e);
            }

            // Forward EventBus events in real time to the Apple WKWebView frontend
            let handle_for_events = handle.clone();
            tokio::spawn(async move {
                while let Ok(event) = rx.recv().await {
                    if let Err(e) = handle_for_events.emit("dex-event", &event) {
                        tracing::trace!("Failed to emit dex-event to webview: {}", e);
                    }
                }
            });

            tracing::info!("Dex Desktop setup successfully completed");
            Ok(())
        });

    builder.run(tauri::generate_context!())?;
    Ok(())
}
