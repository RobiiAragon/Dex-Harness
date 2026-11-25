use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use harness_core::{DexEvent, EventBus};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::error::ModelError;

pub const DEX_MLX_PORT: u16 = 8088;
pub const DEX_LLAMA_PORT: u16 = 8089;
pub const DEX_RUNTIME_SIDECAR_ID: &str = "dex-local-runtime";
pub const LOCAL_MODEL_STARTUP_TIMEOUT_SECS: u64 = 90;
const RUNTIME_INSTALL_TIMEOUT_SECS: u64 = 600;

/// Supported autonomous local inference and multimodal runtimes managed by Dex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    Mlx,
    LlamaCpp,
    MlxWhisper,
    Mflux,
    MlxAudio,
    ParakeetMlx,
    FasterWhisper,
    AudioGen,
    /// Text/image-to-3D (OpenAI Shap-E via diffusers), served by the `dex-gen3d` sidecar.
    Gen3d,
    /// FFmpeg (video editing, frame extraction, audio conversion) via Homebrew.
    Ffmpeg,
    /// Blender (3D convert / render) via Homebrew cask.
    Blender,
    /// `uv` (runs Python MCP extensions with `uvx`) via Homebrew.
    Uv,
    /// Node.js (runs Node MCP extensions with `npx`) via Homebrew.
    Node,
}

/// Homebrew package (name, is_cask) for system-tool runtimes.
fn brew_package(kind: RuntimeKind) -> Option<(&'static str, bool)> {
    match kind {
        RuntimeKind::LlamaCpp => Some(("llama.cpp", false)),
        RuntimeKind::Ffmpeg => Some(("ffmpeg", false)),
        RuntimeKind::Blender => Some(("blender", true)),
        RuntimeKind::Uv => Some(("uv", false)),
        RuntimeKind::Node => Some(("node", false)),
        _ => None,
    }
}

/// Every runtime Dex can install, in display order.
pub const ALL_RUNTIMES: &[RuntimeKind] = &[
    RuntimeKind::Mlx,
    RuntimeKind::LlamaCpp,
    RuntimeKind::MlxWhisper,
    RuntimeKind::Mflux,
    RuntimeKind::MlxAudio,
    RuntimeKind::ParakeetMlx,
    RuntimeKind::FasterWhisper,
    RuntimeKind::AudioGen,
    RuntimeKind::Gen3d,
    RuntimeKind::Ffmpeg,
    RuntimeKind::Blender,
    RuntimeKind::Uv,
    RuntimeKind::Node,
];

