use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tracing::instrument;

use harness_core::{Tool, ToolError};

/// Input parameters for reading a file.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadFileArgs {
    /// The path of the file to read (relative or absolute).
    pub path: String,
    /// 1-based start line to begin reading from (inclusive). Defaults to 1.
    pub start_line: Option<usize>,
    /// Maximum number of lines to read. Defaults to 500 lines.
    pub limit: Option<usize>,
}

#[derive(Default, Clone)]
pub struct ReadFileTool {
    workspace_root: Option<PathBuf>,
}

impl ReadFileTool {
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
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Reads the contents of a text file from disk with optional line ranges and line numbers. Use this tool to inspect code, configuration files, or logs."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(ReadFileArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "The path of the file to read" },
                "start_line": { "type": "integer", "description": "1-based start line" },
                "limit": { "type": "integer", "description": "Max lines to return" }
            },
            "required": ["path"]
        }))
    }

    #[instrument(skip(self, arguments))]
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: ReadFileArgs = serde_json::from_value(arguments).map_err(|e| {
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

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();

        let start_line = args.start_line.unwrap_or(1).max(1);
        let limit = args.limit.unwrap_or(500);

        if start_line > total_lines {
            return Ok(format!(
                "[File '{}' has {} lines; requested start line is {}]",
                args.path, total_lines, start_line
            ));
        }

        let slice_start = start_line - 1;
        let slice_end = (slice_start + limit).min(total_lines);

        let mut output = String::with_capacity(content.len());
        output.push_str(&format!(
            "--- File: {} (lines {}-{} of {}) ---\n",
            args.path,
            start_line,
            slice_end,
            total_lines
        ));

        for (idx, line) in lines[slice_start..slice_end].iter().enumerate() {
            let line_num = slice_start + idx + 1;
            output.push_str(&format!("{line_num:4} | {line}\n"));
        }

        if slice_end < total_lines {
            output.push_str(&format!(
                "--- [Truncated: {} lines remaining] ---\n",
                total_lines - slice_end
            ));
        }

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_read_file_tool() {
        let tool = ReadFileTool::new();
        let args = serde_json::json!({
            "path": "Cargo.toml",
            "start_line": 1,
            "limit": 5
        });
        let result = tool.execute(args).await.expect("File read should succeed");
        assert!(result.contains("--- File: Cargo.toml"));
        assert!(result.contains("   1 | [package]"));
    }
}
