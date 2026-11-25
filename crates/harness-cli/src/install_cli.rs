//! Turns "backend not installed" failures into a one-keystroke fix: when an error carries a
//! `[dex-install:<id>]` marker, the CLI explains what is missing, offers to install it, and
//! re-runs the same command — so users never have to read an install guide.

use std::io::{IsTerminal, Write};

use anyhow::{Context, Result};
use harness_core::{parse_install_markers, strip_install_markers, EventBus};
use harness_models::RuntimeKind;

use crate::models_cli::install_runtime_cli;
use crate::ui::Style;

fn display_names(ids: &[String]) -> String {
    ids.iter()
        .map(|id| {
            RuntimeKind::from_str_loose(id)
                .map(|k| k.display_name().to_string())
                .unwrap_or_else(|| id.clone())
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Asks a yes/no question on the terminal (default yes). Returns false without a TTY.
async fn confirm(question: &str) -> bool {
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        return false;
    }
    print!("{question} [Y/n] ");
    let _ = std::io::stdout().flush();
    let answer = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).map(|_| line)
    })
    .await;
    match answer {
        // 0 bytes read = EOF: treat as "no".
        Ok(Ok(line)) if line.is_empty() => false,
        Ok(Ok(line)) => {
            let a = line.trim().to_lowercase();
            a.is_empty() || matches!(a.as_str(), "y" | "yes" | "s" | "si" | "sí")
        }
        _ => false,
    }
}

/// Installs each runtime id in order, printing progress.
pub async fn install_all(ids: &[String]) -> Result<()> {
    for id in ids {
        install_runtime_cli(id, EventBus::default()).await?;
    }
    Ok(())
}

/// Handles a failed command. If the error names missing runtimes, offers to install them and
/// re-runs the same command line; otherwise returns the original error untouched.
pub async fn recover_from_error(err: anyhow::Error) -> Result<()> {
    let full = format!("{err:#}");
    let ids = parse_install_markers(&full);
    if ids.is_empty() {
        return Err(err);
    }
    let style = Style::detect();
    let names = display_names(&ids);
    eprintln!("{} {}", style.red("✗"), strip_install_markers(&full));
    println!("\n{} Dex needs {} for this.", style.accent("⏺"), style.bold(&names));

    if !confirm(&format!("  Install {names} now?")).await {
        let cmds: Vec<String> = ids.iter().map(|id| format!("dex install {id}")).collect();
        println!("  {}", style.dim(&format!("To install it later run: {}", cmds.join(" && "))));
        std::process::exit(1);
    }

    install_all(&ids).await?;
    println!("\n{} {} installed. Running your command again…\n", style.green("✓"), names);

    let exe = std::env::current_exe().context("Could not locate the dex executable to retry")?;
    let status = tokio::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .status()
        .await
        .context("Failed to re-run the command")?;
    std::process::exit(status.code().unwrap_or(1));
}
