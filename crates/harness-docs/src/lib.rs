pub mod error;
pub mod parser;
pub mod tools;
pub mod vision_ocr;

pub use error::DocsError;
pub use parser::{
    parse_document, parse_document_with_tmp_dir, DocumentParseResult, DEFAULT_DOCS_TMP_DIR,
    DEFAULT_MAX_INLINE_CHARS,
};
pub use tools::{register_docs_tools, DocParseArgs, DocParseTool, OcrImageArgs, OcrImageTool};
pub use vision_ocr::{run_ocr_on_image, OcrBlock, OcrResult};
