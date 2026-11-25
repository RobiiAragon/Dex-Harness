use std::collections::HashSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use tokio::sync::{Mutex, RwLock};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use harness_computer::{
    BrowserClickTool, BrowserExtractDomTool, BrowserNavigateTool, BrowserScreenshotTool,
    BrowserSession, BrowserTypeTool, DesktopKeyboardTool, DesktopMouseTool, DesktopScreenshotTool,
};
use harness_core::{
    Agent, EventBus, HardwareProfile, HookRegistry, LlmProvider, LoadSkillTool, ManageTasksTool,
    ProgressiveSkillLoader, ReActAgent, ScheduleRegistry, SpawnSubagentTool, ToolRegistry,
};
use harness_mcp::McpServer;
use harness_tools::{
    EditFileTool, ExecuteShellTool, FindFilesTool, GitWorktreeTool, GrepSearchTool, PtyShellTool,
    ReadFileTool, WriteFileTool,
};
use harness_3d::register_3d_tools;
use harness_audio::register_audio_tools;
use harness_docs::register_docs_tools;
use harness_extensions::{connect_enabled_mcp_extensions, ExtensionSupervisor};
use harness_media::register_media_tools;
use harness_ui::run_tui;

mod audio_cli;
mod docs_cli;
mod doctor;
mod endpoint;
mod ext_cli;
mod media_cli;
mod models_cli;
mod session;
mod threed_cli;
mod ui;

use endpoint::{CliConfig, Endpoint, EndpointRequest};
use session::{CliSecurityHook, Input, SessionParts};
use ui::{Style, StreamPrinter};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExecutionMode {
    /// Fully autonomous execution without interactive confirmation prompts
    Autopilot,
    /// Safe mode: Prompts for human confirmation before running modifying, shell, or desktop tools
    Safe,
    /// Plan mode: Strictly read-only, prohibiting modifying tools or shell commands
    Plan,
}

#[derive(Parser, Debug)]
#[command(name = "dex")]
#[command(version)]
#[command(about = "Dex — local-first AI agent for your terminal and Mac", long_about = None)]
#[command(
    after_help = "Examples:\n  dex                         start an interactive session (auto-detects LM Studio / Ollama / MLX)\n  dex \"fix the failing test\"  start a session with a first task\n  dex run \"summarize README\"  one-shot, non-interactive\n  dex doctor                  check servers, models and runtimes"
)]
struct Cli {
    /// Model server: auto (default), lmstudio, ollama, mlx, llamacpp, openai
    #[arg(short, long, env = "DEX_PROVIDER", global = true)]
    provider: Option<String>,

    /// Model id (defaults to the last one you picked with /model, else the first loaded model)
    #[arg(short, long, env = "DEX_MODEL", global = true)]
    model: Option<String>,

    /// Permission mode: safe asks before edits/shell (default), plan is read-only, autopilot never asks
    #[arg(long, value_enum, env = "DEX_MODE", global = true)]
    mode: Option<ExecutionMode>,

    /// OpenAI-compatible base URL override (e.g. http://127.0.0.1:8080/v1)
    #[arg(long, env = "DEX_API_BASE", global = true)]
    api_base: Option<String>,

    /// Disable live token streaming (print only the final answer)
    #[arg(long, global = true)]
    no_stream: bool,

    /// Maximum agent steps per task
    #[arg(long, default_value_t = 40, global = true)]
    max_steps: usize,

    /// Verbose output: debug logs and longer tool previews
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Optional first task for the interactive session
    prompt: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run one task non-interactively and print the answer
    Run {
        /// The task for the agent
        #[arg(required = true)]
        goal: String,
    },
    /// Interactive session (same as running `dex` with no arguments)
    Repl,
    /// Diagnose model servers, models, runtimes and extensions
    Doctor,
    /// Start as a native Model Context Protocol (MCP) server over stdio
    McpServe,
    /// Live terminal dashboard of agent events and tasks
    Tui,
    /// Analyze host hardware and provide local LLM model size recommendations
    Cookbook,
    /// Manage scheduled autonomous tasks (.dex/schedules.json)
    Schedule {
        #[command(subcommand)]
        action: ScheduleCommands,
    },
    /// Run background daemon executing scheduled tasks
    Daemon,
    /// Local Model Hub: Search, pull, import, and manage local models (Hugging Face GGUF / Ollama)
    Models {
        #[command(subcommand)]
        action: ModelCommands,
    },
    /// Extensions Hub: Manage external MCP servers and local creative/engineering sidecars
    Ext {
        #[command(subcommand)]
        action: ExtCommands,
    },
    /// Audio & Voice tools: record, transcribe (STT), speak (TTS), and generate music / SFX
    Audio {
        #[command(subcommand)]
        action: AudioCommands,
    },
    /// Documents & OCR tools: Native Apple Vision OCR and deep PDF/document Markdown extraction
    Docs {
        #[command(subcommand)]
        action: DocsCommands,
    },
    /// Visual Studio: Image generation, upscaling, fast processing, and declarative FFmpeg video editing
    Media {
        #[command(subcommand)]
        action: MediaCommands,
    },
    /// 3D & Game Studio: Pure-Rust glTF/OBJ inspection, image-to-3D generation, headless Blender, and Unity tools
    #[command(name = "3d")]
    ThreeD {
        #[command(subcommand)]
        action: ThreeDCommands,
    },
}