impl RuntimeKind {
    pub fn as_id(self) -> &'static str {
        match self {
            Self::Mlx => "mlx",
            Self::LlamaCpp => "llamacpp",
            Self::MlxWhisper => "mlx-whisper",
            Self::Mflux => "mflux",
            Self::MlxAudio => "mlx-audio",
            Self::ParakeetMlx => "parakeet-mlx",
            Self::FasterWhisper => "faster-whisper",
            Self::AudioGen => "audiogen",
            Self::Gen3d => "gen3d",
            Self::Ffmpeg => "ffmpeg",
            Self::Blender => "blender",
            Self::Uv => "uv",
            Self::Node => "node",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Mlx => "MLX Runtime (Apple Silicon)",
            Self::LlamaCpp => "llama.cpp Server (Metal GGUF)",
            Self::MlxWhisper => "Voice STT (mlx-whisper)",
            Self::Mflux => "Image Gen (mflux / FLUX MLX)",
            Self::MlxAudio => "Voice Studio Engine (mlx-audio)",
            Self::ParakeetMlx => "Parakeet TDT v3 ASR (MLX)",
            Self::FasterWhisper => "Faster-Whisper (CPU Fallback)",
            Self::AudioGen => "Music & SFX Gen (MusicGen)",
            Self::Gen3d => "3D Gen (Shap-E)",
            Self::Ffmpeg => "FFmpeg (video & audio)",
            Self::Blender => "Blender (3D convert & render)",
            Self::Uv => "uv (Python extensions)",
            Self::Node => "Node.js (JS extensions)",
        }
    }

    pub fn from_str_loose(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().as_str() {
            "mlx" | "mlx-lm" | "mlx_lm" => Some(Self::Mlx),
            "llamacpp" | "llama.cpp" | "llama-cpp" | "llama" | "gguf" => Some(Self::LlamaCpp),
            "mlx-whisper" | "mlx_whisper" | "whisper" | "stt" => Some(Self::MlxWhisper),
            "mflux" | "mflux-generate" | "flux" | "image" => Some(Self::Mflux),
            "mlx-audio" | "mlx_audio" | "audio" | "tts" | "voice-engine" => Some(Self::MlxAudio),
            "parakeet-mlx" | "parakeet_mlx" | "parakeet" => Some(Self::ParakeetMlx),
            "faster-whisper" | "faster_whisper" => Some(Self::FasterWhisper),
            "audiogen" | "audio-gen" | "musicgen" | "music" | "sfx" | "dex-audiogen" => {
                Some(Self::AudioGen)
            }
            "gen3d" | "3d" | "shap-e" | "shape" | "dex-gen3d" | "mesh" => Some(Self::Gen3d),
            "ffmpeg" | "ffprobe" | "video" => Some(Self::Ffmpeg),
            "blender" => Some(Self::Blender),
            "uv" | "uvx" => Some(Self::Uv),
            "node" | "nodejs" | "npx" | "npm" => Some(Self::Node),
            _ => None,
        }
    }

    pub fn install_hint(self) -> &'static str {
        match self {
            Self::Mlx => {
                "Click 'Install MLX Runtime (1-click)' to create an isolated ~/.dex/runtimes/mlx-venv with mlx-lm."
            }
            Self::LlamaCpp => {
                "Click 'Install llama.cpp (1-click)' (runs `brew install llama.cpp`) or place `llama-server` in ~/.dex/runtimes/llama/."
            }
            Self::MlxWhisper => {
                "Click 'Install Voice STT (1-click)' to install `mlx-whisper` into ~/.dex/runtimes/mlx-venv."
            }
            Self::Mflux => {
                "Click 'Install Image Gen (1-click)' to install `mflux` into ~/.dex/runtimes/mlx-venv."
            }
            Self::MlxAudio => {
                "Click 'Install Voice Studio Engine (1-click)' to install `mlx-audio` (Qwen3-TTS, Chatterbox, Kokoro, Parakeet) into ~/.dex/runtimes/mlx-venv."
            }
            Self::ParakeetMlx => {
                "Click 'Install Parakeet MLX (1-click)' to install `parakeet-mlx` (ultra-fast 25-language Apple Silicon ASR)."
            }
            Self::FasterWhisper => {
                "Click 'Install Faster-Whisper (1-click)' for cross-platform CPU/CTranslate2 Whisper transcription."
            }
            Self::AudioGen => {
                "Click 'Install Music & SFX Gen (1-click)' (or run `dex models install-runtime audiogen`) to install torch + transformers MusicGen into ~/.dex/runtimes/mlx-venv."
            }
            Self::Gen3d => {
                "Click 'Install 3D Gen (1-click)' (or run `dex models install-runtime gen3d`) to install Shap-E text/image-to-3D into ~/.dex/runtimes/mlx-venv."
            }
            Self::Ffmpeg => "Click 'Install FFmpeg (1-click)' (runs `brew install ffmpeg`).",
            Self::Blender => {
                "Click 'Install Blender (1-click)' (runs `brew install --cask blender`)."
            }
            Self::Uv => "Click 'Install uv (1-click)' (runs `brew install uv`).",
            Self::Node => "Click 'Install Node.js (1-click)' (runs `brew install node`).",
        }
    }
}

/// Discovery status of an autonomous local inference runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub id: String,
    pub name: String,
    pub kind: RuntimeKind,
    pub available: bool,
    pub binary_path: Option<String>,
    pub install_hint: String,
    pub managed_dir: String,
}

/// Active model currently mounted in Dex's autonomous local runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveLoadedModel {
    pub model_id: String,
    pub name: String,
    pub format: String,
    pub quantization: Option<String>,
    pub path: String,
    pub runtime: String,
    pub base_url: String,
    pub port: u16,
    pub sidecar_id: String,
    pub size_bytes: u64,
    pub pid: Option<u32>,
}

/// Typed outcome returned when loading a local model into Dex's native runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LoadModelOutcome {
    Loaded {
        model: ActiveLoadedModel,
        message: String,
    },
    RuntimeMissing {
        runtime: String,
        install_hint: String,
        message: String,
    },
}

/// Command launch specification constructed for a local model before handing off to `ExtensionSupervisor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLaunchSpec {
    pub runtime: RuntimeKind,
    pub command: String,
    pub args: Vec<String>,
    pub port: u16,
    pub base_url: String,
    pub healthcheck_url: String,
    pub model_identifier: String,
}

pub fn runtimes_root_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".dex")
        .join("runtimes")
}

pub fn mlx_venv_dir() -> PathBuf {
    runtimes_root_dir().join("mlx-venv")
}

pub fn llama_runtime_dir() -> PathBuf {
    runtimes_root_dir().join("llama")
}

