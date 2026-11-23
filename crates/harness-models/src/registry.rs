use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::error::ModelError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    HuggingFace,
    Ollama,
    Local,
}

impl std::fmt::Display for ModelSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HuggingFace => write!(f, "huggingface"),
            Self::Ollama => write!(f, "ollama"),
            Self::Local => write!(f, "local"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelMetadata {
    pub id: String,
    pub name: String,
    pub source: ModelSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<String>,
    pub size_bytes: u64,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelRegistry {
    pub models: Vec<ModelMetadata>,
    #[serde(skip)]
    pub manifest_path: PathBuf,
    #[serde(skip)]
    pub models_dir: PathBuf,
}

impl ModelRegistry {
    pub const DEFAULT_DIR: &'static str = ".dex/models";
    pub const MANIFEST_FILE: &'static str = "registry.json";

    pub fn default_dir() -> PathBuf {
        dirs::home_dir()
            .map(|h| h.join(Self::DEFAULT_DIR))
            .unwrap_or_else(|| PathBuf::from(Self::DEFAULT_DIR))
    }

    pub fn default_manifest_path() -> PathBuf {
        Self::default_dir().join(Self::MANIFEST_FILE)
    }

    pub async fn load_or_default() -> Self {
        let manifest = Self::default_manifest_path();
        let models_dir = Self::default_dir();
        Self::load_from(manifest, models_dir).await
    }

    pub async fn load_from(manifest_path: impl AsRef<Path>, models_dir: impl AsRef<Path>) -> Self {
        let manifest = manifest_path.as_ref().to_path_buf();
        let dir = models_dir.as_ref().to_path_buf();

        if tokio::fs::try_exists(&manifest).await.unwrap_or(false) {
            match tokio::fs::read_to_string(&manifest).await {
                Ok(content) => match serde_json::from_str::<ModelRegistry>(&content) {
                    Ok(mut registry) => {
                        registry.manifest_path = manifest;
                        registry.models_dir = dir;
                        return registry;
                    }
                    Err(e) => {
                        warn!("Failed to deserialize model registry at {:?}: {}", manifest, e);
                    }
                },
                Err(e) => {
                    warn!("Failed to read model registry file at {:?}: {}", manifest, e);
                }
            }
        }

        Self {
            models: Vec::new(),
            manifest_path: manifest,
            models_dir: dir,
        }
    }

    pub async fn save(&self) -> Result<(), ModelError> {
        if let Some(parent) = self.manifest_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| ModelError::Io {
                path: parent.to_string_lossy().to_string(),
                source: e,
            })?;
        }

        let json = serde_json::to_string_pretty(self).map_err(|e| ModelError::Json {
            url: self.manifest_path.to_string_lossy().to_string(),
            source: e,
        })?;

        tokio::fs::write(&self.manifest_path, json).await.map_err(|e| ModelError::Io {
            path: self.manifest_path.to_string_lossy().to_string(),
            source: e,
        })?;

        debug!("Model registry saved to {:?}", self.manifest_path);
        Ok(())
    }

    pub async fn add(&mut self, metadata: ModelMetadata) -> Result<(), ModelError> {
        // Upsert if model with same id already exists
        if let Some(idx) = self.models.iter().position(|m| m.id == metadata.id) {
            info!(id = %metadata.id, "Updating existing model entry in registry");
            self.models[idx] = metadata;
        } else {
            info!(id = %metadata.id, "Adding new model entry to registry");
            self.models.push(metadata);
        }
        self.save().await
    }

    pub async fn remove(&mut self, id: &str, delete_file: bool) -> Result<Option<ModelMetadata>, ModelError> {
        let pos = self.models.iter().position(|m| m.id == id);
        if let Some(idx) = pos {
            let removed = self.models.remove(idx);
            if delete_file {
                if let Some(ref path_str) = removed.file_path {
                    let path = PathBuf::from(path_str);
                    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
                        info!(path = ?path, "Deleting model file from disk");
                        let _ = tokio::fs::remove_file(&path).await;
                    }
                }
            }
            self.save().await?;
            Ok(Some(removed))
        } else {
            Ok(None)
        }
    }

    pub fn get(&self, id: &str) -> Option<&ModelMetadata> {
        self.models.iter().find(|m| m.id == id || m.name == id)
    }

    pub fn list(&self) -> &[ModelMetadata] {
        &self.models
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_model_registry_crud_lifecycle() {
        let temp_dir = std::env::temp_dir().join(format!("dex_reg_test_{}", std::process::id()));
        let manifest = temp_dir.join("registry.json");
        let models_dir = temp_dir.join("models");

        let mut registry = ModelRegistry::load_from(&manifest, &models_dir).await;
        assert_eq!(registry.list().len(), 0);

        let dummy_path = models_dir.join("dummy-model.gguf");
        tokio::fs::create_dir_all(&models_dir).await.expect("create test dir");
        tokio::fs::write(&dummy_path, b"dummy gguf content").await.expect("write dummy");

        let model = ModelMetadata {
            id: "local:dummy-model".to_string(),
            name: "dummy-model".to_string(),
            source: ModelSource::Local,
            file_path: Some(dummy_path.to_string_lossy().to_string()),
            repo_id: None,
            filename: Some("dummy-model.gguf".to_string()),
            quantization: Some("Q4_K_M".to_string()),
            size_bytes: 18,
            created_at: 1700000000,
        };

        // Add
        registry.add(model).await.expect("add model");
        assert_eq!(registry.list().len(), 1);
        assert!(registry.get("local:dummy-model").is_some());

        // Reload from disk to verify persistence
        let reloaded = ModelRegistry::load_from(&manifest, &models_dir).await;
        assert_eq!(reloaded.list().len(), 1);
        let found = reloaded.get("local:dummy-model").expect("model found");
        assert_eq!(found.quantization.as_deref(), Some("Q4_K_M"));

        // Remove with file deletion
        let removed = registry.remove("local:dummy-model", true).await.expect("remove model");
        assert!(removed.is_some());
        assert_eq!(registry.list().len(), 0);
        assert!(!tokio::fs::try_exists(&dummy_path).await.unwrap_or(true));

        // Cleanup temp dir
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }
}
