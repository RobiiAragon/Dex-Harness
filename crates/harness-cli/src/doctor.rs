//! `dex doctor`: one screen that says what works, what doesn't, and the exact fix.

use std::sync::Arc;

use anyhow::Result;
use harness_core::{find_executable_in_enriched_path, home_dex_dir, EventBus, HardwareProfile, ToolRegistry};
use harness_extensions::{connect_enabled_mcp_extensions, ExtensionSupervisor};
use harness_models::{detect_local_servers, is_chat_model_id};

use crate::endpoint::{resolve, CliConfig, EndpointRequest};
use crate::ui::{truncate_chars, Style};

fn row(style: Style, ok: bool, name: &str, detail: &str) {
    let mark = if ok { style.green("✓") } else { style.red("✗") };
    println!("  {mark} {:<22} {}", name, style.dim(detail));
}

pub async fn run(style: Style) -> Result<()> {
    let hw = HardwareProfile::detect();
    println!("{}", style.bold("System"));
    row(
        style,
        true,
        &hw.cpu_brand,
        &format!(
            "{} · {:.0} GB RAM ({:.0} GB free) · {} {}",
            hw.arch, hw.total_memory_gb, hw.available_memory_gb, hw.os_name, hw.os_version
        ),
    );

    println!("\n{}", style.bold("Model servers"));
    let servers = detect_local_servers().await;
    for srv in &servers {
        let chat: Vec<&String> = srv.loaded_models.iter().filter(|m| is_chat_model_id(m)).collect();
        let detail = if srv.reachable {
            format!("{} · {} chat model(s)", srv.base_url, chat.len())
        } else {
            format!("not running ({})", srv.base_url)
        };
        row(style, srv.reachable, &srv.name, &detail);
    }

    let cfg = CliConfig::load().await;
    let req = EndpointRequest {
        provider: cfg.provider.clone(),
        model: cfg.model.clone(),
        api_base: cfg.api_base.clone(),
    };
    println!("\n{}", style.bold("Default chat endpoint"));
    match resolve(&req, &|_| {}, None).await {
        Ok(ep) => row(style, true, &truncate_chars(&ep.model, 22), &ep.label),
        Err(e) => {
            row(style, false, "none", "");
            for line in e.to_string().lines() {
                println!("      {}", style.dim(line));
            }
        }
    }
    println!("    {}", style.dim(&format!("preferences: {}", CliConfig::path().display())));

    println!("\n{}", style.bold("Creative runtimes"));
    let mlx_python = home_dex_dir().join("runtimes/mlx-venv/bin/python");
    row(
        style,
        mlx_python.exists(),
        "MLX venv",
        &if mlx_python.exists() {
            mlx_python.display().to_string()
        } else {
            "not installed — install from Dex.app › Models Hub".to_string()
        },
    );
    for (bin, purpose, hint) in [
        ("ffmpeg", "video & audio", "brew install ffmpeg"),
        ("blender", "3D convert/render", "brew install --cask blender"),
        ("uvx", "Python MCP servers", "brew install uv"),
        ("npx", "Node MCP servers", "brew install node"),
    ] {
        match find_executable_in_enriched_path(bin) {
            Some(p) => row(style, true, bin, &format!("{purpose} · {}", p.display())),
            None => row(style, false, bin, &format!("{purpose} · fix: {hint}")),
        }
    }

    println!("\n{}", style.bold("Extensions (MCP)"));
    let supervisor = Arc::new(ExtensionSupervisor::new(EventBus::default()));
    let mut scratch = ToolRegistry::new();
    let report = connect_enabled_mcp_extensions(supervisor.clone(), &mut scratch).await;
    if report.registered_tools.is_empty() && report.failed.is_empty() {
        println!("  {}", style.dim("No MCP extensions enabled (`dex ext catalog` to browse)."));
    } else {
        row(
            style,
            !report.registered_tools.is_empty(),
            "connected",
            &format!("{} tool(s) available", report.registered_tools.len()),
        );
        for (id, err) in &report.failed {
            row(style, false, id, &truncate_chars(err, 90));
        }
        if !report.failed.is_empty() {
            println!(
                "    {}",
                style.dim("Disable broken ones with `dex ext disable <id>` to keep startup fast.")
            );
        }
    }
    drop(scratch);
    drop(supervisor);
    Ok(())
}