/// Returns ordered candidate executable paths for `mlx_lm.server`.
pub fn mlx_candidate_paths(home: &Path, path_env: &str) -> Vec<PathBuf> {
    let mut candidates = vec![
        home.join(".dex/runtimes/mlx-venv/bin/mlx_lm.server"),
        home.join(".local/bin/mlx_lm.server"),
        PathBuf::from("/opt/homebrew/bin/mlx_lm.server"),
        PathBuf::from("/usr/local/bin/mlx_lm.server"),
    ];

    for dir in std::env::split_paths(path_env) {
        if !dir.as_os_str().is_empty() {
            candidates.push(dir.join("mlx_lm.server"));
        }
    }

    // Common uv, pyenv, and conda/miniforge/anaconda virtual environments
    candidates.extend([
        home.join(".local/share/uv/tools/mlx-lm/bin/mlx_lm.server"),
        home.join(".pyenv/shims/mlx_lm.server"),
        home.join("miniforge3/bin/mlx_lm.server"),
        home.join("miniconda3/bin/mlx_lm.server"),
        home.join("anaconda3/bin/mlx_lm.server"),
        PathBuf::from("/opt/homebrew/Caskroom/miniforge/base/bin/mlx_lm.server"),
        PathBuf::from("/opt/homebrew/Caskroom/miniconda/base/bin/mlx_lm.server"),
    ]);

    dedup_paths(candidates)
}

/// Returns ordered candidate executable paths for `llama-server` (`llama.cpp`).
pub fn llama_candidate_paths(home: &Path, path_env: &str) -> Vec<PathBuf> {
    let mut candidates = vec![
        home.join(".dex/runtimes/llama/llama-server"),
        home.join(".dex/runtimes/llama/bin/llama-server"),
        PathBuf::from("/opt/homebrew/bin/llama-server"),
        PathBuf::from("/usr/local/bin/llama-server"),
    ];

    for dir in std::env::split_paths(path_env) {
        if !dir.as_os_str().is_empty() {
            candidates.push(dir.join("llama-server"));
        }
    }

    dedup_paths(candidates)
}

fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        if seen.insert(p.clone()) {
            out.push(p);
        }
    }
    out
}

/// Finds the first existing regular file (or symlink to file) in `candidates`.
pub fn find_first_existing(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.is_file()).cloned()
}

fn find_binary_in_candidates(name: &str, extra: &[PathBuf]) -> Option<PathBuf> {
    let mut list: Vec<PathBuf> = extra.to_vec();
    let path_env = std::env::var("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_env) {
        if !dir.as_os_str().is_empty() {
            list.push(dir.join(name));
        }
    }
    find_first_existing(&dedup_paths(list))
}

fn check_mlx_venv_python_module(module_dir: &str) -> Option<PathBuf> {
    let venv_dir = mlx_venv_dir();
    let py = venv_dir.join("bin").join("python");
    if !py.is_file() {
        return None;
    }
    let lib_dir = venv_dir.join("lib");
    let Ok(rd) = std::fs::read_dir(&lib_dir) else {
        return None;
    };
    for entry in rd.flatten() {
        let p = entry.path().join("site-packages").join(module_dir);
        if p.exists() {
            return Some(py);
        }
    }
    None
}

/// Detects available local inference and multimodal runtimes.
pub fn detect_runtimes() -> Vec<RuntimeStatus> {
    ALL_RUNTIMES
        .iter()
        .map(|&kind| {
            let bin = detect_runtime_binary(kind);
            let managed_dir = match kind {
                RuntimeKind::LlamaCpp => llama_runtime_dir(),
                RuntimeKind::Ffmpeg | RuntimeKind::Blender | RuntimeKind::Uv | RuntimeKind::Node => {
                    PathBuf::from(if cfg!(target_arch = "aarch64") { "/opt/homebrew" } else { "/usr/local" })
                }
                _ => mlx_venv_dir(),
            };
            RuntimeStatus {
                id: kind.as_id().to_string(),
                name: kind.display_name().to_string(),
                kind,
                available: bin.is_some(),
                binary_path: bin.map(|p| p.to_string_lossy().to_string()),
                install_hint: kind.install_hint().to_string(),
                managed_dir: managed_dir.to_string_lossy().to_string(),
            }
        })
        .collect()
}

