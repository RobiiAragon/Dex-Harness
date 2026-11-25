//! Shared locations and bootstrap helpers for Dex's managed local text/image-to-3D sidecar
//! (`dex-gen3d`: OpenAI Shap-E through `diffusers`, served over HTTP on `127.0.0.1:8198`).
//!
//! It is the zero-setup default for `mesh_generate_3d`: plain pip packages in the managed
//! `~/.dex/runtimes/mlx-venv` (no CUDA, no compiled extensions), installed with 1 click as the
//! `gen3d` runtime. Higher-quality backends (TRELLIS, Hunyuan3D) remain opt-in extensions.

use std::path::{Path, PathBuf};

use crate::audiogen::{managed_venv_dir, venv_has_module};
use crate::storage::home_dex_dir;

/// Extension / sidecar identifier registered with the `ExtensionSupervisor`.
pub const DEX_GEN3D_SIDECAR_ID: &str = "dex-gen3d";
/// Default localhost port (8195-8197 are reserved for TRELLIS / Hunyuan3D / TripoSR).
pub const DEX_GEN3D_PORT: u16 = 8198;
/// Python packages installed into the managed venv by the 1-click `gen3d` runtime installer.
pub const GEN3D_PIP_PACKAGES: &[&str] = &[
    "torch",
    "diffusers",
    "transformers",
    "accelerate",
    "trimesh",
    "fast-simplification",
    "scipy",
    "pillow",
];

/// Embedded source of the sidecar server script.
pub const GEN3D_SERVER_SOURCE: &str = include_str!("../assets/dex_gen3d_server.py");

/// `~/.dex/runtimes/gen3d/dex_gen3d_server.py`.
pub fn gen3d_server_script_path() -> PathBuf {
    home_dex_dir()
        .join("runtimes")
        .join("gen3d")
        .join("dex_gen3d_server.py")
}

/// Default base URL of the sidecar (`http://127.0.0.1:8198`).
pub fn gen3d_default_base_url() -> String {
    format!("http://127.0.0.1:{DEX_GEN3D_PORT}")
}

/// Returns the managed venv interpreter if it can run Shap-E (torch + diffusers Shap-E + trimesh).
pub fn detect_gen3d_python() -> Option<PathBuf> {
    let venv = managed_venv_dir();
    let py = venv.join("bin").join("python");
    if !py.is_file() {
        return None;
    }
    let ready = venv_has_module(&venv, "torch")
        && venv_has_module(&venv, "diffusers/pipelines/shap_e")
        && venv_has_module(&venv, "trimesh");
    ready.then_some(py)
}

/// Writes the embedded sidecar script to `~/.dex/runtimes/gen3d/` if missing or outdated.
pub async fn ensure_gen3d_server_script() -> std::io::Result<PathBuf> {
    let path = gen3d_server_script_path();
    if let Ok(existing) = tokio::fs::read_to_string(&path).await {
        if existing == GEN3D_SERVER_SOURCE {
            return Ok(path);
        }
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, GEN3D_SERVER_SOURCE).await?;
    Ok(path)
}

/// Command-line arguments used to launch the sidecar with the managed interpreter.
pub fn gen3d_launch_args(script: &Path, port: u16) -> Vec<String> {
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
    fn embedded_server_exposes_generate_and_health() {
        assert!(GEN3D_SERVER_SOURCE.contains("/v1/3d/generate"));
        assert!(GEN3D_SERVER_SOURCE.contains("/health"));
        assert!(GEN3D_SERVER_SOURCE.contains("ShapEPipeline"));
        assert!(gen3d_server_script_path().ends_with("runtimes/gen3d/dex_gen3d_server.py"));
        assert_eq!(gen3d_default_base_url(), "http://127.0.0.1:8198");
    }
}
