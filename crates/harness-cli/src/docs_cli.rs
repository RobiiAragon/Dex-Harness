use std::path::Path;
use std::sync::Arc;
use anyhow::{Context, Result};
use harness_core::EventBus;
use harness_docs::{parse_document, run_ocr_on_image, DEFAULT_MAX_INLINE_CHARS};
use harness_extensions::ExtensionSupervisor;

pub async fn run_ocr(image_path: &str, event_bus: EventBus) -> Result<()> {
    println!("👁️ Running OCR on '{}'...", image_path);
    let result = run_ocr_on_image(Path::new(image_path), Some(&event_bus))
        .await
        .context("Image OCR failed")?;

    println!(
        "✅ OCR complete (engine: {}, blocks: {}):\n",
        result.engine,
        result.blocks.len()
    );
    if result.text.is_empty() {
        println!("(No text detected in image)");
    } else {
        println!("{}", result.text);
    }
    Ok(())
}

pub async fn run_parse(doc_path: &str, event_bus: EventBus) -> Result<()> {
    println!("📄 Parsing document '{}'...", doc_path);
    let supervisor = Arc::new(ExtensionSupervisor::new(event_bus.clone()));
    let result = parse_document(
        Path::new(doc_path),
        DEFAULT_MAX_INLINE_CHARS,
        Some(supervisor),
        Some(&event_bus),
    )
    .await
    .context("Document parsing failed")?;

    let pages = result
        .page_count
        .map(|p| format!(", pages: {p}"))
        .unwrap_or_default();
    println!("✅ Parsed via '{}'{}:\n", result.engine, pages);
    println!("{}", result.markdown);
    if let Some(full) = result.saved_full_path {
        println!("\n💾 Full Markdown saved to: {}", full.display());
    }
    Ok(())
}
