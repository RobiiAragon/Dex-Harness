use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

pub fn register_global_shortcuts(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let mut registered = false;

    // Attempt primary shortcut: Option+Space (Raycast style)
    if let Ok(shortcut) = "Option+Space".parse::<Shortcut>() {
        if app.global_shortcut().register(shortcut).is_ok() {
            tracing::info!("Registered primary global shortcut: Option+Space");
            registered = true;
        }
    }

    // Fallback: Command+Shift+D
    if !registered {
        if let Ok(fallback) = "CommandOrControl+Shift+D".parse::<Shortcut>() {
            if app.global_shortcut().register(fallback).is_ok() {
                tracing::info!("Registered fallback global shortcut: CommandOrControl+Shift+D");
            }
        }
    }

    Ok(())
}
