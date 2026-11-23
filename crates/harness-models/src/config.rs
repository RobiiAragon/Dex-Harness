use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::error::ModelError;
use crate::registry::ModelRegistry;

/// Persistent configuration of local directories scanned for GGUF, MLX, and Safetensors models.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelDirsConfig {
    pub model_dirs: Vec<String>,
    #[serde(default)]
    pub dismissed_defaults: Vec<String>,
    #[serde(skip)]
    pub config_path: PathBuf,
}

impl ModelDirsConfig {
    pub const CONFIG_FILE: &'static str = "config.json";

    pub fn default_config_path() -> PathBuf {
        ModelRegistry::default_dir().join(Self::CONFIG_FILE)
    }

    pub fn default_lmstudio_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".lmstudio/models"))
    }

    /// Expands leading `~/` against the user's home directory.
    pub fn expand_tilde(raw: &str) -> PathBuf {
        let trimmed = raw.trim();
        if let Some(rest) = trimmed.strip_prefix("~/") {
            if let Some(home) = dirs::home_dir() {
                return home.join(rest);
            }
        } else if trimmed == "~" {
            if let Some(home) = dirs::home_dir() {
                return home;
            }
        }
        PathBuf::from(trimmed)
    }

    /// Loads `~/.dex/models/config.json`, ensuring `~/.dex/models` is always present
    /// and automatically including `~/.lmstudio/models` if it exists on disk.
    pub async fn load_or_default() -> Self {
        let cfg_path = Self::default_config_path();
        let dex_dir = ModelRegistry::default_dir();
        let lmstudio_dir = Self::default_lmstudio_dir();
        Self::load_from(&cfg_path, &dex_dir, lmstudio_dir.as_deref()).await
    }

    pub async fn load_from(
        config_path: &Path,
        default_dex_dir: &Path,
        lmstudio_dir: Option<&Path>,
    ) -> Self {
        let mut cfg = if tokio::fs::try_exists(config_path).await.unwrap_or(false) {
            match tokio::fs::read_to_string(config_path).await {
                Ok(content) => match serde_json::from_str::<ModelDirsConfig>(&content) {
                    Ok(parsed) => parsed,
                    Err(e) => {
                        warn!(path = ?config_path, error = %e, "Failed to parse model dirs config; resetting defaults");
                        Self::default()
                    }
                },
                Err(e) => {
                    warn!(path = ?config_path, error = %e, "Failed to read model dirs config");
                    Self::default()
                }
            }
        } else {
            Self::default()
        };

        cfg.config_path = config_path.to_path_buf();

        // Always include default Dex models directory
        let dex_str = default_dex_dir.to_string_lossy().to_string();
        if !cfg.contains_path(&dex_str) {
            cfg.model_dirs.insert(0, dex_str);
        }

        // Auto-detect LM Studio / Bionic models directory if it exists and wasn't dismissed
        if let Some(lm_dir) = lmstudio_dir {
            if tokio::fs::try_exists(lm_dir).await.unwrap_or(false) {
                let lm_str = lm_dir.to_string_lossy().to_string();
                let dismissed = cfg
                    .dismissed_defaults
                    .iter()
                    .any(|d| normalize_cmp(d) == normalize_cmp(&lm_str));
                if !dismissed && !cfg.contains_path(&lm_str) {
                    cfg.model_dirs.push(lm_str);
                }
            }
        }

        cfg
    }

    pub fn contains_path(&self, candidate: &str) -> bool {
        let target = normalize_cmp(candidate);
        self.model_dirs.iter().any(|d| normalize_cmp(d) == target)
    }

    pub fn resolved_paths(&self) -> Vec<PathBuf> {
        self.model_dirs
            .iter()
            .map(|s| Self::expand_tilde(s))
            .collect()
    }

    pub async fn save(&self) -> Result<(), ModelError> {
        if let Some(parent) = self.config_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| ModelError::Io {
                    path: parent.to_string_lossy().to_string(),
                    source: e,
                })?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| ModelError::Json {
            url: self.config_path.to_string_lossy().to_string(),
            source: e,
        })?;
        tokio::fs::write(&self.config_path, json)
            .await
            .map_err(|e| ModelError::Io {
                path: self.config_path.to_string_lossy().to_string(),
                source: e,
            })?;
        debug!(path = ?self.config_path, dirs = self.model_dirs.len(), "Saved model dirs config");
        Ok(())
    }

    /// Adds a directory to `model_dirs` after verifying it exists and is a directory.
    pub async fn add_dir(&mut self, raw_path: &str) -> Result<String, ModelError> {
        let expanded = Self::expand_tilde(raw_path);
        if !tokio::fs::try_exists(&expanded).await.unwrap_or(false) {
            return Err(ModelError::InvalidInput {
                reason: format!("Model folder does not exist: '{}'", expanded.display()),
            });
        }
        let meta = tokio::fs::metadata(&expanded)
            .await
            .map_err(|e| ModelError::Io {
                path: expanded.to_string_lossy().to_string(),
                source: e,
            })?;
        if !meta.is_dir() {
            return Err(ModelError::InvalidInput {
                reason: format!("Path is not a directory: '{}'", expanded.display()),
            });
        }

        let canonical = tokio::fs::canonicalize(&expanded)
            .await
            .unwrap_or(expanded);
        let dir_str = canonical.to_string_lossy().to_string();

        self.dismissed_defaults
            .retain(|d| normalize_cmp(d) != normalize_cmp(&dir_str));
        if !self.contains_path(&dir_str) {
            self.model_dirs.push(dir_str.clone());
        }
        self.save().await?;
        Ok(dir_str)
    }

    /// Removes a directory from `model_dirs` and records it as dismissed if it was a default.
    pub async fn remove_dir(&mut self, raw_path: &str) -> Result<bool, ModelError> {
        let expanded = Self::expand_tilde(raw_path);
        let target_raw = normalize_cmp(raw_path);
        let target_exp = normalize_cmp(&expanded.to_string_lossy());
        let before = self.model_dirs.len();
        self.model_dirs.retain(|d| {
            let n = normalize_cmp(d);
            n != target_raw && n != target_exp
        });
        let removed = self.model_dirs.len() < before;
        if removed {
            let exp_str = expanded.to_string_lossy().to_string();
            if !self
                .dismissed_defaults
                .iter()
                .any(|d| normalize_cmp(d) == normalize_cmp(&exp_str))
            {
                self.dismissed_defaults.push(exp_str);
            }
            self.save().await?;
        }
        Ok(removed)
    }
}

