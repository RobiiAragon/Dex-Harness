use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use harness_core::{DexEvent, EventBus};
use harness_extensions::ExtensionSupervisor;
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::error::DocsError;
use crate::vision_ocr::run_ocr_on_image;

pub const DEFAULT_DOCS_TMP_DIR: &str = ".dex/tmp/docs";
pub const DEFAULT_MAX_INLINE_CHARS: usize = 12_000;
const DOCLING_TIMEOUT_SECS: u64 = 120;

/// Result of parsing a document (PDF, DOCX, PPTX, image, or text) into clean Markdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentParseResult {
    pub markdown: String,
    pub source_path: String,
    pub engine: String,
    pub page_count: Option<usize>,
    pub saved_full_path: Option<PathBuf>,
}

/// Parses PDFs, Office documents, images, or text files into clean Markdown.
/// Prefers `docling-serve` if running (waking `docling-mcp` via `ExtensionSupervisor` if enabled),
/// falls back to Apple Vision OCR for images and pure-Rust `lopdf` extraction for standard PDFs.
pub async fn parse_document(
    path: &Path,
    max_inline_chars: usize,
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<&EventBus>,
) -> Result<DocumentParseResult, DocsError> {
    parse_document_with_tmp_dir(
        path,
        max_inline_chars,
        Path::new(DEFAULT_DOCS_TMP_DIR),
        supervisor,
        event_bus,
    )
    .await
}

pub async fn parse_document_with_tmp_dir(
    path: &Path,
    max_inline_chars: usize,
    tmp_dir: &Path,
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<&EventBus>,
) -> Result<DocumentParseResult, DocsError> {
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        return Err(DocsError::InvalidInput {
            reason: format!("Document file does not exist: '{}'", path.display()),
        });
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let file_label = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document");

    // 1. Direct image OCR path
    if matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "heic" | "tiff" | "tif" | "bmp"
    ) {
        let ocr = run_ocr_on_image(path, event_bus).await?;
        let raw_md = format!("# OCR Extraction: {file_label}\n\n{}\n", ocr.text);
        return finalize_parse_output(
            raw_md,
            path,
            ocr.engine,
            Some(1),
            max_inline_chars,
            tmp_dir,
            None, // run_ocr_on_image already published MediaGenerated("ocr")
        )
        .await;
    }

    // 2. Plain text / Markdown / structured text files
    if matches!(
        ext.as_str(),
        "txt" | "md" | "markdown" | "csv" | "json" | "yaml" | "yml" | "toml"
    ) {
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| DocsError::Io {
                path: path.to_string_lossy().to_string(),
                source: e,
            })?;
        return finalize_parse_output(
            content,
            path,
            "plain-text".to_string(),
            None,
            max_inline_chars,
            tmp_dir,
            event_bus,
        )
        .await;
    }

    // 3. Complex documents (PDF, DOCX, PPTX, HTML): try Docling sidecar first
    if let Some(ref sup) = supervisor {
        let reg = sup.load_registry().await;
        if reg.get("docling-mcp").is_some_and(|m| m.enabled) {
            let _ = sup.start_extension("docling-mcp").await;
            sup.touch_last_used("docling-mcp").await;
        }
    }

    if let Some(endpoint) = resolve_docling_endpoint().await {
        match try_docling_convert(&endpoint, path).await {
            Ok((md, pages)) => {
                info!(path = ?path, endpoint = %endpoint, "Document parsed via Docling");
                return finalize_parse_output(
                    md,
                    path,
                    "docling".to_string(),
                    pages,
                    max_inline_chars,
                    tmp_dir,
                    event_bus,
                )
                .await;
            }
            Err(e) => {
                warn!(endpoint = %endpoint, error = %e, "Docling HTTP conversion failed; checking pure-Rust fallback");
            }
        }
    }

    // 4. Pure-Rust PDF fallback via `lopdf` (offloaded to spawn_blocking)
    if ext == "pdf" {
        let (md, page_count) = extract_pdf_with_lopdf(path).await?;
        return finalize_parse_output(
            md,
            path,
            "lopdf-rust".to_string(),
            Some(page_count),
            max_inline_chars,
            tmp_dir,
            event_bus,
        )
        .await;
    }

    Err(DocsError::UnsupportedFormat {
        path: path.to_string_lossy().to_string(),
        reason: format!(
            "Format '.{ext}' requires Docling sidecar (`docling-serve` / `DEX_DOCLING_URL`). Supported natively without sidecars: .pdf, .png, .jpg, .webp, .md, .txt."
        ),
    })
}

