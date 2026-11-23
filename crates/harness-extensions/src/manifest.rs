use std::collections::HashMap;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::error::ExtensionError;

pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 600;

fn default_idle_timeout() -> u64 {
    DEFAULT_IDLE_TIMEOUT_SECS
}

/// Functional domain category for extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionCategory {
    Audio,
    ImageVideo,
    ThreeD,
    OcrDocs,
    GameDev,
    Custom,
}

impl ExtensionCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::ImageVideo => "image_video",
            Self::ThreeD => "three_d",
            Self::OcrDocs => "ocr_docs",
            Self::GameDev => "game_dev",
            Self::Custom => "custom",
        }
    }

    pub fn display_label(&self) -> &'static str {
        match self {
            Self::Audio => "Audio",
            Self::ImageVideo => "Imagen/Video",
            Self::ThreeD => "3D",
            Self::OcrDocs => "OCR/Docs",
            Self::GameDev => "GameDev",
            Self::Custom => "Custom",
        }
    }

    pub fn from_str_loose(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "audio" => Self::Audio,
            "image_video" | "imagevideo" | "image" | "video" | "imagen/video" => Self::ImageVideo,
            "three_d" | "threed" | "3d" => Self::ThreeD,
            "ocr_docs" | "ocrdocs" | "ocr" | "docs" | "ocr/docs" => Self::OcrDocs,
            "game_dev" | "gamedev" | "game" => Self::GameDev,
            _ => Self::Custom,
        }
    }
}

impl std::fmt::Display for ExtensionCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_label())
    }
}

/// Transport and execution model for an external extension.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtensionKind {
    McpStdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    McpSse {
        url: String,
    },
    LocalSidecar {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        port: u16,
        healthcheck_url: String,
        #[serde(default)]
        env: HashMap<String, String>,
    },
}