#[derive(Subcommand, Debug)]
enum ThreeDCommands {
    /// Inspect a 3D mesh (.glb, .gltf, .obj) natively in pure Rust with game-ready diagnostics
    Inspect {
        /// Path to the 3D model file
        path: String,
    },
    /// Generate a valid glTF 2.0 binary sample cube (.glb) for testing and the offline 3D viewer
    Sample {
        /// Optional output path (defaults to .dex/media/3d/sample_cube.glb)
        #[arg(long)]
        out: Option<String>,
    },
    /// Generate a 3D .glb mesh from a reference image and/or text prompt via local 3D sidecar
    Generate {
        /// Path to input image (e.g. from .dex/media/images/)
        #[arg(long)]
        image: Option<String>,
        /// Optional text prompt describing the 3D object
        #[arg(long)]
        prompt: Option<String>,
        /// Backend selector: auto, trellis, hunyuan3d, triposr, http
        #[arg(long, default_value = "auto")]
        backend: String,
    },
    /// Convert and optionally decimate a 3D mesh via headless Blender (.glb/.gltf/.fbx/.obj/.blend)
    Convert {
        /// Input 3D model path
        input: String,
        /// Target output 3D model path
        #[arg(long)]
        out: String,
        /// Optional polygon decimation ratio (0.05..=1.0) for LODs
        #[arg(long)]
        decimate: Option<f32>,
    },
    /// Render an auto-framed studio preview PNG of a 3D model via headless Blender
    Preview {
        /// Input 3D model path
        input: String,
        /// Optional output PNG path (defaults to .dex/media/3d/previews/)
        #[arg(long)]
        out: Option<String>,
    },
    /// Inspect a Unity project on disk (version, URP/HDRP/Built-in pipeline, scenes, C# scripts)
    UnityInspect {
        /// Root directory of the Unity project (containing Assets/)
        project: String,
    },
    /// Export a 3D model and companion textures safely into a Unity project's Assets/ directory
    UnityExport {
        /// Source 3D model path (.glb, .gltf, .fbx, .obj)
        mesh: String,
        /// Root directory of the Unity project
        #[arg(long)]
        project: String,
        /// Relative subdirectory inside Assets/ (default: Assets/DexGenerated)
        #[arg(long)]
        subdir: Option<String>,
        /// Overwrite existing files at destination
        #[arg(long, default_value_t = false)]
        overwrite: bool,
    },
    /// List stored 3D models in .dex/media/3d/ with file size and triangle count
    List,
}

#[derive(Subcommand, Debug)]
enum MediaCommands {
    /// Generate an image from a text prompt (MLX / ComfyUI / HTTP)
    Imagine {
        /// Text prompt describing the image
        prompt: String,
        /// Optional negative prompt
        #[arg(long)]
        negative: Option<String>,
        /// Output width in pixels
        #[arg(long, default_value_t = 1024)]
        width: u32,
        /// Output height in pixels
        #[arg(long, default_value_t = 1024)]
        height: u32,
        /// Denoising steps
        #[arg(long, default_value_t = 20)]
        steps: u32,
        /// Backend selector: auto, mlx, comfyui, http
        #[arg(long, default_value = "auto")]
        backend: String,
        /// Optional ComfyUI workflow JSON inside .dex/workflows/
        #[arg(long)]
        workflow: Option<String>,
        /// Output file path (defaults to .dex/media/images/)
        #[arg(long)]
        out: Option<String>,
    },
    /// Upscale an image (neural MLX/ComfyUI or deterministic pure-Rust Lanczos3 fallback)
    Upscale {
        /// Path to source image
        path: String,
        /// Scale multiplier (2..=8)
        #[arg(long, default_value_t = 2)]
        scale: u32,
        /// Backend selector: auto, mlx, comfyui
        #[arg(long, default_value = "auto")]
        backend: String,
        /// Output file path (defaults to .dex/media/images/)
        #[arg(long)]
        out: Option<String>,
    },
    /// Fast pure-Rust image resize and format conversion (PNG/JPG/WebP)
    Process {
        /// Path to source image
        path: String,
        /// Resize dimensions in WxH format (e.g. 512x512)
        #[arg(long)]
        resize: Option<String>,
        /// Output format: png, jpg, webp
        #[arg(long)]
        format: Option<String>,
        /// Optional output file path
        #[arg(long)]
        out: Option<String>,
    },
    /// Inspect video metadata (duration, resolution, FPS, codecs) via ffprobe
    InspectVideo {
        /// Path to video file
        path: String,
    },
    /// Extract keyframes from a video as PNG images into .dex/media/frames/
    ExtractFrames {
        /// Path to video file
        path: String,
        /// Extraction rate in frames per second
        #[arg(long, default_value_t = 1.0)]
        fps: f64,
    },
    /// Render a declarative JSON TimelineSpec safely via ffmpeg
    RenderTimeline {
        /// Path to a JSON TimelineSpec file
        timeline: String,
        /// Optional output video file path
        #[arg(long)]
        out: Option<String>,
    },
    /// List generated images and videos stored in .dex/media/{images,video}/
    List,
}