/// Detects the binary for a specific `RuntimeKind`.
pub fn detect_runtime_binary(kind: RuntimeKind) -> Option<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let path_env = std::env::var("PATH").unwrap_or_default();
    match kind {
        RuntimeKind::Mlx => find_first_existing(&mlx_candidate_paths(&home, &path_env)),
        RuntimeKind::LlamaCpp => find_first_existing(&llama_candidate_paths(&home, &path_env)),
        RuntimeKind::MlxWhisper => harness_core::find_executable_in_enriched_path("mlx_whisper"),
        RuntimeKind::Mflux => harness_core::find_executable_in_enriched_path("mflux-generate"),
        RuntimeKind::MlxAudio => harness_core::find_executable_in_enriched_path("mlx_audio.tts.generate")
            .or_else(|| check_mlx_venv_python_module("mlx_audio")),
        RuntimeKind::ParakeetMlx => harness_core::find_executable_in_enriched_path("parakeet-mlx")
            .or_else(|| check_mlx_venv_python_module("parakeet_mlx")),
        RuntimeKind::FasterWhisper => check_mlx_venv_python_module("faster_whisper"),
        RuntimeKind::AudioGen => harness_core::detect_audiogen_python(),
        RuntimeKind::Gen3d => harness_core::detect_gen3d_python(),
        RuntimeKind::Ffmpeg => harness_core::find_executable_in_enriched_path("ffmpeg"),
        RuntimeKind::Blender => harness_core::find_executable_in_enriched_path("blender").or_else(|| {
            let app = PathBuf::from("/Applications/Blender.app/Contents/MacOS/Blender");
            app.is_file().then_some(app)
        }),
        RuntimeKind::Uv => harness_core::find_executable_in_enriched_path("uvx"),
        RuntimeKind::Node => harness_core::find_executable_in_enriched_path("npx"),
    }
}

/// Builds a launch specification for an MLX folder or GGUF file using the detected runtime binary.
/// Python shim that runs `mlx_lm.server` after registering model-type aliases that the
/// installed `mlx-lm` does not know yet. Example: text-only Qwen3.5 exports declare
/// `model_type = "qwen3_5_text"`, which loads fine with the `qwen3_5` implementation but makes
/// stock `mlx_lm.server` fail with "Model type qwen3_5_text not supported".
pub const MLX_SERVER_SHIM: &str = "import sys\n\
import mlx_lm.utils as _u\n\
for _k, _v in {'qwen3_5_text': 'qwen3_5'}.items():\n    _u.MODEL_REMAPPING.setdefault(_k, _v)\n\
from mlx_lm.server import main\n\
sys.argv = ['mlx_lm.server'] + sys.argv[1:]\n\
main()\n";

/// Returns `(command, leading_args)` used to launch the MLX server. When `binary` lives in a
/// virtualenv `bin/` next to its `python`, the shim above runs through that interpreter;
/// otherwise the plain `mlx_lm.server` entry point is used.
pub fn mlx_server_command(binary: &Path, python_exists: impl Fn(&Path) -> bool) -> (String, Vec<String>) {
    if let Some(bin_dir) = binary.parent() {
        let python = bin_dir.join("python");
        if python_exists(&python) {
            return (
                python.to_string_lossy().to_string(),
                vec!["-c".to_string(), MLX_SERVER_SHIM.to_string()],
            );
        }
    }
    (binary.to_string_lossy().to_string(), Vec::new())
}

