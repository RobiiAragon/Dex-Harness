use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tracing::instrument;

use harness_core::{Tool, ToolError};

/// Input arguments for surgically editing a file.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EditFileArgs {
    /// Absolute or relative path to the file to edit.
    pub path: String,
    /// The exact contiguous block of text to be replaced. Must match existing file content exactly.
    pub old_text: String,
    /// The replacement content to insert in place of `old_text`.
    pub new_text: String,
}

#[derive(Default, Clone)]
pub struct EditFileTool {
    workspace_root: Option<PathBuf>,
}

impl EditFileTool {
    pub fn new() -> Self {
        Self { workspace_root: None }
    }

    pub fn with_workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    fn resolve_path(&self, raw: &str) -> PathBuf {
        let p = Path::new(raw);
        if p.is_absolute() {
            p.to_path_buf()
        } else if let Some(ref root) = self.workspace_root {
            root.join(p)
        } else {
            p.to_path_buf()
        }
    }

    /// Computes a lightweight unified diff representation between old and new text blocks.
    fn format_diff(path: &str, old_block: &str, new_block: &str) -> String {
        let mut diff = format!("--- a/{path}\n+++ b/{path}\n");
        for line in old_block.lines() {
            diff.push_str(&format!("- {line}\n"));
        }
        for line in new_block.lines() {
            diff.push_str(&format!("+ {line}\n"));
        }
        diff
    }
}

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Surgically replaces an exact contiguous text block in a file with new content. Ensures the target block is unique to prevent accidental edits. Always inspect the file with read_file before editing."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(EditFileArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Target file path" },
                "old_text": { "type": "string", "description": "Exact text block to replace" },
                "new_text": { "type": "string", "description": "New replacement content" }
            },
            "required": ["path", "old_text", "new_text"]
        }))
    }

    #[instrument(skip(self, arguments))]
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: EditFileArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        let file_path = self.resolve_path(&args.path);
        let content = tokio::fs::read_to_string(&file_path).await.map_err(|e| {
            ToolError::Io {
                name: self.name().to_string(),
                message: format!("Could not read file '{}': {e}", args.path),
            }
        })?;

        let match_count = content.matches(&args.old_text).count();

        if match_count == 0 {
            return Err(ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: format!(
                    "Target text block not found in '{}'. Ensure whitespace and indentation match exactly.\nTarget searched ({} bytes):\n{}",
                    args.path,
                    args.old_text.len(),
                    args.old_text
                ),
            });
        }

        if match_count > 1 {
            return Err(ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: format!(
                    "Target text block matches {} times in '{}'. Provide more surrounding lines to create a unique match.",
                    match_count, args.path
                ),
            });
        }

        // Perform exact single replacement
        let updated_content = content.replacen(&args.old_text, &args.new_text, 1);

        tokio::fs::write(&file_path, &updated_content).await.map_err(|e| {
            ToolError::Io {
                name: self.name().to_string(),
                message: format!("Could not write updated content to '{}': {e}", args.path),
            }
        })?;

        let diff = Self::format_diff(&args.path, &args.old_text, &args.new_text);
        Ok(format!(
            "Successfully edited '{}' (replaced 1 occurrence):\n{}",
            args.path, diff
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_surgical_edit_file_success() {
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("dex_test_edit.txt");
        let path_str = file_path.to_string_lossy().to_string();

        let initial_text = "fn main() {\n    println!(\"Hello old\");\n}\n";
        tokio::fs::write(&file_path, initial_text).await.unwrap();

        let tool = EditFileTool::new();
        let args = serde_json::json!({
            "path": path_str,
            "old_text": "    println!(\"Hello old\");",
            "new_text": "    println!(\"Hello new\");"
        });

        let result = tool.execute(args).await.expect("Edit should succeed");
        assert!(result.contains("Successfully edited"));
        assert!(result.contains("-     println!(\"Hello old\");"));
        assert!(result.contains("+     println!(\"Hello new\");"));

        let updated = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert!(updated.contains("Hello new"));
        assert!(!updated.contains("Hello old"));

        let _ = tokio::fs::remove_file(file_path).await;
    }

    #[tokio::test]
    async fn test_surgical_edit_file_ambiguous_fails() {
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("dex_test_ambiguous.txt");
        let path_str = file_path.to_string_lossy().to_string();

        let initial_text = "foo\nbar\nfoo\n";
        tokio::fs::write(&file_path, initial_text).await.unwrap();

        let tool = EditFileTool::new();
        let args = serde_json::json!({
            "path": path_str,
            "old_text": "foo",
            "new_text": "baz"
        });

        let result = tool.execute(args).await;
        assert!(result.is_err());
        let err = result.err().unwrap().to_string();
        assert!(err.contains("matches 2 times"));

        let _ = tokio::fs::remove_file(file_path).await;
    }
}