/// Persistent manifest entry describing an installed or catalog extension.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionManifest {
    pub id: String,
    pub name: String,
    pub category: ExtensionCategory,
    pub description: String,
    pub kind: ExtensionKind,
    pub estimated_ram_mb: u64,
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_secs: u64,
    #[serde(default)]
    pub install_hint: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Persistent extension registry stored at `~/.dex/extensions/extensions.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExtensionRegistry {
    pub extensions: Vec<ExtensionManifest>,
    #[serde(skip)]
    pub manifest_path: PathBuf,
}

impl ExtensionRegistry {
    pub const DEFAULT_DIR: &'static str = ".dex/extensions";
    pub const MANIFEST_FILE: &'static str = "extensions.json";

    pub fn default_dir() -> PathBuf {
        dirs::home_dir()
            .map(|h| h.join(Self::DEFAULT_DIR))
            .unwrap_or_else(|| PathBuf::from(Self::DEFAULT_DIR))
    }

    pub fn default_manifest_path() -> PathBuf {
        Self::default_dir().join(Self::MANIFEST_FILE)
    }

    pub async fn load_or_default() -> Self {
        Self::load_from(Self::default_manifest_path()).await
    }

    pub async fn load_from(manifest_path: impl AsRef<Path>) -> Self {
        let path = manifest_path.as_ref().to_path_buf();

        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            match tokio::fs::read_to_string(&path).await {
                Ok(content) => match serde_json::from_str::<ExtensionRegistry>(&content) {
                    Ok(mut reg) => {
                        reg.manifest_path = path;
                        return reg;
                    }
                    Err(e) => {
                        warn!(path = ?path, error = %e, "Failed to parse extensions manifest");
                    }
                },
                Err(e) => {
                    warn!(path = ?path, error = %e, "Failed to read extensions manifest");
                }
            }
        }

        Self {
            extensions: Vec::new(),
            manifest_path: path,
        }
    }

    pub async fn save(&self) -> Result<(), ExtensionError> {
        if let Some(parent) = self.manifest_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| ExtensionError::Io {
                    path: parent.to_string_lossy().to_string(),
                    source: e,
                })?;
        }

        let json = serde_json::to_string_pretty(self).map_err(|e| ExtensionError::Json {
            context: self.manifest_path.to_string_lossy().to_string(),
            source: e,
        })?;

        tokio::fs::write(&self.manifest_path, json)
            .await
            .map_err(|e| ExtensionError::Io {
                path: self.manifest_path.to_string_lossy().to_string(),
                source: e,
            })?;

        debug!(path = ?self.manifest_path, count = self.extensions.len(), "Saved extensions registry");
        Ok(())
    }

    pub async fn upsert(&mut self, manifest: ExtensionManifest) -> Result<(), ExtensionError> {
        if let Some(idx) = self.extensions.iter().position(|e| e.id == manifest.id) {
            info!(id = %manifest.id, "Updating existing extension manifest");
            self.extensions[idx] = manifest;
        } else {
            info!(id = %manifest.id, "Adding new extension manifest");
            self.extensions.push(manifest);
        }
        self.save().await
    }

    pub async fn remove(&mut self, id: &str) -> Result<Option<ExtensionManifest>, ExtensionError> {
        if let Some(idx) = self.extensions.iter().position(|e| e.id == id) {
            let removed = self.extensions.remove(idx);
            self.save().await?;
            Ok(Some(removed))
        } else {
            Ok(None)
        }
    }

    pub async fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<ExtensionManifest, ExtensionError> {
        let entry = self
            .extensions
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| ExtensionError::NotFound { id: id.to_string() })?;
        entry.enabled = enabled;
        let cloned = entry.clone();
        self.save().await?;
        Ok(cloned)
    }

    pub async fn set_idle_timeout(
        &mut self,
        id: &str,
        idle_timeout_secs: u64,
    ) -> Result<ExtensionManifest, ExtensionError> {
        let entry = self
            .extensions
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| ExtensionError::NotFound { id: id.to_string() })?;
        entry.idle_timeout_secs = idle_timeout_secs;
        let cloned = entry.clone();
        self.save().await?;
        Ok(cloned)
    }

    pub fn get(&self, id: &str) -> Option<&ExtensionManifest> {
        self.extensions.iter().find(|e| e.id == id)
    }

    pub fn list(&self) -> &[ExtensionManifest] {
        &self.extensions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_extension_registry_persistence_crud() {
        let temp_dir =
            std::env::temp_dir().join(format!("dex_ext_reg_test_{}", std::process::id()));
        let manifest_path = temp_dir.join("extensions.json");

        let mut reg = ExtensionRegistry::load_from(&manifest_path).await;
        assert!(reg.list().is_empty());

        let ext = ExtensionManifest {
            id: "custom-blender".to_string(),
            name: "Custom Blender MCP".to_string(),
            category: ExtensionCategory::ThreeD,
            description: "Control Blender scenes".to_string(),
            kind: ExtensionKind::McpStdio {
                command: "uvx".to_string(),
                args: vec!["blender-mcp".to_string()],
                env: HashMap::new(),
            },
            estimated_ram_mb: 3072,
            idle_timeout_secs: 300,
            install_hint: "pip install uv".to_string(),
            enabled: true,
            tags: vec!["blender".to_string(), "3d".to_string()],
        };

        reg.upsert(ext.clone()).await.expect("upsert failed");
        assert_eq!(reg.list().len(), 1);

        // Reload from disk and verify fields survived round-trip
        let mut reloaded = ExtensionRegistry::load_from(&manifest_path).await;
        assert_eq!(reloaded.list().len(), 1);
        let found = reloaded.get("custom-blender").expect("extension missing");
        assert_eq!(found.name, "Custom Blender MCP");
        assert_eq!(found.idle_timeout_secs, 300);
        assert!(found.enabled);

        // Toggle enabled & idle timeout
        reloaded
            .set_enabled("custom-blender", false)
            .await
            .expect("disable failed");
        reloaded
            .set_idle_timeout("custom-blender", 120)
            .await
            .expect("timeout update failed");

        let reloaded2 = ExtensionRegistry::load_from(&manifest_path).await;
        let updated = reloaded2.get("custom-blender").expect("extension missing");
        assert!(!updated.enabled);
        assert_eq!(updated.idle_timeout_secs, 120);

        // Remove
        let mut reg_rm = reloaded2;
        let removed = reg_rm.remove("custom-blender").await.expect("remove failed");
        assert!(removed.is_some());
        assert!(reg_rm.list().is_empty());

        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }
}