#[derive(Subcommand, Debug)]
enum AudioCommands {
    /// Record audio from the default microphone to a 16kHz mono PCM WAV file
    Record {
        /// Recording duration in seconds
        #[arg(short, long, default_value_t = 5)]
        seconds: u64,
    },
    /// Transcribe an audio/video file (.wav, .mp3, .m4a) to text
    Transcribe {
        /// Path to the audio file
        path: String,
        /// Include segment start/end timestamps
        #[arg(short, long, default_value_t = false)]
        timestamps: bool,
        /// Explicit ISO-639-1 language code (e.g. es, en, auto)
        #[arg(short, long)]
        language: Option<String>,
        /// ASR engine override (auto, mlx-whisper, parakeet-mlx, faster-whisper, whisper-cli, apple-speech)
        #[arg(long)]
        engine: Option<String>,
        /// Optional model ID override
        #[arg(long)]
        model: Option<String>,
    },
    /// Synthesize speech audio from text (mlx-audio, sidecar, or native macOS fallback)
    Speak {
        /// Text to speak
        text: String,
        /// Optional voice name or saved cloned voice profile ID (e.g. Monica, Samantha, Vivian)
        #[arg(long)]
        voice: Option<String>,
        /// Optional language code (e.g. es, en, auto)
        #[arg(short, long)]
        language: Option<String>,
        /// Optional TTS engine (auto, mlx-audio, sidecar, macos-say)
        #[arg(long)]
        engine: Option<String>,
        /// Optional TTS model ID
        #[arg(long)]
        model: Option<String>,
        /// Optional output file path (.wav or .m4a)
        #[arg(short, long)]
        out: Option<String>,
    },
    /// Generate an instrumental music clip (.wav) locally with MusicGen (auto-starts the sidecar)
    Music {
        /// Style / genre / mood / instruments / tempo prompt
        prompt: String,
        /// Target duration in seconds (2-120)
        #[arg(short, long, default_value_t = 15.0)]
        duration: f64,
        /// Optional lyrics (MusicGen is instrumental: used as mood context only)
        #[arg(long)]
        lyrics: Option<String>,
        /// Optional output .wav path
        #[arg(short, long)]
        out: Option<String>,
    },
    /// Generate a sound effect (.wav) locally (MusicGen; AudioLDM opt-in via DEX_SFX_BACKEND=audioldm)
    Sfx {
        /// Sound effect description (e.g. "glass shattering on a stone floor")
        prompt: String,
        /// Target duration in seconds (0.5-30)
        #[arg(short, long, default_value_t = 4.0)]
        duration: f64,
        /// Optional output .wav path
        #[arg(short, long)]
        out: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum DocsCommands {
    /// Run native OCR (Apple Vision on macOS / Tesseract fallback) on an image
    Ocr {
        /// Path to the image file (.png, .jpg, .webp)
        path: String,
    },
    /// Extract a PDF, DOCX, PPTX, or image document into clean Markdown
    Parse {
        /// Path to the document file
        path: String,
    },
}

#[derive(Subcommand, Debug)]
enum ExtCommands {
    /// Display the built-in catalog of MCP servers and local sidecars grouped by category
    Catalog,
    /// List installed extensions with their enabled state and runtime status
    List,
    /// Install a built-in extension recipe into ~/.dex/extensions/extensions.json
    Install {
        /// Catalog recipe ID (e.g. blender-mcp, docling-mcp, trellis-3d)
        catalog_id: String,
    },
    /// Register a custom MCP server (stdio command or SSE URL)
    AddMcp {
        /// Unique extension identifier
        #[arg(long)]
        id: String,
        /// Human-readable display name
        #[arg(long)]
        name: String,
        /// Command executable for MCP stdio transport (e.g. uvx, npx, python3)
        #[arg(long)]
        command: Option<String>,
        /// Whitespace-separated command arguments for MCP stdio transport
        #[arg(long)]
        args: Option<String>,
        /// SSE endpoint URL for MCP SSE transport
        #[arg(long)]
        url: Option<String>,
        /// Extension category (audio, image_video, three_d, ocr_docs, game_dev, custom)
        #[arg(long)]
        category: Option<String>,
        /// Short description of the extension
        #[arg(long)]
        description: Option<String>,
        /// Estimated RAM footprint in MB
        #[arg(long, default_value_t = 512)]
        ram_mb: u64,
        /// Idle auto-unload timeout in seconds (default 600)
        #[arg(long)]
        idle_timeout: Option<u64>,
    },
    /// Enable an installed extension
    Enable {
        /// Extension ID
        id: String,
    },
    /// Disable an installed extension
    Disable {
        /// Extension ID
        id: String,
    },
    /// Start an extension or sidecar process
    Start {
        /// Extension ID
        id: String,
    },
    /// Stop a running extension or sidecar process
    Stop {
        /// Extension ID
        id: String,
    },
}

#[derive(Subcommand, Debug)]
enum ModelCommands {
    /// Search GGUF models on Hugging Face Hub
    Search {
        /// Search query (e.g. qwen, llama, deepseek)
        query: String,
        /// Maximum number of repositories to return
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// Download a model from Hugging Face or Ollama
    Pull {
        /// Model identifier (e.g. Qwen/Qwen2.5-Coder-14B-Instruct-GGUF or ollama:qwen2.5:7b)
        source: String,
        /// Preferred quantization (e.g. Q4_K_M, Q5_K_M, Q8_0)
        #[arg(short, long, default_value = "Q4_K_M")]
        quant: String,
    },
    /// Import a local .gguf file or MLX model directory into Dex registry (and optionally register into Ollama)
    Import {
        /// Path to a .gguf file or MLX model directory on disk
        path: String,
        /// Alias name for the model
        #[arg(short, long)]
        name: Option<String>,
        /// Also register the imported GGUF model in the local Ollama instance
        #[arg(long, default_value_t = false)]
        to_ollama: bool,
    },
    /// List all discovered local models (GGUF, MLX, PT) across configured folders and local servers
    List,
    /// Manage local directories scanned for GGUF, MLX, and Safetensors models (~/.dex/models/config.json)
    Dirs {
        #[command(subcommand)]
        action: ModelDirCommands,
    },
    /// Remove a model from the Dex registry and disk
    Rm {
        /// Model identifier to remove (e.g. local:my-model, hf:..., or ollama:...)
        model_id: String,
    },
    /// List Dex-managed local runtimes (MLX, llama.cpp, Whisper, mflux, mlx-audio, MusicGen) and their status
    Runtimes,
    /// 1-click install of a managed runtime into ~/.dex/runtimes (e.g. audiogen, mlx, mflux, mlx-audio)
    InstallRuntime {
        /// Runtime ID (mlx, llamacpp, mlx-whisper, mflux, mlx-audio, parakeet-mlx, faster-whisper, audiogen)
        runtime: String,
    },
}

#[derive(Subcommand, Debug)]
enum ModelDirCommands {
    /// List configured model folders (including auto-detected ~/.lmstudio/models)
    List,
    /// Add an existing folder to scan for local GGUF/MLX/PT models
    Add {
        /// Folder path (supports ~/)
        path: String,
    },
    /// Remove a folder from the model scanner configuration
    Rm {
        /// Folder path to remove
        path: String,
    },
}

#[derive(Subcommand, Debug)]
enum ScheduleCommands {
    /// List all scheduled tasks
    List,
    /// Add a new scheduled task
    Add {
        /// Short task name
        #[arg(short, long)]
        name: String,
        /// Autonomous goal for the agent
        #[arg(short, long)]
        goal: String,
        /// Execution interval in seconds
        #[arg(short, long, default_value_t = 3600)]
        interval: u64,
    },
    /// Remove a scheduled task by ID
    Remove {
        /// Task ID to remove
        #[arg(required = true)]
        id: String,
    },
}

fn build_tools(
    provider: Arc<dyn LlmProvider>,
    skill_loader: ProgressiveSkillLoader,
    ext_supervisor: Arc<ExtensionSupervisor>,
    event_bus: EventBus,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    // File inspection & surgical manipulation
    registry.register(Arc::new(ReadFileTool::new()));
    registry.register(Arc::new(WriteFileTool::new()));
    registry.register(Arc::new(EditFileTool::new()));

    // Search & repository navigation
    registry.register(Arc::new(FindFilesTool::new()));
    registry.register(Arc::new(GrepSearchTool::new()));

    // Terminal execution
    registry.register(Arc::new(ExecuteShellTool::new()));
    registry.register(Arc::new(PtyShellTool::new()));

    // Git branch isolation
    registry.register(Arc::new(GitWorktreeTool::new()));

    // Project & Task graph management
    registry.register(Arc::new(
        ManageTasksTool::new(".dex/tasks.json").with_event_bus(event_bus.clone()),
    ));

    // Progressive skills
    registry.register(Arc::new(LoadSkillTool::new(skill_loader)));

    // Computer Use: Browser Automation
    let browser_session = BrowserSession::new();
    registry.register(Arc::new(BrowserNavigateTool::new(browser_session.clone())));
    registry.register(Arc::new(BrowserExtractDomTool::new(browser_session.clone())));
    registry.register(Arc::new(BrowserClickTool::new(browser_session.clone())));
    registry.register(Arc::new(BrowserTypeTool::new(browser_session.clone())));
    registry.register(Arc::new(BrowserScreenshotTool::new(browser_session)));

    // Computer Use: Desktop Automation
    registry.register(Arc::new(DesktopScreenshotTool::new()));
    registry.register(Arc::new(DesktopMouseTool::new()));
    registry.register(Arc::new(DesktopKeyboardTool::new()));

    // Subagents orchestrator (using an isolated snapshot to prevent Arc cycles)
    let master_copy = registry.snapshot();
    registry.register(Arc::new(
        SpawnSubagentTool::new(provider, master_copy).with_event_bus(event_bus.clone()),
    ));

    // Deferred Audio (STT, TTS, Music, SFX), Docs/OCR, Visual Studio (Image/Video), and 3D/GameDev tools
    register_audio_tools(
        &mut registry,
        Some(ext_supervisor.clone()),
        Some(event_bus.clone()),
    );
    register_docs_tools(
        &mut registry,
        Some(ext_supervisor.clone()),
        Some(event_bus.clone()),
    );
    register_media_tools(
        &mut registry,
        Some(ext_supervisor.clone()),
        Some(event_bus.clone()),
    );
    register_3d_tools(&mut registry, Some(ext_supervisor), Some(event_bus.clone()));

    // Dynamic deferred tool discovery (`search_tools`)
    registry.register_search_tools(Some(event_bus));

    registry
}

fn system_prompt(skills_index: &str) -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string());
    let is_git = std::path::Path::new(".git").exists();
    format!(
        "You are Dex, a local-first autonomous coding and creative agent running in the user's terminal.\n\
         Working directory: {cwd} (git repo: {is_git}). Platform: {os}/{arch}.\n\
         Guidelines:\n\
         - Reply in the same language the user writes in. Be concise; use Markdown.\n\
         - Inspect before you change: read files and search the repo instead of guessing.\n\
         - Prefer `edit_file` for surgical changes; verify with tests or commands when possible.\n\
         - If a tool is denied, do not retry it; explain or ask the user.\n\
         - Use `search_tools` to discover specialized tools (audio, image, video, 3D, OCR, MCP extensions).\n\
         - Prefer Dex's built-in tools (e.g. `image_generate`, `audio_*`, `mesh_*`, `docs_*`) over MCP extension tools; those only work while their app (Unity Editor, ComfyUI, Blender) is running.\n\
         - Track long multi-step work with `manage_tasks`; delegate focused research with `spawn_subagent`.\n\
         {skills_index}",
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
    )
}

/// Connects enabled MCP extensions in the background so they never delay the first prompt.
/// Their tools appear in `registry` (shared handle) as soon as each server answers.
fn connect_extensions_in_background(supervisor: Arc<ExtensionSupervisor>, registry: &ToolRegistry) {
    let mut shared = registry.clone();
    tokio::spawn(async move {
        let report = connect_enabled_mcp_extensions(supervisor, &mut shared).await;
        session::mark_external_tools(&report.registered_tools);
        if !report.failed.is_empty() {
            info!(failed = report.failed.len(), "Some MCP extensions are unavailable (see `dex doctor`)");
        }
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Structured tracing goes to stderr and is quiet unless --verbose / RUST_LOG.
    let default_filter = if cli.verbose { "debug" } else { "error" };
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter)),
        )
        .init();