async fn finalize_parse_output(
    raw_markdown: String,
    source_path: &Path,
    engine: String,
    page_count: Option<usize>,
    max_inline_chars: usize,
    tmp_dir: &Path,
    event_bus: Option<&EventBus>,
) -> Result<DocumentParseResult, DocsError> {
    let total_chars = raw_markdown.chars().count();
    let (markdown, saved_full_path) = if max_inline_chars > 0 && total_chars > max_inline_chars {
        tokio::fs::create_dir_all(tmp_dir)
            .await
            .map_err(|e| DocsError::Io {
                path: tmp_dir.to_string_lossy().to_string(),
                source: e,
            })?;

        let stem = source_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("doc");
        let safe_stem: String = stem
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .take(40)
            .collect();
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let full_path = tmp_dir.join(format!("{safe_stem}_{ts}.md"));

        tokio::fs::write(&full_path, &raw_markdown)
            .await
            .map_err(|e| DocsError::Io {
                path: full_path.to_string_lossy().to_string(),
                source: e,
            })?;

        let head_len = (max_inline_chars * 2) / 3;
        let tail_len = max_inline_chars / 4;
        let head: String = raw_markdown.chars().take(head_len).collect();
        let tail: String = raw_markdown
            .chars()
            .skip(total_chars.saturating_sub(tail_len))
            .collect();

        let summary = format!(
            "{head}\n\n... [DOCUMENT TRUNCATED: {total_chars} chars exceeded inline limit ({max_inline_chars}). Full Markdown saved to '{}'. Use `read_file` to inspect specific sections.] ...\n\n{tail}",
            full_path.display()
        );
        (summary, Some(full_path))
    } else {
        (raw_markdown, None)
    };

    if let Some(bus) = event_bus {
        let preview: String = markdown.chars().take(120).collect();
        bus.publish(DexEvent::MediaGenerated {
            kind: "ocr".to_string(),
            path: saved_full_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| source_path.to_string_lossy().to_string()),
            prompt: preview,
            duration_secs: None,
        });
    }

    Ok(DocumentParseResult {
        markdown,
        source_path: source_path.to_string_lossy().to_string(),
        engine,
        page_count,
        saved_full_path,
    })
}

async fn resolve_docling_endpoint() -> Option<String> {
    if let Ok(url) = std::env::var("DEX_DOCLING_URL") {
        let trimmed = url.trim().trim_end_matches('/').to_string();
        if !trimmed.is_empty() {
            return Some(if trimmed.contains("/convert/") {
                trimmed
            } else {
                format!("{trimmed}/v1alpha/convert/file")
            });
        }
    }

    let probe = reqwest::Client::builder()
        .timeout(Duration::from_millis(250))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    if probe.get("http://127.0.0.1:5001/health").send().await.is_ok() {
        return Some("http://127.0.0.1:5001/v1alpha/convert/file".to_string());
    }
    None
}

async fn try_docling_convert(
    endpoint: &str,
    path: &Path,
) -> Result<(String, Option<usize>), DocsError> {
    let bytes = tokio::fs::read(path).await.map_err(|e| DocsError::Io {
        path: path.to_string_lossy().to_string(),
        source: e,
    })?;

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document.pdf")
        .to_string();

    let form = Form::new()
        .part("files", Part::bytes(bytes).file_name(file_name))
        .text("to_formats", "md");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(DOCLING_TIMEOUT_SECS))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    debug!(endpoint = %endpoint, path = ?path, "Submitting document to Docling");
    let resp = tokio::time::timeout(
        Duration::from_secs(DOCLING_TIMEOUT_SECS),
        client.post(endpoint).multipart(form).send(),
    )
    .await
    .map_err(|_| DocsError::Timeout {
        operation: "docling conversion".to_string(),
        timeout_secs: DOCLING_TIMEOUT_SECS,
    })?
    .map_err(|e| DocsError::Http {
        url: endpoint.to_string(),
        source: e,
    })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(DocsError::OcrFailed {
            path: path.to_string_lossy().to_string(),
            reason: format!("Docling HTTP {status}: {body}"),
        });
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| DocsError::Http {
        url: endpoint.to_string(),
        source: e,
    })?;

    // Docling Serve returns `{ "document": { "md_content": "..." } }` or `{ "md_content": "..." }`
    let md = json
        .pointer("/document/md_content")
        .and_then(|v| v.as_str())
        .or_else(|| json.get("md_content").and_then(|v| v.as_str()))
        .or_else(|| json.get("markdown").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
        .ok_or_else(|| DocsError::OcrFailed {
            path: path.to_string_lossy().to_string(),
            reason: "Docling response did not contain `md_content` field".to_string(),
        })?;

    Ok((md, None))
}

