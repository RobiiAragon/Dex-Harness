use std::path::Path;
use std::sync::Arc;
use async_trait::async_trait;
use harness_core::{EventBus, Tool, ToolError, ToolRegistry};
use harness_extensions::ExtensionSupervisor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::parser::{parse_document, DEFAULT_MAX_INLINE_CHARS};
use crate::vision_ocr::run_ocr_on_image;

/// Arguments for `ocr_image`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OcrImageArgs {
    /// Path to an image or screenshot (.png, .jpg, .webp, .heic).
    pub path: String,
}

/// Extracts structured text from an image using Apple Vision Framework (macOS) or Tesseract.
pub struct OcrImageTool {
    event_bus: Option<EventBus>,
}

impl OcrImageTool {
    pub fn new(event_bus: Option<EventBus>) -> Self {
        Self { event_bus }
    }
}

#[async_trait]
impl Tool for OcrImageTool {
    fn name(&self) -> &str {
        "ocr_image"
    }

    fn description(&self) -> &str {
        "Runs offline multilingual OCR (Apple Vision on macOS / Tesseract fallback) to extract text from an image or screenshot (.png, .jpg, .webp)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(OcrImageArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: OcrImageArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let res = run_ocr_on_image(Path::new(&args.path), self.event_bus.as_ref())
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: e.to_string(),
            })?;

        if res.text.is_empty() {
            Ok(format!(
                "OCR completed via {} on '{}', but no text was detected.",
                res.engine, args.path
            ))
        } else {
            Ok(format!(
                "OCR Result (engine: {}, blocks: {}):\n\n{}",
                res.engine,
                res.blocks.len(),
                res.text
            ))
        }
    }
}

/// Arguments for `doc_parse`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DocParseArgs {
    /// Path to a PDF, DOCX, PPTX, image, or text document.
    pub path: String,
    /// Optional inline character limit before spilling full Markdown to `.dex/tmp/docs/`.
    #[serde(default)]
    pub max_chars: Option<usize>,
}

/// Extracts PDF, Office, or image documents into clean Markdown (via Docling sidecar or pure-Rust `lopdf` / Vision OCR fallback).
pub struct DocParseTool {
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<EventBus>,
}

impl DocParseTool {
    pub fn new(supervisor: Option<Arc<ExtensionSupervisor>>, event_bus: Option<EventBus>) -> Self {
        Self {
            supervisor,
            event_bus,
        }
    }
}

#[async_trait]
impl Tool for DocParseTool {
    fn name(&self) -> &str {
        "doc_parse"
    }

    fn description(&self) -> &str {
        "Converts PDF, DOCX, PPTX, or image documents into clean Markdown (using Docling if running, or native pure-Rust PDF / Apple Vision OCR fallback). Spills very large documents to `.dex/tmp/docs/`."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(DocParseArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: DocParseArgs =
            serde_json::from_value(arguments).map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: e.to_string(),
            })?;

        let limit = args.max_chars.unwrap_or(DEFAULT_MAX_INLINE_CHARS);
        let res = parse_document(
            Path::new(&args.path),
            limit,
            self.supervisor.clone(),
            self.event_bus.as_ref(),
        )
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_string(),
            message: e.to_string(),
        })?;

        let pages_note = res
            .page_count
            .map(|p| format!(", pages: {p}"))
            .unwrap_or_default();
        Ok(format!(
            "Document parsed (engine: {}{pages_note}):\n\n{}",
            res.engine, res.markdown
        ))
    }
}

/// Registers `ocr_image` and `doc_parse` as `ToolVisibility::Deferred` under category `"OcrDocs"`.
pub fn register_docs_tools(
    registry: &mut ToolRegistry,
    supervisor: Option<Arc<ExtensionSupervisor>>,
    event_bus: Option<EventBus>,
) {
    registry.register_deferred(
        Arc::new(OcrImageTool::new(event_bus.clone())),
        "OcrDocs",
        vec![
            "ocr".to_string(),
            "vision".to_string(),
            "image".to_string(),
            "text".to_string(),
            "screenshot".to_string(),
        ],
    );

    registry.register_deferred(
        Arc::new(DocParseTool::new(supervisor, event_bus)),
        "OcrDocs",
        vec![
            "pdf".to_string(),
            "docs".to_string(),
            "docling".to_string(),
            "markdown".to_string(),
            "parse".to_string(),
            "ocr".to_string(),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_docs_tools_registered_as_deferred_under_ocr_docs_category() {
        let mut registry = ToolRegistry::new();
        register_docs_tools(&mut registry, None, None);

        assert!(registry.active_definitions().is_empty());
        assert_eq!(registry.deferred_count(), 2);

        let hits = registry.search_deferred("pdf", Some("OcrDocs"));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "doc_parse");
    }
}
