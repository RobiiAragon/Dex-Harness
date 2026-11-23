#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod shortcuts;
mod state;
mod tray;

use std::error::Error;
use std::time::Duration;
use harness_core::EventBus;
use state::DesktopState;
use tauri::Emitter;
use tauri_plugin_global_shortcut::ShortcutState;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,harness_desktop=debug,harness_extensions=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("Initializing Dex Desktop (macOS Native Agent Harness)");

    let event_bus = EventBus::new(512);
    let mut rx = event_bus.subscribe();

    let desktop_state = DesktopState::new(event_bus.clone());
    let mode_for_tray = desktop_state.mode.clone();
    let supervisor_for_reaper = desktop_state.extension_supervisor.clone();
    let reaper_cancel = desktop_state.reaper_cancel.clone();

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
            commands::list_extension_catalog,
            commands::list_installed_extensions,
            commands::install_catalog_extension,
            commands::add_custom_mcp_extension,
            commands::toggle_extension,
            commands::start_extension,
            commands::stop_extension,
            commands::remove_extension,
            commands::update_extension_timeout,
            commands::start_voice_recording,
            commands::stop_and_transcribe,
            commands::speak_text,
            commands::list_audio_media,
            commands::run_ocr_image,
            commands::parse_document,
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
            // IMPORTANT: Use `tauri::async_runtime::spawn` because `setup` runs on AppKit's main thread.
            let handle_for_events = handle.clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(event) = rx.recv().await {
                    if let Err(e) = handle_for_events.emit("dex-event", &event) {
                        tracing::trace!("Failed to emit dex-event to webview: {}", e);
                    }
                }
            });

            // Background supervisor reaper: auto-unloads idle sidecars/MCPs to free RAM for local LLMs
            tauri::async_runtime::spawn(async move {
                let interval = Duration::from_secs(30);
                loop {
                    tokio::select! {
                        _ = reaper_cancel.cancelled() => {
                            tracing::info!("Extension idle reaper stopped");
                            break;
                        }
                        _ = tokio::time::sleep(interval) => {
                            let unloaded = supervisor_for_reaper.reap_idle_extensions().await;
                            if !unloaded.is_empty() {
                                tracing::info!(extensions = ?unloaded, "Reclaimed RAM from idle extensions");
                            }
                        }
                    }
                }
            });

            tracing::info!("Dex Desktop setup successfully completed");
            Ok(())
        });

    builder.run(tauri::generate_context!())?;
    Ok(())
}
