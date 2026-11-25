use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use anyhow::{bail, Context, Result};
use harness_core::cookbook::HardwareProfile;
use harness_core::{DexEvent, EventBus};
use harness_models::{
    detect_local_servers, detect_runtimes, install_runtime, RuntimeKind, import_local_model, scan_model_dirs, HfClient, MemoryFit,
    ModelDirsConfig, ModelDownloader, ModelMetadata, ModelRegistry, ModelSource, OllamaBridge,
};
use tokio_util::sync::CancellationToken;

pub async fn search_models(query: &str, limit: usize) -> Result<()> {
    println!("🔍 Searching Hugging Face for GGUF models matching '{query}'...");
    let client = HfClient::new(None);
    let profile = HardwareProfile::detect();

    let results = client
        .search_models(query, limit)
        .await
        .context("Failed to search Hugging Face Hub")?;

    if results.is_empty() {
        println!("No GGUF models found for '{query}'.");
        return Ok(());
    }

    println!("\n{:<50} {:<12} {:<8}", "REPOSITORY ID", "DOWNLOADS", "LIKES");
    println!("{:-<50} {:-<12} {:-<8}", "", "", "");

    for repo in &results {
        println!(
            "{:<50} {:<12} {:<8}",
            truncate_str(&repo.id, 48),
            repo.downloads.unwrap_or(0),
            repo.likes.unwrap_or(0)
        );
    }

    // Inspect files for the top match
    if let Some(top) = results.first() {
        println!("\n📦 Inspecting available quantizations for top match: {}", top.id);
        println!(
            "Host RAM: {:.1} GB available ({:.1} GB total, Apple Silicon: {})\n",
            profile.available_memory_gb, profile.total_memory_gb, profile.is_apple_silicon
        );

        match client.inspect_gguf_files(&top.id, &profile).await {
            Ok(files) => {
                if files.is_empty() {
                    println!("   No .gguf files found in this repository.");
                } else {
                    println!(
                        "   {:<35} {:<10} {:<10} {:<12}",
                        "FILENAME", "QUANT", "SIZE", "RAM FIT"
                    );
                    println!(
                        "   {:-<35} {:-<10} {:-<10} {:-<12}",
                        "", "", "", ""
                    );
                    for f in &files {
                        let size_gb = f.size_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                        let fit_badge = match f.memory_fit {
                            MemoryFit::Fits => "🟢 Fits",
                            MemoryFit::Tight => "🟡 Tight",
                            MemoryFit::TooLarge => "🔴 Too Large",
                        };
                        println!(
                            "   {:<35} {:<10} {:<10} {:<12}",
                            truncate_str(&f.filename, 33),
                            f.quantization,
                            format!("{:.1} GB", size_gb),
                            fit_badge
                        );
                    }
                    println!("\n💡 To download a quantization, run:");
                    println!("   dex models pull {} --quant Q4_K_M", top.id);
                }
            }
            Err(e) => {
                println!("   (Could not inspect files: {e})");
            }
        }
    }

    Ok(())
}