pub fn build_launch_spec(
    format: &str,
    model_path: &str,
    binary_path: &Path,
) -> Result<RuntimeLaunchSpec, ModelError> {
    let fmt = format.trim().to_lowercase();
    let bin_str = binary_path.to_string_lossy().to_string();

    if fmt == "mlx" {
        let port = DEX_MLX_PORT;
        let base_url = format!("http://127.0.0.1:{port}/v1");
        let (command, mut args) = mlx_server_command(binary_path, |p| p.exists());
        args.extend([
            "--model".to_string(),
            model_path.to_string(),
            "--host".to_string(),
            "127.0.0.1".to_string(),
            "--port".to_string(),
            port.to_string(),
        ]);
        return Ok(RuntimeLaunchSpec {
            runtime: RuntimeKind::Mlx,
            command,
            args,
            port,
            base_url: base_url.clone(),
            healthcheck_url: format!("{base_url}/models"),
            model_identifier: model_path.to_string(),
        });
    }

    if fmt == "gguf" {
        let port = DEX_LLAMA_PORT;
        let base_url = format!("http://127.0.0.1:{port}/v1");
        let model_leaf = Path::new(model_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(model_path)
            .to_string();
        return Ok(RuntimeLaunchSpec {
            runtime: RuntimeKind::LlamaCpp,
            command: bin_str,
            args: vec![
                "-m".to_string(),
                model_path.to_string(),
                "--host".to_string(),
                "127.0.0.1".to_string(),
                "--port".to_string(),
                port.to_string(),
                "-ngl".to_string(),
                "99".to_string(),
            ],
            port,
            base_url,
            healthcheck_url: format!("http://127.0.0.1:{port}/health"),
            model_identifier: model_leaf,
        });
    }

    Err(ModelError::InvalidInput {
        reason: format!(
            "Unsupported local runtime format '{format}'. Supported formats: mlx, gguf."
        ),
    })
}

fn emit_progress(
    event_bus: Option<&EventBus>,
    runtime: &str,
    stage: &str,
    message: impl Into<String>,
    done: bool,
    is_error: bool,
) {
    if let Some(bus) = event_bus {
        bus.publish(DexEvent::RuntimeProgress {
            runtime: runtime.to_string(),
            stage: stage.to_string(),
            message: message.into(),
            done,
            is_error,
        });
    }
}

async fn run_streaming_command(
    program: &Path,
    args: &[&str],
    runtime_id: &str,
    stage: &str,
    event_bus: Option<&EventBus>,
) -> Result<(), ModelError> {
    let cmd_display = format!("{} {}", program.display(), args.join(" "));
    info!(runtime = %runtime_id, cmd = %cmd_display, "Executing runtime installer step");
    emit_progress(
        event_bus,
        runtime_id,
        stage,
        format!("$ {cmd_display}"),
        false,
        false,
    );

    let mut cmd = Command::new(program);
    harness_core::apply_enriched_path(&mut cmd);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| ModelError::RuntimeFailed {
        runtime: runtime_id.to_string(),
        message: format!("Failed to spawn '{}': {e}", program.display()),
    })?;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let bus_out = event_bus.cloned();
    let bus_err = event_bus.cloned();
    let rt_out = runtime_id.to_string();
    let rt_err = runtime_id.to_string();
    let st_out = stage.to_string();
    let st_err = stage.to_string();

    let out_task = tokio::spawn(async move {
        let mut tail = Vec::new();
        if let Some(pipe) = stdout_pipe {
            let mut lines = BufReader::new(pipe).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let clean = line.trim().to_string();
                if clean.is_empty() {
                    continue;
                }
                debug!(runtime = %rt_out, stdout = %clean, "Installer stdout");
                emit_progress(bus_out.as_ref(), &rt_out, &st_out, clean.clone(), false, false);
                if tail.len() >= 25 {
                    tail.remove(0);
                }
                tail.push(clean);
            }
        }
        tail
    });

    let err_task = tokio::spawn(async move {
        let mut tail = Vec::new();
        if let Some(pipe) = stderr_pipe {
            let mut lines = BufReader::new(pipe).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let clean = line.trim().to_string();
                if clean.is_empty() {
                    continue;
                }
                debug!(runtime = %rt_err, stderr = %clean, "Installer stderr");
                emit_progress(bus_err.as_ref(), &rt_err, &st_err, clean.clone(), false, false);
                if tail.len() >= 25 {
                    tail.remove(0);
                }
                tail.push(clean);
            }
        }
        tail
    });

    let wait_res = tokio::time::timeout(
        Duration::from_secs(RUNTIME_INSTALL_TIMEOUT_SECS),
        child.wait(),
    )
    .await;

    let status = match wait_res {
        Ok(Ok(st)) => st,
        Ok(Err(e)) => {
            return Err(ModelError::RuntimeFailed {
                runtime: runtime_id.to_string(),
                message: format!("Error waiting for installer process: {e}"),
            });
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(ModelError::RuntimeFailed {
                runtime: runtime_id.to_string(),
                message: format!(
                    "Installer timed out after {RUNTIME_INSTALL_TIMEOUT_SECS}s running `{cmd_display}`"
                ),
            });
        }
    };

    let _ = out_task.await;
    let err_tail = err_task.await.unwrap_or_default();

    if !status.success() {
        let detail = if err_tail.is_empty() {
            format!("Command `{cmd_display}` failed with {status}")
        } else {
            format!(
                "Command `{cmd_display}` failed with {status}:\n{}",
                err_tail.join("\n")
            )
        };
        warn!(runtime = %runtime_id, detail = %detail, "Runtime installer step failed");
        emit_progress(event_bus, runtime_id, "error", detail.clone(), true, true);
        return Err(ModelError::RuntimeFailed {
            runtime: runtime_id.to_string(),
            message: detail,
        });
    }

    Ok(())
}

