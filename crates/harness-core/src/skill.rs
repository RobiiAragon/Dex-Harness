use std::collections::HashMap;
use std::path::{Path, PathBuf};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::{debug, info};

use crate::error::ToolError;
use crate::tool::Tool;

#[derive(Debug, Clone, Deserialize)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
}

pub type SkillMetadata = SkillFrontmatter;

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub file_path: PathBuf,
    pub content: String,
}

/// Discovers and progressively loads skills from `SKILL.md` documents.
#[derive(Debug, Clone, Default)]
pub struct ProgressiveSkillLoader {
    skills: HashMap<String, Skill>,
}

impl ProgressiveSkillLoader {
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
        }
    }

    /// Parses a single markdown file with YAML frontmatter into a Skill.
    pub fn parse_skill_content(content: &str, file_path: PathBuf) -> Option<Skill> {
        let trimmed = content.trim_start();
        if !trimmed.starts_with("---") {
            return None;
        }

        let after_first = &trimmed[3..];
        let end_idx = after_first.find("---")?;
        let yaml_str = &after_first[..end_idx];
        let body = after_first[end_idx + 3..].trim().to_string();

        let frontmatter: SkillFrontmatter = serde_yaml::from_str(yaml_str).ok()?;

        Some(Skill {
            name: frontmatter.name,
            description: frontmatter.description,
            file_path,
            content: body,
        })
    }

    /// Recursively scans a directory for `SKILL.md` files.
    pub async fn scan_directory(&mut self, root: impl AsRef<Path>) {
        let root_path = root.as_ref();
        if !tokio::fs::try_exists(root_path).await.unwrap_or(false) {
            return;
        }

        let mut dirs = vec![root_path.to_path_buf()];

        while let Some(current_dir) = dirs.pop() {
            let mut entries = match tokio::fs::read_dir(&current_dir).await {
                Ok(e) => e,
                Err(_) => continue,
            };

            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.file_name().and_then(|n| n.to_str()) == Some("SKILL.md") {
                    if let Ok(content) = tokio::fs::read_to_string(&path).await {
                        if let Some(skill) = Self::parse_skill_content(&content, path.clone()) {
                            info!(skill = %skill.name, path = %path.display(), "Registered skill");
                            self.skills.insert(skill.name.clone(), skill);
                        }
                    }
                }
            }
        }
    }

    /// Scans standard locations (`.dex/skills/` in workspace and `~/.dex/skills/`).
    pub async fn scan_standard_locations(&mut self) {
        // Workspace directory
        self.scan_directory(".dex/skills").await;

        // User home directory
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            self.scan_directory(home.join(".dex/skills")).await;
        }
    }

    /// Generates a compact index of available skills for the system prompt.
    pub fn generate_prompt_index(&self) -> String {
        if self.skills.is_empty() {
            return String::new();
        }

        let mut index = String::from("\n## Available Skills\n");
        index.push_str("To execute specialized workflows, invoke the `load_skill` tool to inspect their complete instructions:\n");
        for skill in self.skills.values() {
            index.push_str(&format!("- **{}**: {}\n", skill.name, skill.description));
        }
        index
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LoadSkillArgs {
    /// The unique name of the skill to load (as listed in Available Skills).
    pub name: String,
}

/// Agent tool that dynamically loads the full instructions of a skill.
pub struct LoadSkillTool {
    loader: ProgressiveSkillLoader,
}

impl LoadSkillTool {
    pub fn new(loader: ProgressiveSkillLoader) -> Self {
        Self { loader }
    }
}

#[async_trait]
impl Tool for LoadSkillTool {
    fn name(&self) -> &str {
        "load_skill"
    }

    fn description(&self) -> &str {
        "Loads the complete instructions, steps, and script references for an available skill. Use this when a task aligns with a known skill."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let schema = schemars::schema_for!(LoadSkillArgs);
        serde_json::to_value(schema).unwrap_or(serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Skill name to load" }
            },
            "required": ["name"]
        }))
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String, ToolError> {
        let args: LoadSkillArgs = serde_json::from_value(arguments).map_err(|e| {
            ToolError::InvalidArguments {
                name: self.name().to_string(),
                reason: format!("Failed to parse arguments: {e}"),
            }
        })?;

        debug!(skill = %args.name, "Loading skill content");

        match self.loader.get(&args.name) {
            Some(skill) => Ok(format!(
                "### Skill: {}\n**Description:** {}\n**Source:** {}\n\n---\n{}",
                skill.name,
                skill.description,
                skill.file_path.display(),
                skill.content
            )),
            None => Err(ToolError::NotFound {
                name: format!("Skill '{}' not found in catalog", args.name),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_skill_frontmatter() {
        let raw = "\
---
name: code-reviewer
description: Performs rigorous code reviews on diffs
---

# Instructions
1. Inspect the git diff carefully.
2. Check for security vulnerabilities.
";
        let skill = ProgressiveSkillLoader::parse_skill_content(raw, PathBuf::from("dummy/SKILL.md"))
            .expect("Skill should parse successfully");

        assert_eq!(skill.name, "code-reviewer");
        assert_eq!(skill.description, "Performs rigorous code reviews on diffs");
        assert!(skill.content.contains("# Instructions"));
        assert!(skill.content.contains("2. Check for security vulnerabilities."));
    }

    #[tokio::test]
    async fn test_progressive_skill_loader_scanning() {
        let temp_dir = std::env::temp_dir().join("dex_test_skills_dir");
        let skill_dir = temp_dir.join("reviewer");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();

        let raw = "\
---
name: test-skill
description: Skill for testing
---
Run tests thoroughly.
";
        tokio::fs::write(skill_dir.join("SKILL.md"), raw).await.unwrap();

        let mut loader = ProgressiveSkillLoader::new();
        loader.scan_directory(&temp_dir).await;

        assert!(!loader.is_empty());
        let index = loader.generate_prompt_index();
        assert!(index.contains("test-skill"));
        assert!(index.contains("Skill for testing"));

        let skill = loader.get("test-skill").unwrap();
        assert_eq!(skill.name, "test-skill");

        let _ = tokio::fs::remove_dir_all(temp_dir).await;
    }
}
