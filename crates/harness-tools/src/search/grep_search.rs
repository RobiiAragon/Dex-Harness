use async_trait::async_trait;
use ignore::WalkBuilder;
use regex::RegexBuilder;
use schemars::JsonSchema;
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use tracing::instrument;

use harness_core::{Tool, ToolError};

/// Input parameters for regex code search.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GrepSearchArgs {
    /// Regular expression or substring pattern to search for.
    pub pattern: String,
    /// Root path (directory or file) to search within. Defaults to active workspace root (or ".").
    pub path: Option<String>,
    /// Match case-sensitively. Defaults to false.
    pub case_sensitive: Option<bool>,
    /// Maximum number of matching lines to return. Defaults to 50.
    pub max_results: Option<usize>,
}

#[derive(Default, Clone)]
pub struct GrepSearchTool {
    workspace_root: Option<PathBuf>,
}

impl GrepSearchTool {
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
impl Tool for GrepSearchTool {
    fn name(&self) -> &str {
        "grep_search"
    }

    fn description(&self) -> &str {
        "Fast regular expression search across project files respecting .gitignore. Returns matching file paths, line numbers, and contents."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(GrepSearchArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex pattern to match" },
                "path": { "type": "string", "description": "Search directory or file" },
                "case_sensitive": { "type": "boolean", "description": "Case sensitive match" },
                "max_results": { "type": "integer", "description": "Maximum matching lines" }
            },
            "required": ["pattern"]
        }))
    }

    #[instrument(skip(self, arguments))]
    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: GrepSearchArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        let case_sensitive = args.case_sensitive.unwrap_or(false);
        let max_results = args.max_results.unwrap_or(50).max(1);
        let root_path = self.resolve_root(args.path.as_deref());
        let root_str = root_path.to_string_lossy().to_string();

        let regex = RegexBuilder::new(&args.pattern)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|e| ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Invalid regex pattern '{}': {e}", args.pattern),
            })?;

        // Offload walking and matching to a blocking task to keep tokio runtime responsive
        let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let mut matches = Vec::new();
            let mut total_matches = 0;

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

                // Skip non-text or binary files cleanly
                let file = match File::open(path) {
                    Ok(f) => f,
                    Err(_) => continue,
                };

                let reader = BufReader::new(file);
                for (line_idx, line_res) in reader.lines().enumerate() {
                    let line = match line_res {
                        Ok(l) => l,
                        Err(_) => break, // Likely non-UTF8 binary file, skip remainder
                    };

                    if regex.is_match(&line) {
                        total_matches += 1;
                        if matches.len() < max_results {
                            let rel_path = path
                                .strip_prefix(&root_path)
                                .or_else(|_| path.strip_prefix("."))
                                .unwrap_or(path);
                            matches.push(format!("{}:{}: {}", rel_path.display(), line_idx + 1, line.trim()));
                        }
                    }
                }

                if matches.len() >= max_results {
                    break;
                }
            }

            if matches.is_empty() {
                return Ok(format!("No matches found for pattern '{}' in '{}'", args.pattern, root_str));
            }

            let mut output = format!("Found {} match(es) for pattern '{}':\n\n", total_matches, args.pattern);
            output.push_str(&matches.join("\n"));
            if total_matches > max_results {
                output.push_str(&format!("\n\n... [Showing first {max_results} results out of {total_matches}]"));
            }

            Ok(output)
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            name: "grep_search".to_string(),
            message: format!("Blocking search task panicked: {e}"),
        })?
        .map_err(|e| ToolError::ExecutionFailed {
            name: "grep_search".to_string(),
            message: e,
        })?;

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_grep_search_cargo_toml() {
        let tool = GrepSearchTool::new();
        let args = serde_json::json!({
            "pattern": "workspace",
            "path": "."
        });
        let result = tool.execute(args).await.expect("Search should succeed");
        assert!(result.contains("Cargo.toml"));
        assert!(result.contains("workspace"));
    }
}