    let event_bus = EventBus::default();

    // Check standalone utility commands first
    match cli.command {
        Some(Commands::Cookbook) => {
            let profile = HardwareProfile::detect();
            println!("{}", profile.generate_report());
            return Ok(());
        }
        Some(Commands::Tui) => {
            let mode_str = format!("{:?}", cli.mode.unwrap_or(ExecutionMode::Safe));
            run_tui(event_bus, &mode_str).await.context("Failed to run TUI")?;
            return Ok(());
        }
        Some(Commands::Doctor) => {
            doctor::run(Style::detect()).await?;
            return Ok(());
        }
        Some(Commands::Schedule { action }) => {
            let mut reg = ScheduleRegistry::load_or_default(ScheduleRegistry::DEFAULT_PATH).await;
            match action {
                ScheduleCommands::List => {
                    println!("📋 Scheduled Autonomous Tasks (.dex/schedules.json):");
                    if reg.tasks.is_empty() {
                        println!("  No tasks scheduled.");
                    } else {
                        for t in &reg.tasks {
                            println!(
                                "  - [{}] '{}' (every {}s, enabled: {}) -> {}",
                                t.id, t.name, t.interval_seconds, t.enabled, t.goal
                            );
                        }
                    }
                }
                ScheduleCommands::Add { name, goal, interval } => {
                    let id = reg.add(name, goal, interval);
                    reg.save().await.context("Failed to save schedule")?;
                    println!("✅ Added schedule task '{id}'.");
                }
                ScheduleCommands::Remove { id } => {
                    if reg.remove(&id) {
                        reg.save().await.context("Failed to save schedule")?;
                        println!("✅ Removed schedule task '{id}'.");
                    } else {
                        println!("❌ Schedule task '{id}' not found.");
                    }
                }
            }
            return Ok(());
        }
        Some(Commands::Models { action }) => {
            match action {
                ModelCommands::Search { query, limit } => {
                    models_cli::search_models(&query, limit).await?;
                }
                ModelCommands::Pull { source, quant } => {
                    models_cli::pull_model(&source, &quant).await?;
                }
                ModelCommands::Import { path, name, to_ollama } => {
                    models_cli::import_model(&path, name.as_deref(), to_ollama).await?;
                }
                ModelCommands::List => {
                    models_cli::list_models().await?;
                }
                ModelCommands::Dirs { action } => match action {
                    ModelDirCommands::List => models_cli::list_model_dirs().await?,
                    ModelDirCommands::Add { path } => models_cli::add_model_dir(&path).await?,
                    ModelDirCommands::Rm { path } => models_cli::remove_model_dir(&path).await?,
                },
                ModelCommands::Rm { model_id } => {
                    models_cli::remove_model(&model_id).await?;
                }
                ModelCommands::Runtimes => {
                    models_cli::list_runtimes();
                }
                ModelCommands::InstallRuntime { runtime } => {
                    models_cli::install_runtime_cli(&runtime, event_bus).await?;
                }
            }
            return Ok(());
        }
        Some(Commands::Ext { action }) => {
            match action {
                ExtCommands::Catalog => {
                    ext_cli::show_catalog();
                }
                ExtCommands::List => {
                    ext_cli::list_extensions(event_bus).await?;
                }
                ExtCommands::Install { catalog_id } => {
                    ext_cli::install_extension(event_bus, &catalog_id).await?;
                }
                ExtCommands::AddMcp {
                    id,
                    name,
                    command,
                    args,
                    url,
                    category,
                    description,
                    ram_mb,
                    idle_timeout,
                } => {
                    ext_cli::add_custom_mcp(
                        event_bus,
                        id,
                        name,
                        command,
                        args,
                        url,
                        category,
                        description,
                        ram_mb,
                        idle_timeout,
                    )
                    .await?;
                }
                ExtCommands::Enable { id } => {
                    ext_cli::set_extension_enabled(event_bus, &id, true).await?;
                }
                ExtCommands::Disable { id } => {
                    ext_cli::set_extension_enabled(event_bus, &id, false).await?;
                }
                ExtCommands::Start { id } => {
                    ext_cli::start_extension(event_bus, &id).await?;
                }
                ExtCommands::Stop { id } => {
                    ext_cli::stop_extension(event_bus, &id).await?;
                }
            }
            return Ok(());
        }
        Some(Commands::Audio { action }) => {
            match action {
                AudioCommands::Record { seconds } => {
                    audio_cli::record_audio(seconds, event_bus).await?;
                }
                AudioCommands::Transcribe {
                    path,
                    timestamps,
                    language,
                    engine,
                    model,
                } => {
                    audio_cli::transcribe_audio(
                        &path,
                        timestamps,
                        language.as_deref(),
                        engine.as_deref(),
                        model.as_deref(),
                    )
                    .await?;
                }
                AudioCommands::Speak {
                    text,
                    voice,
                    language,
                    engine,
                    model,
                    out,
                } => {
                    audio_cli::speak_text(
                        &text,
                        voice.as_deref(),
                        language.as_deref(),
                        engine.as_deref(),
                        model.as_deref(),
                        out.as_deref(),
                        event_bus,
                    )
                    .await?;
                }
                AudioCommands::Music {
                    prompt,
                    duration,
                    lyrics,
                    out,
                } => {
                    audio_cli::generate_music_clip(
                        &prompt,
                        lyrics.as_deref(),
                        duration,
                        out.as_deref(),
                        event_bus,
                    )
                    .await?;
                }
                AudioCommands::Sfx {
                    prompt,
                    duration,
                    out,
                } => {
                    audio_cli::generate_sfx_clip(&prompt, duration, out.as_deref(), event_bus)
                        .await?;
                }
            }
            return Ok(());
        }
        Some(Commands::Docs { action }) => {
            match action {
                DocsCommands::Ocr { path } => {
                    docs_cli::run_ocr(&path, event_bus).await?;
                }
                DocsCommands::Parse { path } => {
                    docs_cli::run_parse(&path, event_bus).await?;
                }
            }
            return Ok(());
        }
        Some(Commands::Media { action }) => {
            match action {
                MediaCommands::Imagine {
                    prompt,
                    negative,
                    width,
                    height,
                    steps,
                    backend,
                    workflow,
                    out,
                } => {
                    media_cli::imagine(
                        &prompt, negative, width, height, steps, &backend, workflow, out, event_bus,
                    )
                    .await?;
                }
                MediaCommands::Upscale {
                    path,
                    scale,
                    backend,
                    out,
                } => {
                    media_cli::upscale(&path, scale, &backend, out, event_bus).await?;
                }
                MediaCommands::Process {
                    path,
                    resize,
                    format,
                    out,
                } => {
                    media_cli::process(&path, resize.as_deref(), format, out).await?;
                }
                MediaCommands::InspectVideo { path } => {
                    media_cli::inspect_video_cli(&path).await?;
                }
                MediaCommands::ExtractFrames { path, fps } => {
                    media_cli::extract_frames_cli(&path, fps).await?;
                }
                MediaCommands::RenderTimeline { timeline, out } => {
                    media_cli::render_timeline_cli(&timeline, out.as_deref(), event_bus).await?;
                }
                MediaCommands::List => {
                    media_cli::list_media().await?;
                }
            }
            return Ok(());
        }
        Some(Commands::ThreeD { action }) => {
            match action {
                ThreeDCommands::Inspect { path } => {
                    threed_cli::inspect_model(&path).await?;
                }
                ThreeDCommands::Sample { out } => {
                    threed_cli::create_sample(out.as_deref()).await?;
                }
                ThreeDCommands::Generate {
                    image,
                    prompt,
                    backend,
                } => {
                    threed_cli::generate_model(image, prompt, &backend, event_bus).await?;
                }
                ThreeDCommands::Convert {
                    input,
                    out,
                    decimate,
                } => {
                    threed_cli::convert_model(&input, &out, decimate, event_bus).await?;
                }
                ThreeDCommands::Preview { input, out } => {
                    threed_cli::preview_model(&input, out.as_deref(), event_bus).await?;
                }
                ThreeDCommands::UnityInspect { project } => {
                    threed_cli::unity_inspect(&project).await?;
                }
                ThreeDCommands::UnityExport {
                    mesh,
                    project,
                    subdir,
                    overwrite,
                } => {
                    threed_cli::unity_export(&mesh, &project, subdir.as_deref(), overwrite)
                        .await?;
                }
                ThreeDCommands::List => {
                    threed_cli::list_models().await?;
                }
            }
            return Ok(());
        }
        _ => {}
    }

