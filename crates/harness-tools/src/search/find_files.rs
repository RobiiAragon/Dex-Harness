use async_trait::async_trait;
use ignore::WalkBuilder;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tracing::instrument;

use harness_core::{Tool, ToolError};

/// Input parameters for discovering project files.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindFilesArgs {
    /// Optional filename or path substring/pattern to match against. If omitted, lists all files.
    pub pattern: Option<String>,
    /// Root directory to search from. Defaults to the active workspace root (or ".").
    pub path: Option<String>,
    /// Maximum number of file paths to return. Defaults to 100.
    pub max_results: Option<usize>,
}

#[derive(Default, Clone)]
pub struct FindFilesTool {
    workspace_root: Option<PathBuf>,
}

impl FindFilesTool {
    pub fn new() -> Self {
        Self { workspace_root: None }
    }

    pub fn with_workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    fn resolve_root(&self, raw: Option<&str>) -> PathBuf {
        match raw {
            Some(s) if !s.trim().is_empty() => {
                let p = Path::new(s);
                if p.is_absolute() {
                    p.to_path_buf()
                } else if let Some(ref ws) = self.workspace_root {
                    ws.join(p)
                } else {
                    p.to_path_buf()
                }
            }
            _ => self.workspace_root.clone().unwrap_or_else(|| PathBuf::from(".")),
        }
    }
}

#[async_trait]
impl Tool for FindFilesTool {
    fn name(&self) -> &str {
        "find_files"
    }

    fn description(&self) -> &str {
        "Finds files in the workspace matching a pattern or substring, respecting .gitignore and hidden file configurations."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(FindFilesArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Filename or path substring" },
                "path": { "type": "string", "description": "Starting directory" },
                "max_results": { "type": "integer", "description": "Maximum files to return" }
            }
        }))
    }

    #[instrument(skip(self, arguments))]
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: FindFilesArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        let root_path = self.resolve_root(args.path.as_deref());
        let root_str = root_path.to_string_lossy().to_string();
        let max_results = args.max_results.unwrap_or(100).max(1);
        let pattern_opt = args.pattern.map(|p| p.to_lowercase());

        let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let mut matches = Vec::new();
            let mut total_found = 0;

            let walker = WalkBuilder::new(&root_path)
                .hidden(false)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .build();

            for entry in walker.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }

                let path_display = path
                    .strip_prefix(&root_path)
                    .or_else(|_| path.strip_prefix("."))
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_string();

                let is_match = match pattern_opt {
                    Some(ref pat) => path_display.to_lowercase().contains(pat),
                    None => true,
                };

                if is_match {
                    total_found += 1;
                    if matches.len() < max_results {
                        matches.push(path_display);
                    }
                }
            }

            if matches.is_empty() {
                return Ok(format!("No files found in '{}' matching the criteria", root_str));
            }

            let mut output = format!("Found {} file(s):\n", total_found);
            output.push_str(&matches.join("\n"));
            if total_found > max_results {
                output.push_str(&format!("\n\n... [Showing first {max_results} files out of {total_found}]"));
            }

            Ok(output)
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            name: "find_files".to_string(),
            message: format!("Blocking file search panicked: {e}"),
        })?
        .map_err(|e| ToolError::ExecutionFailed {
            name: "find_files".to_string(),
            message: e,
        })?;

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_find_files_cargo() {
        let tool = FindFilesTool::new();
        let args = serde_json::json!({
            "pattern": "Cargo.toml",
            "path": "."
        });
        let result = tool.execute(args).await.expect("Find files should succeed");
        assert!(result.contains("Cargo.toml"));
    }
}