fn normalize_cmp(p: &str) -> String {
    ModelDirsConfig::expand_tilde(p)
        .to_string_lossy()
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_model_dirs_config_autodetect_and_persistence() {
        let base = std::env::temp_dir().join(format!("dex_model_dirs_test_{}", std::process::id()));
        let dex_dir = base.join("dex_models");
        let lm_dir = base.join("lmstudio_models");
        let custom_dir = base.join("custom_models");
        let cfg_path = dex_dir.join("config.json");

        tokio::fs::create_dir_all(&dex_dir).await.expect("dex dir");
        tokio::fs::create_dir_all(&lm_dir).await.expect("lm dir");
        tokio::fs::create_dir_all(&custom_dir)
            .await
            .expect("custom dir");

        let mut cfg = ModelDirsConfig::load_from(&cfg_path, &dex_dir, Some(&lm_dir)).await;
        assert_eq!(cfg.model_dirs.len(), 2);
        assert!(cfg.contains_path(&dex_dir.to_string_lossy()));
        assert!(cfg.contains_path(&lm_dir.to_string_lossy()));

        // Add custom folder and persist
        cfg.add_dir(&custom_dir.to_string_lossy())
            .await
            .expect("add custom dir");
        assert_eq!(cfg.model_dirs.len(), 3);

        // Reload and confirm persistence
        let mut reloaded = ModelDirsConfig::load_from(&cfg_path, &dex_dir, Some(&lm_dir)).await;
        assert_eq!(reloaded.model_dirs.len(), 3);

        // Remove lm_dir and ensure it stays dismissed on next load
        assert!(reloaded
            .remove_dir(&lm_dir.to_string_lossy())
            .await
            .expect("remove lm_dir"));
        let after_rm = ModelDirsConfig::load_from(&cfg_path, &dex_dir, Some(&lm_dir)).await;
        assert!(!after_rm.contains_path(&lm_dir.to_string_lossy()));
        assert_eq!(after_rm.model_dirs.len(), 2);

        let _ = tokio::fs::remove_dir_all(&base).await;
    }
}