    // ── Agent commands ────────────────────────────────────────────────────────────────
    let cfg = CliConfig::load().await;
    let style = Style::detect();
    let req = EndpointRequest {
        provider: cli.provider.clone().or(cfg.provider.clone()),
        model: cli.model.clone().or(cfg.model.clone()),
        api_base: cli.api_base.clone().or(cfg.api_base.clone()),
    };
    let mode = cli
        .mode
        .or_else(|| cfg.mode.as_deref().and_then(session::parse_mode))
        .unwrap_or(ExecutionMode::Safe);

    let mut skill_loader = ProgressiveSkillLoader::new();
    skill_loader.scan_standard_locations().await;
    let skills_index = skill_loader.generate_prompt_index();
    let ext_supervisor = Arc::new(ExtensionSupervisor::new(event_bus.clone()));

    let make_tools: Arc<dyn Fn(Arc<dyn LlmProvider>) -> ToolRegistry + Send + Sync> = {
        let skill_loader = skill_loader.clone();
        let ext_supervisor = ext_supervisor.clone();
        let event_bus = event_bus.clone();
        Arc::new(move |provider: Arc<dyn LlmProvider>| {
            let tools = build_tools(
                provider,
                skill_loader.clone(),
                ext_supervisor.clone(),
                event_bus.clone(),
            );
            connect_extensions_in_background(ext_supervisor.clone(), &tools);
            tools
        })
    };