pub async fn pull_model(source: &str, preferred_quant: &str) -> Result<()> {
    let cancel = CancellationToken::new();

    if source.starts_with("ollama:") {
        let model_name = source.trim_start_matches("ollama:");
        println!("🚀 Pulling model '{model_name}' via Ollama library...");
        let bridge = OllamaBridge::default();

        bridge
            .pull_model(model_name, None, cancel)
            .await
            .context("Failed to pull model from Ollama")?;

        // Register in Dex registry
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let mut registry = ModelRegistry::load_or_default().await;
        let _ = registry
            .add(ModelMetadata {
                id: format!("ollama:{}", model_name),
                name: model_name.to_string(),
                source: ModelSource::Ollama,
                file_path: None,
                repo_id: None,
                filename: None,
                quantization: None,
                format: Some("ollama".to_string()),
                size_bytes: 0,
                created_at: now,
            })
            .await;

        println!("✅ Successfully pulled and registered '{model_name}' in Ollama.");
        return Ok(());
    }

    // Hugging Face repository download
    println!("🤗 Connecting to Hugging Face repository '{source}'...");
    let client = HfClient::new(None);
    let profile = HardwareProfile::detect();

    let files = client
        .inspect_gguf_files(source, &profile)
        .await
        .with_context(|| format!("Failed to inspect GGUF files in Hugging Face repository '{source}'"))?;

    if files.is_empty() {
        bail!("No .gguf files found in repository '{source}'");
    }

    // Find requested or preferred quantization
    let selected = files
        .iter()
        .find(|f| f.quantization.eq_ignore_ascii_case(preferred_quant) || f.filename.to_uppercase().contains(&preferred_quant.to_uppercase()))
        .or_else(|| files.iter().find(|f| f.quantization == "Q4_K_M"))
        .or_else(|| files.first())
        .ok_or_else(|| anyhow::anyhow!("No suitable GGUF file found in repository"))?;

    let size_gb = selected.size_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    println!("🎯 Selected file: {}", selected.filename);
    println!("   Quantization: {} | Size: {:.2} GB | Compatibility: {}", selected.quantization, size_gb, selected.memory_fit);

    let models_dir = ModelRegistry::default_dir();
    let dest_path = models_dir.join(&selected.filename);

    println!("📥 Downloading to {:?}...", dest_path);
    let downloader = ModelDownloader::new(None);

    let downloaded_path = downloader
        .download(
            &selected.download_url,
            &dest_path,
            source,
            &selected.filename,
            None,
            cancel,
        )
        .await
        .context("Download failed")?;

    // Register in local Dex registry
    let model_id = format!("hf:{}", selected.filename.trim_end_matches(".gguf"));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let metadata = ModelMetadata {
        id: model_id.clone(),
        name: selected.filename.clone(),
        source: ModelSource::HuggingFace,
        file_path: Some(downloaded_path.to_string_lossy().to_string()),
        repo_id: Some(source.to_string()),
        filename: Some(selected.filename.clone()),
        quantization: Some(selected.quantization.clone()),
        format: Some(selected.format.to_lowercase()),
        size_bytes: selected.size_bytes,
        created_at: now,
    };

    let mut registry = ModelRegistry::load_or_default().await;
    registry
        .add(metadata)
        .await
        .context("Failed to register model in local registry")?;

    println!("\n✅ Model downloaded and registered successfully!");
    println!("   Model ID: {model_id}");
    println!("   Path: {:?}", downloaded_path);

    Ok(())
}

pub async fn import_model(path: &str, custom_name: Option<&str>, to_ollama: bool) -> Result<()> {
    println!("Importing local model from '{path}'...");
    let p = Path::new(path);
    let bridge = if to_ollama {
        Some(OllamaBridge::default())
    } else {
        None
    };

    let metadata = import_local_model(p, custom_name, true, to_ollama, bridge.as_ref())
        .await
        .context("Failed to import local model (.gguf file or MLX folder)")?;

    println!("Model imported successfully:");
    println!("   ID: {}", metadata.id);
    println!("   Name: {}", metadata.name);
    println!("   Format: {}", metadata.format.as_deref().unwrap_or("gguf").to_uppercase());
    println!("   Quantization: {}", metadata.quantization.as_deref().unwrap_or("Unknown"));
    println!("   Path: {}", metadata.file_path.as_deref().unwrap_or(""));
    if to_ollama {
        println!("   Ollama Status: Registered in local Ollama daemon");
    }

    Ok(())
}

pub async fn list_model_dirs() -> Result<()> {
    let cfg = ModelDirsConfig::load_or_default().await;
    println!("Configured Model Directories ({}):", cfg.config_path.display());
    for dir in &cfg.model_dirs {
        let exists = tokio::fs::try_exists(ModelDirsConfig::expand_tilde(dir))
            .await
            .unwrap_or(false);
        let status = if exists { "active" } else { "missing" };
        println!("   - [{status}] {dir}");
    }
    Ok(())
}

pub async fn add_model_dir(path: &str) -> Result<()> {
    let mut cfg = ModelDirsConfig::load_or_default().await;
    let added = cfg
        .add_dir(path)
        .await
        .with_context(|| format!("Failed to add model folder '{path}'"))?;
    println!("Added model directory: {added}");
    Ok(())
}

pub async fn remove_model_dir(path: &str) -> Result<()> {
    let mut cfg = ModelDirsConfig::load_or_default().await;
    if cfg.remove_dir(path).await.context("Failed to update model dirs config")? {
        println!("Removed model directory: {path}");
    } else {
        println!("Model directory '{path}' was not in configuration.");
    }
    Ok(())
}

