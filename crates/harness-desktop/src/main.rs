#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod projects;
mod shortcuts;
mod state;
mod tray;

use std::error::Error;
use std::path::PathBuf;
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
    let workspace_for_init = desktop_state.active_workspace_dir.clone();

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
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = event {
                let path_strings: Vec<String> = paths
                    .iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect();
                if !path_strings.is_empty() {
                    let _ = window.emit("dex-files-dropped", path_strings);
                }
            }
        })
        .manage(desktop_state)
        .invoke_handler(tauri::generate_handler![
            commands::run_goal,
            commands::stop_goal,
            commands::set_mode,
            commands::get_mode,
            commands::resolve_approval,
            commands::get_cookbook,
            commands::get_live_memory,
            commands::get_tasks,
            commands::get_screenshots,
            commands::search_hf_models,
            commands::get_hf_model_readme,
            commands::list_hf_files,
            commands::download_model,
            commands::cancel_download,
            commands::import_local_gguf,
            commands::list_installed_models,
            commands::list_model_dirs,
            commands::add_model_dir,
            commands::remove_model_dir,
            commands::scan_local_models,
            commands::detect_local_servers,
            commands::detect_runtimes,
            commands::install_runtime,
            commands::load_local_model,
            commands::unload_local_model,
            commands::get_active_loaded_model,
            commands::activate_mlx_model,
            commands::open_external_url,
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
            commands::get_audio_studio_state,
            commands::save_audio_settings_cmd,
            commands::list_cloned_voices,
            commands::save_cloned_voice,
            commands::delete_cloned_voice,
            commands::record_voice_sample,
            commands::test_mic_and_stt,
            commands::run_ocr_image,
            commands::parse_document,
            commands::generate_image,
            commands::upscale_image,
            commands::process_image,
            commands::list_visual_media,
            commands::inspect_video_file,
            commands::list_comfyui_workflows,
            commands::reveal_in_finder,
            commands::list_3d_models,
            commands::inspect_3d_model,
            commands::read_glb_base64,
            commands::create_sample_3d_model,
            commands::generate_3d_model,
            commands::convert_3d_model,
            commands::render_3d_preview,
            commands::inspect_unity_project,
            commands::export_model_to_unity,
            commands::check_blender_status,
            commands::list_projects,
            commands::create_project,
            commands::open_project_dialog,
            commands::select_project,
            commands::remove_project,
            commands::pick_attachment_files,
            commands::pick_file_dialog,
            commands::pick_folder_dialog,
            commands::inspect_attachment_paths,
            commands::list_project_sessions,
            commands::save_project_session,
            commands::load_project_session,
            commands::rename_project_session,
            commands::delete_project_session,
            commands::get_qa_walkthrough_config,
            commands::qa_capture_screenshot,
            commands::qa_write_report,
            commands::qa_exit_app,
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

            // Restore last active project workspace from ~/.dex/projects.json and anchor writable storage root
            tauri::async_runtime::spawn(async move {
                let store = projects::ProjectsStore::load_or_default().await;
                if let Some(active) = store.active_project() {
                    let p = PathBuf::from(&active.path);
                    if !harness_core::is_unwritable_system_root(&p)
                        && tokio::fs::try_exists(&p).await.unwrap_or(false)
                    {
                        {
                            let mut ws = workspace_for_init.write().await;
                            *ws = p.clone();
                        }
                        harness_core::set_default_storage_root(p.join(".dex"));
                        return;
                    }
                }
                harness_core::set_default_storage_root(harness_core::home_dex_dir());
            });

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
