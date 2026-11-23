use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use anyhow::{bail, Context, Result};
use harness_core::cookbook::HardwareProfile;
use harness_models::{
    HfClient, MemoryFit, ModelDownloader, ModelMetadata, ModelRegistry, ModelSource, OllamaBridge,
    import_local_gguf,
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
    println!("📥 Importing local GGUF model from '{path}'...");
    let p = Path::new(path);
    let bridge = if to_ollama {
        Some(OllamaBridge::default())
    } else {
        None
    };

    let metadata = import_local_gguf(p, custom_name, true, to_ollama, bridge.as_ref())
        .await
        .context("Failed to import local GGUF model")?;

    println!("✅ Model imported successfully!");
    println!("   ID: {}", metadata.id);
    println!("   Name: {}", metadata.name);
    println!("   Quantization: {}", metadata.quantization.as_deref().unwrap_or("Unknown"));
    println!("   Path: {}", metadata.file_path.as_deref().unwrap_or(""));
    if to_ollama {
        println!("   Ollama Status: Registered in local Ollama daemon");
    }

    Ok(())
}

pub async fn list_models() -> Result<()> {
    println!("📋 Local Dex Model Registry (~/.dex/models/registry.json):");
    let registry = ModelRegistry::load_or_default().await;

    if registry.list().is_empty() {
        println!("   (No models installed in Dex registry yet. Run 'dex models search' or 'dex models pull' to install.)\n");
    } else {
        println!(
            "{:<32} {:<12} {:<10} {:<10} {:<30}",
            "ID", "SOURCE", "QUANT", "SIZE", "FILENAME"
        );
        println!(
            "{:-<32} {:-<12} {:-<10} {:-<10} {:-<30}",
            "", "", "", "", ""
        );
        for m in registry.list() {
            let size_str = if m.size_bytes > 0 {
                format!("{:.1} GB", m.size_bytes as f64 / (1024.0 * 1024.0 * 1024.0))
            } else {
                "-".to_string()
            };
            println!(
                "{:<32} {:<12} {:<10} {:<10} {:<30}",
                truncate_str(&m.id, 30),
                m.source.to_string(),
                m.quantization.as_deref().unwrap_or("-"),
                size_str,
                truncate_str(m.filename.as_deref().unwrap_or(&m.name), 28)
            );
        }
        println!();
    }

    // Inspect Ollama instance
    let bridge = OllamaBridge::default();
    match bridge.list_models().await {
        Ok(tags) => {
            println!("🦙 Ollama Local Models (http://localhost:11434):");
            if tags.is_empty() {
                println!("   (No models found in Ollama)");
            } else {
                println!("{:<32} {:<10} {:<10}", "MODEL NAME", "PARAM SIZE", "QUANT");
                println!("{:-<32} {:-<10} {:-<10}", "", "", "");
                for t in &tags {
                    let param = t.details.as_ref().and_then(|d| d.parameter_size.as_deref()).unwrap_or("-");
                    let quant = t.details.as_ref().and_then(|d| d.quantization_level.as_deref()).unwrap_or("-");
                    println!("{:<32} {:<10} {:<10}", truncate_str(&t.name, 30), param, quant);
                }
            }
        }
        Err(_) => {
            println!("🦙 Ollama: Daemon not reachable on localhost:11434");
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