pub async fn list_models() -> Result<()> {
    let dirs_cfg = ModelDirsConfig::load_or_default().await;
    let scanned = scan_model_dirs(&dirs_cfg.resolved_paths())
        .await
        .unwrap_or_default();

    println!("Local Model Folders ({} configured):", dirs_cfg.model_dirs.len());
    for d in &dirs_cfg.model_dirs {
        println!("   - {d}");
    }
    println!();

    println!("Discovered Local Models (GGUF / MLX / PT): {}", scanned.len());
    if scanned.is_empty() {
        println!("   (No local models found in configured folders.)\n");
    } else {
        println!(
            "{:<34} {:<7} {:<10} {:<10} {:<12} {:<20}",
            "NAME", "FORMAT", "QUANT", "SIZE", "INTEGRITY", "CAPS"
        );
        println!(
            "{:-<34} {:-<7} {:-<10} {:-<10} {:-<12} {:-<20}",
            "", "", "", "", "", ""
        );
        for m in &scanned {
            let size_str = format!("{:.2} GB", m.size_bytes as f64 / (1024.0 * 1024.0 * 1024.0));
            let caps = if m.capabilities.is_empty() {
                "-".to_string()
            } else {
                m.capabilities.join(",")
            };
            println!(
                "{:<34} {:<7} {:<10} {:<10} {:<12} {:<20}",
                truncate_str(&m.name, 32),
                m.format.to_string(),
                m.quantization.as_deref().unwrap_or("-"),
                size_str,
                m.integrity_status.to_string(),
                truncate_str(&caps, 18)
            );
            if let Some(ref note) = m.integrity_note {
                println!("   -> WARNING: {note}");
            }
        }
        println!();
    }

    // Local servers status (LM Studio / Ollama / MLX-LM)
    let servers = detect_local_servers().await;
    println!("Local Inference Servers:");
    for srv in &servers {
        let state = if srv.reachable { "ONLINE" } else { "offline" };
        println!(
            "   - {} ({}) [{}] — {} model(s)",
            srv.name,
            srv.base_url,
            state,
            srv.loaded_models.len()
        );
        for loaded in srv.loaded_models.iter().take(6) {
            println!("       * {loaded}");
        }
    }

    Ok(())
}

pub async fn remove_model(model_id: &str) -> Result<()> {
    println!("🗑️  Removing model '{model_id}'...");

    if model_id.starts_with("ollama:") {
        let name = model_id.trim_start_matches("ollama:");
        let bridge = OllamaBridge::default();
        if let Err(e) = bridge.delete_model(name).await {
            println!("   (Warning: Ollama delete reported: {e})");
        }
    }

    let mut registry = ModelRegistry::load_or_default().await;
    match registry.remove(model_id, true).await? {
        Some(removed) => {
            println!("✅ Successfully removed '{}' from registry and disk.", removed.name);
        }
        None => {
            println!("⚠️  Model '{model_id}' not found in registry.");
        }
    }

    Ok(())
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        format!("{}…", &s[..max_len - 1])
    } else {
        s.to_string()
    }
}

pub fn list_runtimes() {
    println!("Dex-managed local runtimes:");
    for rt in detect_runtimes() {
        let mark = if rt.available { "Ready        " } else { "Not installed" };
        println!("  [{mark}] {:<15} {}", rt.id, rt.name);
        match (&rt.binary_path, rt.available) {
            (Some(bin), true) => println!("                    {bin}"),
            _ => println!("                    {}", rt.install_hint),
        }
    }
}

pub async fn install_runtime_cli(runtime: &str, event_bus: EventBus) -> Result<()> {
    let Some(kind) = RuntimeKind::from_str_loose(runtime) else {
        let valid: Vec<&str> = harness_models::ALL_RUNTIMES.iter().map(|k| k.as_id()).collect();
        bail!("Unknown component '{runtime}'. Valid: {}", valid.join(", "));
    };
    println!("Installing {} ...", kind.display_name());
    let mut rx = event_bus.subscribe();
    let mut printer = tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            if let DexEvent::RuntimeProgress { message, done, .. } = ev {
                println!("  {message}");
                if done {
                    break;
                }
            }
        }
    });
    let result = install_runtime(kind, Some(&event_bus)).await;
    // Let the printer flush the final progress line, then stop it.
    if tokio::time::timeout(std::time::Duration::from_millis(300), &mut printer)
        .await
        .is_err()
    {
        printer.abort();
    }
    let status = result.with_context(|| format!("Installing runtime '{}' failed", kind.as_id()))?;
    println!(
        "{} ready: {}",
        status.name,
        status.binary_path.as_deref().unwrap_or(&status.managed_dir)
    );
    Ok(())
}