/// 1-click installer for Dex's autonomous local runtimes:
/// - `RuntimeKind::Mlx`: creates an isolated virtual environment at `~/.dex/runtimes/mlx-venv`
///   using `uv` (preferred) or `python3 -m venv` and installs `mlx-lm` without modifying system Python.
/// - `RuntimeKind::LlamaCpp`: installs `llama.cpp` via Homebrew (`brew install llama.cpp`) on macOS.
pub async fn install_runtime(
    kind: RuntimeKind,
    event_bus: Option<&EventBus>,
) -> Result<RuntimeStatus, ModelError> {
    let rt_id = kind.as_id();
    let root = runtimes_root_dir();
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|e| ModelError::Io {
            path: root.to_string_lossy().to_string(),
            source: e,
        })?;

    match kind {
        RuntimeKind::Mlx
        | RuntimeKind::MlxWhisper
        | RuntimeKind::Mflux
        | RuntimeKind::MlxAudio
        | RuntimeKind::ParakeetMlx
        | RuntimeKind::FasterWhisper
        | RuntimeKind::AudioGen
        | RuntimeKind::Gen3d => {
            let (packages, expected_bin): (Vec<&str>, &str) = match kind {
                RuntimeKind::Mlx => (vec!["mlx-lm"], "mlx_lm.server"),
                RuntimeKind::MlxWhisper => (vec!["mlx-whisper"], "mlx_whisper"),
                RuntimeKind::Mflux => (vec!["mflux"], "mflux-generate"),
                RuntimeKind::MlxAudio => (vec!["mlx-audio"], "mlx_audio"),
                RuntimeKind::ParakeetMlx => (vec!["parakeet-mlx"], "parakeet-mlx"),
                RuntimeKind::FasterWhisper => (vec!["faster-whisper"], "faster_whisper"),
                RuntimeKind::AudioGen => (
                    harness_core::AUDIOGEN_PIP_PACKAGES.to_vec(),
                    "python (torch + transformers MusicGen)",
                ),
                RuntimeKind::Gen3d => (
                    harness_core::GEN3D_PIP_PACKAGES.to_vec(),
                    "python (torch + diffusers Shap-E + trimesh)",
                ),
                RuntimeKind::LlamaCpp
                | RuntimeKind::Ffmpeg
                | RuntimeKind::Blender
                | RuntimeKind::Uv
                | RuntimeKind::Node => {
                    return Err(ModelError::RuntimeFailed {
                        runtime: rt_id.to_string(),
                        message: format!("{rt_id} is not a Python venv runtime"),
                    });
                }
            };
            let pkg_name = packages.join(" ");
            let venv_dir = mlx_venv_dir();
            let venv_str = venv_dir.to_string_lossy().to_string();
            let venv_python = venv_dir.join("bin").join("python");
            let venv_exists = tokio::fs::try_exists(&venv_python).await.unwrap_or(false);
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

            let uv_bin = find_binary_in_candidates(
                "uv",
                &[
                    home.join(".local/bin/uv"),
                    home.join(".cargo/bin/uv"),
                    PathBuf::from("/opt/homebrew/bin/uv"),
                    PathBuf::from("/usr/local/bin/uv"),
                ],
            );

            if let Some(uv) = uv_bin {
                if !venv_exists {
                    emit_progress(
                        event_bus,
                        rt_id,
                        "install",
                        format!("Using uv ({}) to create isolated ~/.dex/runtimes/mlx-venv...", uv.display()),
                        false,
                        false,
                    );
                    run_streaming_command(&uv, &["venv", &venv_str], rt_id, "install", event_bus).await?;
                }
                let venv_py_str = venv_python.to_string_lossy().to_string();
                let mut uv_args = vec!["pip", "install", "--python", venv_py_str.as_str()];
                uv_args.extend(packages.iter().copied());
                run_streaming_command(&uv, &uv_args, rt_id, "install", event_bus).await?;
            } else {
                if !venv_exists {
                    let python_bin = find_binary_in_candidates(
                        "python3",
                        &[
                            PathBuf::from("/opt/homebrew/bin/python3"),
                            PathBuf::from("/usr/local/bin/python3"),
                            PathBuf::from("/usr/bin/python3"),
                        ],
                    )
                    .ok_or_else(|| {
                        let msg = "Neither `uv` nor `python3` was found on this machine. Install Python 3.10+ or `uv` first.".to_string();
                        emit_progress(event_bus, rt_id, "error", msg.clone(), true, true);
                        ModelError::RuntimeFailed {
                            runtime: rt_id.to_string(),
                            message: msg,
                        }
                    })?;

                    emit_progress(
                        event_bus,
                        rt_id,
                        "install",
                        format!(
                            "Creating isolated virtual environment at {venv_str} via {}...",
                            python_bin.display()
                        ),
                        false,
                        false,
                    );
                    run_streaming_command(
                        &python_bin,
                        &["-m", "venv", &venv_str],
                        rt_id,
                        "install",
                        event_bus,
                    )
                    .await?;
                }

                let pip_bin = venv_dir.join("bin").join("pip");
                let mut pip_args = vec!["install", "--upgrade", "pip"];
                pip_args.extend(packages.iter().copied());
                run_streaming_command(&pip_bin, &pip_args, rt_id, "install", event_bus).await?;
            }

            if kind == RuntimeKind::AudioGen {
                let script = harness_core::ensure_audiogen_server_script()
                    .await
                    .map_err(|e| ModelError::Io {
                        path: harness_core::audiogen_server_script_path()
                            .to_string_lossy()
                            .to_string(),
                        source: e,
                    })?;
                emit_progress(
                    event_bus,
                    rt_id,
                    "install",
                    format!("Wrote music/SFX sidecar server to {}", script.display()),
                    false,
                    false,
                );
            }
            if kind == RuntimeKind::Gen3d {
                let script = harness_core::ensure_gen3d_server_script()
                    .await
                    .map_err(|e| ModelError::Io {
                        path: harness_core::gen3d_server_script_path()
                            .to_string_lossy()
                            .to_string(),
                        source: e,
                    })?;
                emit_progress(
                    event_bus,
                    rt_id,
                    "install",
                    format!(
                        "Wrote 3D sidecar server to {} (Shap-E weights download on first use, ~3.6 GB)",
                        script.display()
                    ),
                    false,
                    false,
                );
            }

            let status = detect_runtimes()
                .into_iter()
                .find(|r| r.kind == kind)
                .ok_or_else(|| ModelError::RuntimeFailed {
                    runtime: rt_id.to_string(),
                    message: format!("{pkg_name} installation finished but runtime status could not be refreshed"),
                })?;

            if !status.available {
                let msg = format!(
                    "{pkg_name} finished installing, but `{expected_bin}` was not found in {venv_str}/bin/"
                );
                emit_progress(event_bus, rt_id, "error", msg.clone(), true, true);
                return Err(ModelError::RuntimeFailed {
                    runtime: rt_id.to_string(),
                    message: msg,
                });
            }

            emit_progress(
                event_bus,
                rt_id,
                "installed",
                format!(
                    "{} ready at {}",
                    kind.display_name(),
                    status.binary_path.as_deref().unwrap_or(&venv_str)
                ),
                true,
                false,
            );
            Ok(status)
        }
        RuntimeKind::LlamaCpp
        | RuntimeKind::Ffmpeg
        | RuntimeKind::Blender
        | RuntimeKind::Uv
        | RuntimeKind::Node => install_with_homebrew(kind, event_bus).await,
    }
}

