use std::collections::HashMap;
use anyhow::{Context, Result};
use harness_core::EventBus;
use harness_extensions::{
    builtin_catalog, ExtensionCategory, ExtensionKind, ExtensionManifest, ExtensionStatus,
    ExtensionSupervisor, DEFAULT_IDLE_TIMEOUT_SECS,
};

pub fn show_catalog() {
    let catalog = builtin_catalog();
    println!("🧩 Dex-Harness Built-in Extension Catalog (MCP Servers & Local Sidecars)\n");

    let categories = [
        ExtensionCategory::Audio,
        ExtensionCategory::ImageVideo,
        ExtensionCategory::ThreeD,
        ExtensionCategory::OcrDocs,
        ExtensionCategory::GameDev,
        ExtensionCategory::Custom,
    ];

    for cat in categories {
        let items: Vec<&ExtensionManifest> = catalog
            .iter()
            .filter(|e| e.category == cat)
            .collect();
        if items.is_empty() {
            continue;
        }

        println!("=== {} ===", cat.display_label());
        for ext in items {
            let kind_label = match &ext.kind {
                ExtensionKind::McpStdio { command, args, .. } => {
                    format!("MCP stdio (`{} {}`)", command, args.join(" "))
                }
                ExtensionKind::McpSse { url } => format!("MCP SSE ({url})"),
                ExtensionKind::LocalSidecar { port, .. } => format!("Local Sidecar (port {port})"),
            };
            println!(
                "  • {} [id: {}] — ~{} MB RAM | idle unload: {}s",
                ext.name, ext.id, ext.estimated_ram_mb, ext.idle_timeout_secs
            );
            println!("    Tipo: {kind_label}");
            println!("    Desc: {}", ext.description);
            println!("    Setup: {}", ext.install_hint);
            println!();
        }
    }

    println!("💡 Instala una receta con: dex ext install <catalog_id>");
}

pub async fn list_extensions(event_bus: EventBus) -> Result<()> {
    let supervisor = ExtensionSupervisor::new(event_bus);
    let items = supervisor.list_with_status().await;

    println!("🧩 Installed Extensions (~/.dex/extensions/extensions.json):");
    if items.is_empty() {
        println!("   (No extensions installed yet. Run `dex ext catalog` or `dex ext install <id>`.)");
        return Ok(());
    }

    println!(
        "\n{:<20} {:<14} {:<10} {:<12} {:<10} {:<10}",
        "ID", "CATEGORY", "ENABLED", "STATUS", "RAM (MB)", "TIMEOUT"
    );
    println!(
        "{:-<20} {:-<14} {:-<10} {:-<12} {:-<10} {:-<10}",
        "", "", "", "", "", ""
    );

    for item in &items {
        let status_badge = match item.status {
            ExtensionStatus::Running => "🟢 Running",
            ExtensionStatus::Sleeping => "💤 Sleeping",
            ExtensionStatus::Stopped => "⚪ Stopped",
            ExtensionStatus::Error => "🔴 Error",
        };
        let enabled_str = if item.manifest.enabled { "yes" } else { "no" };
        println!(
            "{:<20} {:<14} {:<10} {:<12} {:<10} {}s",
            truncate_str(&item.manifest.id, 18),
            item.manifest.category.display_label(),
            enabled_str,
            status_badge,
            item.manifest.estimated_ram_mb,
            item.manifest.idle_timeout_secs
        );
    }

    Ok(())
}

pub async fn install_extension(event_bus: EventBus, catalog_id: &str) -> Result<()> {
    let supervisor = ExtensionSupervisor::new(event_bus);
    let installed = supervisor
        .install_from_catalog(catalog_id)
        .await
        .with_context(|| format!("Failed to install catalog extension '{catalog_id}'"))?;

    println!("✅ Installed extension '{}' (id: {})", installed.name, installed.id);
    println!("   Category: {} | Est. RAM: {} MB | Idle Auto-Unload: {}s",
        installed.category.display_label(),
        installed.estimated_ram_mb,
        installed.idle_timeout_secs
    );
    if !installed.install_hint.is_empty() {
        println!("   Setup Hint: {}", installed.install_hint);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn add_custom_mcp(
    event_bus: EventBus,
    id: String,
    name: String,
    command: Option<String>,
    args: Option<String>,
    sse_url: Option<String>,
    category: Option<String>,
    description: Option<String>,
    estimated_ram_mb: u64,
    idle_timeout_secs: Option<u64>,
) -> Result<()> {
    let kind = if let Some(url) = sse_url {
        ExtensionKind::McpSse { url }
    } else if let Some(cmd) = command {
        let parsed_args: Vec<String> = args
            .map(|a| a.split_whitespace().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        ExtensionKind::McpStdio {
            command: cmd,
            args: parsed_args,
            env: HashMap::new(),
        }
    } else {
        anyhow::bail!("Provide either --command <cmd> (for MCP stdio) or --url <sse_url> (for MCP SSE)");
    };

    let cat = category
        .as_deref()
        .map(ExtensionCategory::from_str_loose)
        .unwrap_or(ExtensionCategory::Custom);

    let manifest = ExtensionManifest {
        id: id.clone(),
        name: name.clone(),
        category: cat,
        description: description
            .unwrap_or_else(|| format!("Custom MCP extension '{name}'")),
        kind,
        estimated_ram_mb,
        idle_timeout_secs: idle_timeout_secs.unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS),
        install_hint: "Custom user-configured MCP server.".to_string(),
        enabled: true,
        tags: vec![id.clone(), cat.as_str().to_string(), "mcp".to_string()],
    };

    let supervisor = ExtensionSupervisor::new(event_bus);
    let saved = supervisor
        .add_custom_extension(manifest)
        .await
        .context("Failed to register custom MCP extension")?;

    println!("✅ Registered custom MCP extension '{}' (id: {})", saved.name, saved.id);
    Ok(())
}

pub async fn set_extension_enabled(
    event_bus: EventBus,
    id: &str,
    enabled: bool,
) -> Result<()> {
    let supervisor = ExtensionSupervisor::new(event_bus);
    let updated = supervisor
        .set_enabled(id, enabled)
        .await
        .with_context(|| format!("Failed to update enabled state for '{id}'"))?;

    let state_word = if enabled { "enabled" } else { "disabled" };
    println!("✅ Extension '{}' ({}) is now {}.", updated.name, updated.id, state_word);
    Ok(())
}

pub async fn start_extension(event_bus: EventBus, id: &str) -> Result<()> {
    let supervisor = ExtensionSupervisor::new(event_bus);
    println!("🚀 Starting extension '{id}'...");
    let info = supervisor
        .start_extension(id)
        .await
        .with_context(|| format!("Failed to start extension '{id}'"))?;

    println!(
        "✅ Extension '{}' is {} (PID: {:?}, Est. RAM: {} MB)",
        info.manifest.name, info.status, info.pid, info.manifest.estimated_ram_mb
    );
    Ok(())
}

pub async fn stop_extension(event_bus: EventBus, id: &str) -> Result<()> {
    let supervisor = ExtensionSupervisor::new(event_bus);
    let info = supervisor
        .stop_extension(id)
        .await
        .with_context(|| format!("Failed to stop extension '{id}'"))?;

    println!("🛑 Extension '{}' is now {}.", info.manifest.name, info.status);
    Ok(())
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() > max_len {
        let head: String = s.chars().take(max_len.saturating_sub(1)).collect();
        format!("{head}…")
    } else {
        s.to_string()
    }
}
