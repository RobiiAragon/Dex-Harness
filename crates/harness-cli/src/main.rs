use std::io::{self, Write};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand, ValueEnum};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use harness_computer::{
    BrowserClickTool, BrowserExtractDomTool, BrowserNavigateTool, BrowserScreenshotTool,
    BrowserSession, BrowserTypeTool, DesktopKeyboardTool, DesktopMouseTool, DesktopScreenshotTool,
};
use harness_core::{
    Agent, AgentEvent, AgentStep, CoreError, EventBus, HardwareProfile, Hook, HookAction,
    HookRegistry, LlmProvider, LoadSkillTool, ManageTasksTool, ProgressiveSkillLoader, ReActAgent,
    ScheduleRegistry, SpawnSubagentTool, ToolRegistry,
};
use harness_llm::{OllamaProvider, OpenAiProvider};
use harness_mcp::McpServer;
use harness_tools::{
    EditFileTool, ExecuteShellTool, FindFilesTool, GitWorktreeTool, GrepSearchTool, PtyShellTool,
    ReadFileTool, WriteFileTool,
};
use harness_3d::register_3d_tools;
use harness_audio::register_audio_tools;
use harness_docs::register_docs_tools;
use harness_extensions::{register_enabled_mcp_extensions, ExtensionSupervisor};
use harness_media::register_media_tools;
use harness_ui::{run_tui, start_web_dashboard};

mod audio_cli;
mod docs_cli;
mod ext_cli;
mod media_cli;
mod models_cli;
mod threed_cli;

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
#[command(author = "Dex-Harness Contributors")]
#[command(version = "0.3.0")]
#[command(about = "High-Performance Autonomous Agent Harness in Rust", long_about = None)]
struct Cli {
    /// LLM inference provider (ollama, openai)
    #[arg(short, long, default_value = "ollama", env = "DEX_PROVIDER")]
    provider: String,

    /// Model identifier to use (e.g. qwen2.5:7b, llama3.1:8b, gpt-4o-mini)
    #[arg(short, long, default_value = "qwen2.5:7b", env = "DEX_MODEL")]
    model: String,

    /// Execution security mode (autopilot, safe, plan)
    #[arg(long, value_enum, default_value_t = ExecutionMode::Autopilot, env = "DEX_MODE")]
    mode: ExecutionMode,

    /// Optional API base URL override (e.g. http://localhost:11434 for Ollama)
    #[arg(long, env = "DEX_API_BASE")]
    api_base: Option<String>,

    /// Enable live streaming of reasoning tokens
    #[arg(long, default_value_t = true)]
    stream: bool,

