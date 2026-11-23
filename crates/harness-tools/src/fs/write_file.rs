use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tracing::instrument;

use harness_core::{Tool, ToolError};

/// Input arguments for writing content to a file.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteFileArgs {
    /// Absolute or relative path to the file to create or overwrite.
    pub path: String,
    /// The complete content to write into the file.
    pub content: String,
    /// Whether to automatically create parent directories if they do not exist. Defaults to true.
    pub create_directories: Option<bool>,
}

#[derive(Default, Clone)]
pub struct WriteFileTool {
    workspace_root: Option<PathBuf>,
}

impl WriteFileTool {
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
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Creates a new file or completely overwrites an existing file with the provided content. Automatically creates parent directories when required."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(WriteFileArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Target file path" },
                "content": { "type": "string", "description": "Content to write" },
                "create_directories": { "type": "boolean", "description": "Create parent directories" }
            },
            "required": ["path", "content"]
        }))
    }

    #[instrument(skip(self, arguments))]
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: WriteFileArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        let file_path = self.resolve_path(&args.path);
        let create_dirs = args.create_directories.unwrap_or(true);

        if create_dirs {
            if let Some(parent) = file_path.parent() {
                if !parent.as_os_str().is_empty() {
                    tokio::fs::create_dir_all(parent).await.map_err(|e| ToolError::Io {
                        name: self.name().to_string(),
                        message: format!("Failed to create parent directory '{}': {e}", parent.display()),
                    })?;
                }
            }
        }

        tokio::fs::write(&file_path, &args.content).await.map_err(|e| ToolError::Io {
            name: self.name().to_string(),
            message: format!("Failed to write to file '{}': {e}", args.path),
        })?;

        Ok(format!(
            "Successfully wrote {} bytes to file '{}'",
            args.content.len(),
            args.path
        ))
    }
}