/// Installs a system tool with Homebrew (`brew install [--cask] <pkg>`) and verifies it.
async fn install_with_homebrew(
    kind: RuntimeKind,
    event_bus: Option<&EventBus>,
) -> Result<RuntimeStatus, ModelError> {
    let rt_id = kind.as_id();
    let fail = |msg: String| {
        emit_progress(event_bus, rt_id, "error", msg.clone(), true, true);
        ModelError::RuntimeFailed {
            runtime: rt_id.to_string(),
            message: msg,
        }
    };
    let (package, cask) = brew_package(kind)
        .ok_or_else(|| fail(format!("{rt_id} has no Homebrew package")))?;
    if !cfg!(target_os = "macos") {
        return Err(fail(format!(
            "1-click {} installation uses Homebrew on macOS. On this platform install `{package}` with your package manager.",
            kind.display_name()
        )));
    }
    let brew_bin = find_binary_in_candidates(
        "brew",
        &[
            PathBuf::from("/opt/homebrew/bin/brew"),
            PathBuf::from("/usr/local/bin/brew"),
        ],
    )
    .ok_or_else(|| {
        fail(
            "Homebrew is not installed. Open Terminal and paste the one-line installer from https://brew.sh (it asks for your Mac password), then press Install again."
                .to_string(),
        )
    })?;

    let mut args = vec!["install"];
    if cask {
        args.push("--cask");
    }
    args.push(package);
    emit_progress(
        event_bus,
        rt_id,
        "install",
        format!("Installing {} via Homebrew (`brew {}`)...", kind.display_name(), args.join(" ")),
        false,
        false,
    );
    run_streaming_command(&brew_bin, &args, rt_id, "install", event_bus).await?;

    let status = detect_runtimes()
        .into_iter()
        .find(|r| r.kind == kind)
        .ok_or_else(|| fail(format!("{package} installed but runtime status could not be refreshed")))?;
    if !status.available {
        return Err(fail(format!(
            "Homebrew finished, but {} was not detected in /opt/homebrew/bin, /usr/local/bin or /Applications.",
            kind.display_name()
        )));
    }
    emit_progress(
        event_bus,
        rt_id,
        "installed",
        format!(
            "{} ready at {}",
            kind.display_name(),
            status.binary_path.as_deref().unwrap_or(package)
        ),
        true,
        false,
    );
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mlx_and_llama_candidate_priority() {
        let home = Path::new("/Users/tester");
        let path_env = "/custom/bin:/usr/bin";

        let mlx = mlx_candidate_paths(home, path_env);
        assert_eq!(mlx[0], PathBuf::from("/Users/tester/.dex/runtimes/mlx-venv/bin/mlx_lm.server"));
        assert_eq!(mlx[1], PathBuf::from("/Users/tester/.local/bin/mlx_lm.server"));
        assert_eq!(mlx[2], PathBuf::from("/opt/homebrew/bin/mlx_lm.server"));
        assert!(mlx.contains(&PathBuf::from("/custom/bin/mlx_lm.server")));
        assert!(mlx.contains(&PathBuf::from("/Users/tester/.pyenv/shims/mlx_lm.server")));
        assert!(mlx.contains(&PathBuf::from("/Users/tester/miniforge3/bin/mlx_lm.server")));

        let llama = llama_candidate_paths(home, path_env);
        assert_eq!(llama[0], PathBuf::from("/Users/tester/.dex/runtimes/llama/llama-server"));
        assert_eq!(llama[2], PathBuf::from("/opt/homebrew/bin/llama-server"));
        assert!(llama.contains(&PathBuf::from("/custom/bin/llama-server")));
    }

    #[test]
    fn test_find_first_existing_prefers_managed_venv() {
        let base = std::env::temp_dir().join(format!("dex_rt_detect_test_{}", std::process::id()));
        let venv_bin_dir = base.join(".dex/runtimes/mlx-venv/bin");
        let local_bin_dir = base.join(".local/bin");
        std::fs::create_dir_all(&venv_bin_dir).expect("create venv dir");
        std::fs::create_dir_all(&local_bin_dir).expect("create local dir");

        let venv_bin = venv_bin_dir.join("mlx_lm.server");
        let local_bin = local_bin_dir.join("mlx_lm.server");
        std::fs::write(&local_bin, "#!/bin/sh\n").expect("write local");
        std::fs::write(&venv_bin, "#!/bin/sh\n").expect("write venv");

        let candidates = mlx_candidate_paths(&base, "");
        let chosen = find_first_existing(&candidates).expect("should find binary");
        assert_eq!(chosen, venv_bin);

        // Removing venv binary should fall back to ~/.local/bin/mlx_lm.server
        std::fs::remove_file(&venv_bin).expect("remove venv");
        let fallback = find_first_existing(&candidates).expect("should fall back");
        assert_eq!(fallback, local_bin);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_build_launch_spec_mlx_and_gguf() {
        let mlx_spec = build_launch_spec(
            "mlx",
            "/Users/tester/.lmstudio/models/Qwen3.8-27B-4bit",
            Path::new("/Users/tester/.dex/runtimes/mlx-venv/bin/mlx_lm.server"),
        )
        .expect("build mlx spec");
        assert_eq!(mlx_spec.runtime, RuntimeKind::Mlx);
        assert_eq!(mlx_spec.port, DEX_MLX_PORT);
        assert_eq!(mlx_spec.base_url, "http://127.0.0.1:8088/v1");
        assert_eq!(mlx_spec.healthcheck_url, "http://127.0.0.1:8088/v1/models");
        assert!(mlx_spec.args.contains(&"--host".to_string()));
        assert!(mlx_spec.args.contains(&"127.0.0.1".to_string()));
        assert!(mlx_spec.args.contains(&"8088".to_string()));

        let gguf_spec = build_launch_spec(
            "gguf",
            "/Users/tester/.dex/models/qwen3-14b-q4_k_m.gguf",
            Path::new("/opt/homebrew/bin/llama-server"),
        )
        .expect("build gguf spec");
        assert_eq!(gguf_spec.runtime, RuntimeKind::LlamaCpp);
        assert_eq!(gguf_spec.port, DEX_LLAMA_PORT);
        assert_eq!(gguf_spec.base_url, "http://127.0.0.1:8089/v1");
        assert_eq!(gguf_spec.healthcheck_url, "http://127.0.0.1:8089/health");
        assert!(gguf_spec.args.contains(&"-ngl".to_string()));
        assert!(gguf_spec.args.contains(&"99".to_string()));

        assert!(build_launch_spec("pt", "/tmp/model", Path::new("/bin/true")).is_err());
    }

    #[test]
    fn mlx_server_uses_venv_python_shim_when_available() {
        let bin = Path::new("/v/bin/mlx_lm.server");
        let (cmd, args) = mlx_server_command(bin, |_| true);
        assert_eq!(cmd, "/v/bin/python");
        assert_eq!(args[0], "-c");
        assert!(args[1].contains("'qwen3_5_text': 'qwen3_5'"));
        assert!(args[1].contains("from mlx_lm.server import main"));

        let (cmd, args) = mlx_server_command(bin, |_| false);
        assert_eq!(cmd, "/v/bin/mlx_lm.server");
        assert!(args.is_empty());
    }
}