    if let Some(Commands::McpServe) = cli.command {
        // stdout carries the MCP protocol: status lines must go to stderr.
        let endpoint = match endpoint::resolve(&req, &|m| eprintln!("{m}")).await {
            Ok(ep) => ep,
            Err(e) => {
                eprintln!("dex mcp-serve: no chat model available for subagents ({e}); continuing.");
                Endpoint {
                    kind: "openai".to_string(),
                    model: req.model.clone().unwrap_or_default(),
                    base_url: Some(harness_models::LM_STUDIO_DEFAULT_URL.to_string()),
                    label: "LM Studio".to_string(),
                    local: true,
                }
            }
        };
        let tools = make_tools(endpoint.build_provider());
        let server = McpServer::new(tools);
        server.run_stdio().await.context("MCP stdio server terminated with error")?;
        return Ok(());
    }

    let status = |m: &str| eprintln!("{}", style.dim(m));
    let endpoint = match endpoint::resolve(&req, &status).await {
        Ok(ep) => ep,
        Err(e) => {
            eprintln!("{} {e}", style.red("✗"));
            std::process::exit(2);
        }
    };

    let input = Input::spawn();
    let parts = SessionParts {
        endpoint,
        system_prompt: system_prompt(&skills_index),
        event_bus: event_bus.clone(),
        mode: Arc::new(RwLock::new(mode)),
        input: input.clone(),
        stream: !cli.no_stream,
        verbose: cli.verbose,
        max_steps: cli.max_steps,
        style,
        always_allowed: Arc::new(Mutex::new(HashSet::new())),
        printer: StreamPrinter::new(style),
        make_tools,
    };

