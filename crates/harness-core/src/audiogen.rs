//! Shared locations and bootstrap helpers for Dex's managed local music/SFX sidecar
//! (`dex-audiogen`: MusicGen for music and SFX, optional AudioLDM, served over HTTP on `127.0.0.1:8189`).
//!
//! The Python server ships embedded in this crate and is materialized into
//! `~/.dex/runtimes/audiogen/` on demand, then executed with the managed
//! `~/.dex/runtimes/mlx-venv` interpreter (never the system Python).

use std::path::{Path, PathBuf};

use crate::storage::home_dex_dir;

/// Extension / sidecar identifier registered with the `ExtensionSupervisor`.
pub const DEX_AUDIOGEN_SIDECAR_ID: &str = "dex-audiogen";
/// Default localhost port for the music/SFX sidecar.
pub const DEX_AUDIOGEN_PORT: u16 = 8189;
/// Default Hugging Face model used for music generation.
pub const DEFAULT_MUSICGEN_MODEL: &str = "facebook/musicgen-small";
/// Python packages installed into the managed venv by the 1-click `audiogen` runtime installer.
/// (AudioLDM via `diffusers` is opt-in with `DEX_SFX_BACKEND=audioldm` and not installed by default.)
pub const AUDIOGEN_PIP_PACKAGES: &[&str] = &["torch", "transformers", "soundfile", "numpy", "scipy"];

/// Embedded source of the sidecar server script.
pub const AUDIOGEN_SERVER_SOURCE: &str = include_str!("../assets/dex_audiogen_server.py");

/// `~/.dex/runtimes/mlx-venv` (shared managed Python environment).
pub fn managed_venv_dir() -> PathBuf {
    home_dex_dir().join("runtimes").join("mlx-venv")
}

/// Python interpreter inside the managed venv.
pub fn managed_venv_python() -> PathBuf {
    managed_venv_dir().join("bin").join("python")
}

/// `~/.dex/runtimes/audiogen/dex_audiogen_server.py`.
pub fn audiogen_server_script_path() -> PathBuf {
    home_dex_dir()
        .join("runtimes")
        .join("audiogen")
        .join("dex_audiogen_server.py")
}

/// Default base URL of the sidecar (`http://127.0.0.1:8189`).
pub fn audiogen_default_base_url() -> String {
    format!("http://127.0.0.1:{DEX_AUDIOGEN_PORT}")
}

pub(crate) fn venv_has_module(venv_dir: &Path, module_rel: &str) -> bool {
    let Ok(rd) = std::fs::read_dir(venv_dir.join("lib")) else {
        return false;
    };
    rd.flatten()
        .any(|entry| entry.path().join("site-packages").join(module_rel).exists())
}

/// Returns the managed venv interpreter if it can run MusicGen (torch + transformers MusicGen + soundfile).
pub fn detect_audiogen_python() -> Option<PathBuf> {
    let venv = managed_venv_dir();
    let py = venv.join("bin").join("python");
    if !py.is_file() {
        return None;
    }
    let ready = venv_has_module(&venv, "torch")
        && venv_has_module(&venv, "transformers/models/musicgen")
        && (venv_has_module(&venv, "soundfile.py") || venv_has_module(&venv, "soundfile"));
    ready.then_some(py)
}

/// Writes the embedded sidecar script to `~/.dex/runtimes/audiogen/` if missing or outdated.
pub async fn ensure_audiogen_server_script() -> std::io::Result<PathBuf> {
    let path = audiogen_server_script_path();
    if let Ok(existing) = tokio::fs::read_to_string(&path).await {
        if existing == AUDIOGEN_SERVER_SOURCE {
            return Ok(path);
        }
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, AUDIOGEN_SERVER_SOURCE).await?;
    Ok(path)
}

/// Command-line arguments used to launch the sidecar with the managed interpreter.
pub fn audiogen_launch_args(script: &Path, port: u16) -> Vec<String> {
    vec![
        script.to_string_lossy().to_string(),
        "--host".to_string(),
        "127.0.0.1".to_string(),
        "--port".to_string(),
        port.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedded_server_exposes_expected_endpoints() {
        assert!(AUDIOGEN_SERVER_SOURCE.contains("/v1/audio/music"));
        assert!(AUDIOGEN_SERVER_SOURCE.contains("/v1/audio/sfx"));
        assert!(AUDIOGEN_SERVER_SOURCE.contains("/health"));
        assert!(AUDIOGEN_SERVER_SOURCE.contains("MusicgenForConditionalGeneration"));
    }

    #[test]
    fn test_paths_live_under_home_dex_runtimes() {
        let script = audiogen_server_script_path();
        assert!(script.ends_with("runtimes/audiogen/dex_audiogen_server.py"));
        assert!(managed_venv_python().ends_with("runtimes/mlx-venv/bin/python"));
        let args = audiogen_launch_args(&script, 9999);
        assert_eq!(args.last().map(String::as_str), Some("9999"));
        assert_eq!(audiogen_default_base_url(), "http://127.0.0.1:8189");
    }
}