    /// Enable verbose debug output
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Execute a single autonomous objective
    Run {
        /// The objective or task for the agent to accomplish
        #[arg(required = true)]
        goal: String,
    },
    /// Launch the interactive agent REPL
    Repl,
    /// Start as a native Model Context Protocol (MCP) server over stdio
    McpServe,
    /// Launch the live interactive Terminal UI (TUI) dashboard
    Tui,
    /// Launch the real-time embedded Web dashboard
    Web {
        /// Web server port
        #[arg(short, long, default_value_t = 3000)]
        port: u16,
    },
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
    /// Audio & Voice tools: Record microphone, transcribe speech (STT), and synthesize voice (TTS)
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
    },
    /// Synthesize speech audio from text (sidecar or native macOS fallback)
    Speak {
        /// Text to speak
        text: String,
        /// Optional voice name (e.g. Monica, Samantha, alloy)
        #[arg(long)]
        voice: Option<String>,
        /// Optional output file path (.wav or .m4a)
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

/// Security guard hook enforcing Autopilot, Safe, or Plan restrictions across all sensitive tools.
struct SecurityHook {
    mode: ExecutionMode,
}

impl SecurityHook {
    fn is_modifying_or_sensitive(tool_name: &str) -> bool {
        matches!(
            tool_name,
            "write_file"
                | "edit_file"
                | "execute_shell"
                | "pty_shell"
                | "git_worktree"
                | "desktop_mouse"
                | "desktop_keyboard"
                | "browser_click"
                | "browser_type"
                | "image_generate"
                | "image_upscale"
                | "image_process"
                | "video_generate"
                | "video_extract_frames"
                | "video_edit"
                | "mesh_generate_3d"
                | "blender_convert_mesh"
                | "blender_render_preview"
                | "unity_export_asset"
        )
    }
}

#[async_trait]
impl Hook for SecurityHook {
    async fn on_event(&self, event: &AgentEvent) -> Result<HookAction, CoreError> {
        if let AgentEvent::PreToolUse {
            ref tool_name,
            ref arguments,
        } = event
        {
            if Self::is_modifying_or_sensitive(tool_name) {
                match self.mode {
                    ExecutionMode::Autopilot => Ok(HookAction::Continue),
                    ExecutionMode::Plan => {
                        let reason = format!(
                            "Execution halted: tool '{tool_name}' is blocked in Plan mode (read-only mode active)"
                        );
                        Ok(HookAction::Halt(reason))
                    }
                    ExecutionMode::Safe => {
                        println!("\n⚠️  [SAFE MODE] Permission requested to execute tool '{}'", tool_name);
                        println!("   Arguments: {}", arguments);
                        print!("   Allow execution? [y/N]: ");
                        let _ = io::stdout().flush();

                        let mut answer = String::new();
                        if io::stdin().read_line(&mut answer).is_err() {
                            return Ok(HookAction::Halt("Failed to read user input".to_string()));
                        }

                        let trimmed = answer.trim();
                        if trimmed.eq_ignore_ascii_case("y") || trimmed.eq_ignore_ascii_case("yes") {
                            Ok(HookAction::Continue)
                        } else {
                            Ok(HookAction::Halt(format!(
                                "User denied authorization to execute tool '{tool_name}'"
                            )))
                        }
                    }
                }
            } else {
                Ok(HookAction::Continue)
            }
        } else {
            Ok(HookAction::Continue)
        }
    }
}

fn build_provider(provider_type: &str, model: &str, api_base: Option<&str>) -> Result<Arc<dyn LlmProvider>> {
    match provider_type.to_lowercase().as_str() {
        "ollama" => {
            let mut p = OllamaProvider::new(model);
            if let Some(base) = api_base {
                p = p.with_base_url(base);
            }
            Ok(Arc::new(p))
        }
        "openai" => {
            let mut p = OpenAiProvider::new(model);
            if let Some(base) = api_base {
                p = p.with_base_url(base);
            }
            Ok(Arc::new(p))
        }
        "lmstudio" | "lm_studio" | "bionic" | "mlx" => {
            let mut p = OpenAiProvider::lm_studio(model);
            if let Some(base) = api_base {
                p = p.with_base_url(base);
            }
            Ok(Arc::new(p))
        }
        other => anyhow::bail!("Unsupported provider '{other}'. Supported: ollama, openai, lmstudio"),
    }
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

fn print_step(step: &AgentStep) {
    println!("\n━━━ [Step {}] ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━", step.iteration);
    if let Some(ref thought) = step.thought {
        if !thought.trim().is_empty() {
            println!("\n🧠 Thought:\n{thought}");
        }
    }

    for (call, result) in step.tool_calls.iter().zip(step.tool_results.iter()) {
        println!("\n🛠️  Tool Call: {}", call.name);
        println!("   Arguments: {}", call.arguments);
        if result.is_error {
            println!("   Status: ❌ FAILED");
        } else {
            println!("   Status: ✅ SUCCESS");
        }
        let preview = if result.output.len() > 1000 {
            format!("{}...\n[Output truncated: {} bytes total]", &result.output[..1000], result.output.len())
        } else {
            result.output.clone()
        };
        println!("   Output:\n{preview}");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize structured tracing
    let default_filter = if cli.verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
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
            let mode_str = format!("{:?}", cli.mode);
            run_tui(event_bus, &mode_str).await.context("Failed to run TUI")?;
            return Ok(());
        }
        Some(Commands::Web { port }) => {
            start_web_dashboard(event_bus, port).await.map_err(|e| anyhow::anyhow!(e))?;
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
                AudioCommands::Transcribe { path, timestamps } => {
                    audio_cli::transcribe_audio(&path, timestamps).await?;
                }
                AudioCommands::Speak { text, voice, out } => {
                    audio_cli::speak_text(&text, voice.as_deref(), out.as_deref(), event_bus)
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
                } => {
                    media_cli::imagine(
                        &prompt, negative, width, height, steps, &backend, workflow, event_bus,
                    )
                    .await?;
                }
                MediaCommands::Upscale {
                    path,
                    scale,
                    backend,
                } => {
                    media_cli::upscale(&path, scale, &backend, event_bus).await?;
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
        Some(Commands::McpServe) => {
            let mut skill_loader = ProgressiveSkillLoader::new();
            skill_loader.scan_standard_locations().await;

            let provider = build_provider(&cli.provider, &cli.model, cli.api_base.as_deref())?;
            let ext_supervisor = Arc::new(ExtensionSupervisor::new(event_bus.clone()));
            let tools = build_tools(provider, skill_loader, ext_supervisor, event_bus);

            let server = McpServer::new(tools);
            server.run_stdio().await.context("MCP stdio server terminated with error")?;
            return Ok(());
        }
        _ => {}
    }

    // Agent execution commands
    let mut skill_loader = ProgressiveSkillLoader::new();
    skill_loader.scan_standard_locations().await;

    let provider = build_provider(&cli.provider, &cli.model, cli.api_base.as_deref())
        .context("Failed to configure LLM provider")?;
    let ext_supervisor = Arc::new(ExtensionSupervisor::new(event_bus.clone()));
    let mut tools = build_tools(
        provider.clone(),
        skill_loader.clone(),
        ext_supervisor.clone(),
        event_bus.clone(),
    );

    // Dynamically discover enabled external MCP extensions as Deferred tools
    let deferred_loaded = register_enabled_mcp_extensions(ext_supervisor, &mut tools).await;
    if !deferred_loaded.is_empty() {
        info!(count = deferred_loaded.len(), "Loaded deferred MCP extension tools");
    }

    let mut hooks = HookRegistry::new();
    hooks.register(Arc::new(SecurityHook { mode: cli.mode }));

    let skills_index = skill_loader.generate_prompt_index();
    let base_system_prompt = format!(
        "You are Dex, a world-class autonomous software engineering and systems agent.\n\
         You operate with high precision, surgical file edits, computer vision perception, and safety awareness.\n\
         Mode: {:?}\n\
         {}\n\
         Track complex objectives using `manage_tasks` (.dex/tasks.json).\n\
         Delegate specialized tasks using `spawn_subagent` (Explorer, Coder, Reviewer).\n\
         You can inspect web pages with `browser_*` and take desktop screenshots with `desktop_*`.",
        cli.mode, skills_index
    );

    match cli.command {
        Some(Commands::Run { goal }) => {
            println!("🚀 Dex-Harness (Provider: {}, Model: {}, Mode: {:?})", cli.provider, cli.model, cli.mode);
            println!("🎯 Objective: {goal}");

            let observer = Arc::new(|step: &AgentStep| {
                print_step(step);
            });

            let mut agent = ReActAgent::new(provider, tools)
                .with_system_prompt(base_system_prompt)
                .with_observer(observer)
                .with_hooks(hooks)
                .with_event_bus(event_bus);

            if cli.stream {
                let stream_cb = Arc::new(|token: &str| {
                    print!("{token}");
                    let _ = io::stdout().flush();
                });
                agent = agent.with_token_callback(stream_cb);
            }

            match agent.run(&goal).await {
                Ok(answer) => {
                    println!("\n════════════════════════════════════════════════════════");
                    println!("🏁 Final Answer:\n\n{answer}");
                    println!("════════════════════════════════════════════════════════");
                }
                Err(e) => {
                    eprintln!("\n❌ Execution failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Daemon) => {
            println!("🤖 Dex-Harness Background Scheduler Daemon started...");
            let mut reg = ScheduleRegistry::load_or_default(ScheduleRegistry::DEFAULT_PATH).await;

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
                        .with_system_prompt(base_system_prompt.clone())
                        .with_hooks(hooks.clone())
                        .with_event_bus(event_bus.clone());

                    match agent.run(&goal).await {
                        Ok(ans) => {
                            info!(task_id = %id, "Scheduled task finished: {}", &ans[..ans.len().min(100)]);
                        }
                        Err(e) => {
                            error!(task_id = %id, "Scheduled task error: {e}");
                        }
                    }

                    reg.mark_executed(&id, now);
                    let _ = reg.save().await;
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            }
        }
        Some(Commands::Repl) | None => {
            println!("========================================================");
            println!("       🤖 Dex-Harness Interactive REPL (dex)            ");
            println!("  Provider: {} | Model: {} | Mode: {:?}", cli.provider, cli.model, cli.mode);
            println!("  Type 'exit' or 'quit' to leave, or enter your goal.   ");
            println!("========================================================\n");

            let observer = Arc::new(|step: &AgentStep| {
                print_step(step);
            });

            let mut agent = ReActAgent::new(provider, tools)
                .with_system_prompt(base_system_prompt)
                .with_observer(observer)
                .with_hooks(hooks)
                .with_event_bus(event_bus);

            if cli.stream {
                let stream_cb = Arc::new(|token: &str| {
                    print!("{token}");
                    let _ = io::stdout().flush();
                });
                agent = agent.with_token_callback(stream_cb);
            }

            loop {
                print!("\ndex> ");
                io::stdout().flush().context("Failed to flush stdout")?;

                let mut input = String::new();
                if io::stdin().read_line(&mut input)? == 0 {
                    break;
                }

                let trimmed = input.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed.eq_ignore_ascii_case("exit") || trimmed.eq_ignore_ascii_case("quit") {
                    println!("Goodbye!");
                    break;
                }

                match agent.run(trimmed).await {
                    Ok(answer) => {
                        println!("\n🏁 Answer:\n{answer}");
                    }
                    Err(e) => {
                        eprintln!("\n❌ Agent error: {e}");
                    }
                }
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
                SecurityHook::is_modifying_or_sensitive(tool),
                "Expected '{tool}' to be gated by SecurityHook"
            );
        }
        assert!(!SecurityHook::is_modifying_or_sensitive("video_inspect"));
        assert!(!SecurityHook::is_modifying_or_sensitive("mesh_inspect"));
        assert!(!SecurityHook::is_modifying_or_sensitive("unity_inspect_project"));
    }
}