async fn extract_pdf_with_lopdf(path: &Path) -> Result<(String, usize), DocsError> {
    let owned_path = path.to_path_buf();
    let display_path = path.to_string_lossy().to_string();

    tokio::task::spawn_blocking(move || {
        let doc = lopdf::Document::load(&owned_path).map_err(|e| DocsError::PdfExtractFailed {
            path: owned_path.to_string_lossy().to_string(),
            reason: e.to_string(),
        })?;

        let pages = doc.get_pages();
        let page_count = pages.len();
        let mut page_nums: Vec<u32> = pages.keys().copied().collect();
        page_nums.sort_unstable();

        let file_name = owned_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("document.pdf");

        let mut sections = Vec::with_capacity(page_count + 1);
        sections.push(format!("# {file_name}\n"));

        let mut extracted_chars = 0usize;
        for page_num in page_nums {
            let page_text = doc.extract_text(&[page_num]).unwrap_or_default();
            let cleaned = page_text.trim();
            if !cleaned.is_empty() {
                extracted_chars += cleaned.len();
                sections.push(format!("## Page {page_num}\n\n{cleaned}\n"));
            }
        }

        if extracted_chars == 0 {
            sections.push(
                "_No embedded text layer detected in this PDF (it may be a scanned document). Enable `docling-mcp` or render pages and run `ocr_image`._\n"
                    .to_string(),
            );
        }

        Ok((sections.join("\n"), page_count))
    })
    .await
    .map_err(|e| DocsError::PdfExtractFailed {
        path: display_path,
        reason: format!("PDF extraction task panicked or was cancelled: {e}"),
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream};

    fn create_minimal_test_pdf(path: &Path, text: &str) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => font_id,
            },
        });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal(text)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            content.encode().expect("encode pdf stream"),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });
        let pages = dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        doc.save(path).expect("save minimal test pdf");
    }

    #[tokio::test]
    async fn test_pure_rust_pdf_parse_and_large_output_spill() {
        let temp_dir =
            std::env::temp_dir().join(format!("dex_docs_pdf_test_{}", std::process::id()));
        tokio::fs::create_dir_all(&temp_dir)
            .await
            .expect("create test dir");

        let pdf_path = temp_dir.join("sample.pdf");
        create_minimal_test_pdf(&pdf_path, "Dex Harness Phase 7 PDF Extraction Test");

        let res = parse_document_with_tmp_dir(
            &pdf_path,
            DEFAULT_MAX_INLINE_CHARS,
            &temp_dir,
            None,
            None,
        )
        .await
        .expect("parse_document should succeed for PDF");

        assert_eq!(res.engine, "lopdf-rust");
        assert_eq!(res.page_count, Some(1));
        assert!(res.markdown.contains("Dex Harness Phase 7 PDF Extraction Test"));
        assert!(res.saved_full_path.is_none());

        // Test large output spill guardrail with a large markdown file
        let big_txt = temp_dir.join("large_doc.txt");
        let big_content = format!("HEAD_DOC_{}_TAIL_DOC", "A".repeat(1500));
        tokio::fs::write(&big_txt, &big_content)
            .await
            .expect("write big txt");

        let big_res =
            parse_document_with_tmp_dir(&big_txt, 400, &temp_dir, None, None)
                .await
                .expect("large doc parse should succeed");

        let saved = big_res
            .saved_full_path
            .expect("large doc must spill to tmp dir");
        assert!(big_res.markdown.contains("DOCUMENT TRUNCATED"));
        assert!(big_res.markdown.contains("HEAD_DOC_"));
        assert!(big_res.markdown.contains("_TAIL_DOC"));
        assert_eq!(
            tokio::fs::read_to_string(&saved).await.expect("read saved"),
            big_content
        );

        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }
}