    match cli.command {
        Some(Commands::Run { goal }) => {
            let mut agent = parts.build_agent();
            eprintln!(
                "{}",
                style.dim(&format!(
                    "dex · {} ({}) · {}",
                    parts.endpoint.model,
                    parts.endpoint.label,
                    session::mode_label(mode)
                ))
            );
            let ok = session::run_turn(&mut agent, &parts, &goal).await;
            if !ok {
                std::process::exit(130);
            }
        }
        Some(Commands::Daemon) => {
            println!("Dex scheduler daemon started ({}).", parts.endpoint.model);
            let mut reg = ScheduleRegistry::load_or_default(ScheduleRegistry::DEFAULT_PATH).await;
            let provider = parts.endpoint.build_provider();
            let tools = (parts.make_tools)(provider.clone());
            // Nobody can answer prompts in a daemon: Safe-mode tools stop the task (use --mode autopilot).
            let mut hooks = HookRegistry::new();
            hooks.register(Arc::new(CliSecurityHook {
                mode: parts.mode.clone(),
                input: None,
                always_allowed: parts.always_allowed.clone(),
                style,
            }));

            loop {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                let due_tasks: Vec<(String, String)> = reg
                    .tasks
                    .iter()
                    .filter(|t| t.should_run(now))
                    .map(|t| (t.id.clone(), t.goal.clone()))
                    .collect();

                for (id, goal) in due_tasks {
                    info!(task_id = %id, goal = %goal, "Executing scheduled autonomous task");
                    let mut agent = ReActAgent::new(provider.clone(), tools.clone())
                        .with_system_prompt(parts.system_prompt.clone())
                        .with_hooks(hooks.clone())
                        .with_max_steps(parts.max_steps)
                        .with_event_bus(event_bus.clone());

                    match agent.run(&goal).await {
                        Ok(ans) => {
                            let preview: String = ans.chars().take(100).collect();
                            info!(task_id = %id, "Scheduled task finished: {preview}");
                        }
                        Err(e) => {
                            error!(task_id = %id, "Scheduled task error: {e}");
                        }
                    }

                    reg.mark_executed(&id, now);
                    if let Err(e) = reg.save().await {
                        error!(task_id = %id, "Failed to persist schedule: {e}");
                    }
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            }
        }
        Some(Commands::Repl) | None => {
            if !input.interactive {
                // Piped stdin (`echo task | dex`): run it once instead of an interactive loop.
                let mut goal = String::new();
                while let Some(line) = input.next_line().await {
                    goal.push_str(&line);
                    goal.push('\n');
                }
                let goal = match cli.prompt {
                    Some(p) if goal.trim().is_empty() => p,
                    Some(p) => format!("{p}\n\n{goal}"),
                    None => goal,
                };
                if goal.trim().is_empty() {
                    eprintln!("Nothing to do: pass a task, e.g. `dex run \"...\"`.");
                    std::process::exit(2);
                }
                let mut agent = parts.build_agent();
                session::run_turn(&mut agent, &parts, goal.trim()).await;
            } else {
                session::repl(parts, cli.prompt).await?;
            }
        }
        _ => {}
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn test_cli_command_structure_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn bare_prompt_and_flags_parse() {
        let cli = Cli::try_parse_from(["dex", "hola mundo"]).expect("parse");
        assert_eq!(cli.prompt.as_deref(), Some("hola mundo"));
        assert!(cli.command.is_none());
        let cli = Cli::try_parse_from(["dex", "--mode", "plan", "run", "x"]).expect("parse");
        assert_eq!(cli.mode, Some(ExecutionMode::Plan));
        assert!(matches!(cli.command, Some(Commands::Run { .. })));
        // The old `web` subcommand is gone: the word is now just a prompt.
        let cli = Cli::try_parse_from(["dex", "web"]).expect("parse");
        assert_eq!(cli.prompt.as_deref(), Some("web"));
    }

    #[test]
    fn test_security_hook_covers_disk_writing_media_tools() {
        for tool in [
            "image_generate",
            "image_upscale",
            "image_process",
            "video_generate",
            "video_extract_frames",
            "video_edit",
            "mesh_generate_3d",
            "blender_convert_mesh",
            "blender_render_preview",
            "unity_export_asset",
        ] {
            assert!(
                session::is_sensitive_tool(tool),
                "Expected '{tool}' to be gated by SecurityHook"
            );
        }
        assert!(!session::is_sensitive_tool("video_inspect"));
        assert!(!session::is_sensitive_tool("mesh_inspect"));
        assert!(!session::is_sensitive_tool("unity_inspect_project"));
    }
}
