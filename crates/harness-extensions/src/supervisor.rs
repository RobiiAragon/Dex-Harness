use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use harness_core::{DexEvent, EventBus};
use harness_mcp::StdioTransport;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::catalog::find_catalog_recipe;
use crate::error::ExtensionError;
use crate::manifest::{ExtensionKind, ExtensionManifest, ExtensionRegistry};

/// Runtime lifecycle state of an extension or local sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionStatus {
    Running,
    Sleeping,
    Stopped,
    Error,
}

impl std::fmt::Display for ExtensionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Sleeping => write!(f, "sleeping"),
            Self::Stopped => write!(f, "stopped"),
            Self::Error => write!(f, "error"),
        }
    }
}

/// Serializable runtime view combining manifest configuration and live process state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionRuntimeInfo {
    pub manifest: ExtensionManifest,
    pub status: ExtensionStatus,
    pub pid: Option<u32>,
    pub last_used_epoch_secs: Option<u64>,
    pub last_error: Option<String>,
}

enum ManagedHandle {
    Sidecar(Child),
    McpStdio(Arc<StdioTransport>),
    McpSse,
}

struct ManagedRuntime {
    status: ExtensionStatus,
    pid: Option<u32>,
    handle: Option<ManagedHandle>,
    last_used_at: Instant,
    last_used_epoch_secs: Option<u64>,
    last_error: Option<String>,
    idle_timeout_secs: u64,
    recent_logs: Arc<Mutex<Vec<String>>>,
}

impl ManagedRuntime {
    async fn is_alive(&mut self) -> bool {
        match &mut self.handle {
            Some(ManagedHandle::Sidecar(child)) => matches!(child.try_wait(), Ok(None)),
            Some(ManagedHandle::McpStdio(transport)) => transport.is_running().await,
            Some(ManagedHandle::McpSse) => self.status == ExtensionStatus::Running,
            None => false,
        }
    }

    async fn terminate(&mut self) {
        if let Some(handle) = self.handle.take() {
            match handle {
                ManagedHandle::Sidecar(mut child) => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
                ManagedHandle::McpStdio(transport) => {
                    let _ = transport.kill().await;
                }
                ManagedHandle::McpSse => {}
            }
        }
        self.pid = None;
    }
}

/// Share of physical RAM Dex may fill with its own local models (chat, image, music, 3D…).
const MODEL_MEMORY_FRACTION: f64 = 0.80;
/// Free memory to keep on top of a model's estimate before loading it.
const MEMORY_HEADROOM_MB: u64 = 2048;

/// Share of RAM local models may use together (`DEX_MODEL_MEMORY_FRACTION`, 0.2–0.95, overrides).
fn model_memory_fraction() -> f64 {
    std::env::var("DEX_MODEL_MEMORY_FRACTION")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .map(|f| f.clamp(0.2, 0.95))
        .unwrap_or(MODEL_MEMORY_FRACTION)
}

/// `~/.dex/run/<id>.pid`: lets a later Dex process reap a sidecar orphaned by a crash or a
/// forced quit (child processes outlive their parent on macOS, holding GBs of model memory).
fn pidfile_path(id: &str) -> PathBuf {
    let safe: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    harness_core::home_dex_dir().join("run").join(format!("{safe}.pid"))
}

async fn record_pid(id: &str, pid: Option<u32>) {
    let Some(pid) = pid else { return };
    let path = pidfile_path(id);
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(e) = tokio::fs::write(&path, pid.to_string()).await {
        debug!(id, error = %e, "Could not record sidecar pid");
    }
}

async fn clear_pid(id: &str) {
    let _ = tokio::fs::remove_file(pidfile_path(id)).await;
}

/// Kills a previous run's sidecar for `id` if it was orphaned (parent is launchd) and is still
/// running the same `command`. Never touches a process another live Dex instance owns.
async fn reap_orphaned_sidecar(id: &str, command: &str) {
    let path = pidfile_path(id);
    let Ok(raw) = tokio::fs::read_to_string(&path).await else { return };
    let Ok(pid) = raw.trim().parse::<u32>() else {
        clear_pid(id).await;
        return;
    };
    let probe = tokio::time::timeout(
        Duration::from_secs(3),
        Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "ppid=,command="])
            .output(),
    )
    .await;
    let Ok(Ok(out)) = probe else { return };
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let (ppid, cmdline) = line.split_once(char::is_whitespace).unwrap_or(("", ""));
    let orphaned = ppid.trim() == "1";
    if orphaned && !command.is_empty() && cmdline.contains(command) {
        info!(id, pid, "Stopping sidecar left running by a previous Dex session");
        for signal in ["-TERM", "-KILL"] {
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                Command::new("kill").args([signal, &pid.to_string()]).status(),
            )
            .await;
            tokio::time::sleep(Duration::from_millis(800)).await;
            let alive = Command::new("ps")
                .args(["-p", &pid.to_string()])
                .stdout(Stdio::null())
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);
            if !alive {
                break;
            }
        }
    }
    clear_pid(id).await;
}

/// Supervises external MCP servers and local sidecar processes,
/// providing on-demand startup, health probing, and idle auto-unload to reclaim RAM.
pub struct ExtensionSupervisor {
    manifest_path: PathBuf,
    runtime: Arc<Mutex<HashMap<String, ManagedRuntime>>>,
    event_bus: EventBus,
    http_client: reqwest::Client,
}

impl ExtensionSupervisor {
    pub fn new(event_bus: EventBus) -> Self {
        Self::with_manifest_path(event_bus, ExtensionRegistry::default_manifest_path())
    }

    pub fn with_manifest_path(event_bus: EventBus, manifest_path: impl Into<PathBuf>) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            manifest_path: manifest_path.into(),
            runtime: Arc::new(Mutex::new(HashMap::new())),
            event_bus,
            http_client,
        }
    }

    pub fn event_bus(&self) -> EventBus {
        self.event_bus.clone()
    }

    pub async fn load_registry(&self) -> ExtensionRegistry {
        ExtensionRegistry::load_from(&self.manifest_path).await
    }

    pub async fn install_from_catalog(
        &self,
        catalog_id: &str,
    ) -> Result<ExtensionManifest, ExtensionError> {
        let mut recipe = find_catalog_recipe(catalog_id).ok_or_else(|| {
            ExtensionError::CatalogNotFound {
                id: catalog_id.to_string(),
            }
        })?;
        recipe.enabled = true;

        let mut reg = self.load_registry().await;
        reg.upsert(recipe.clone()).await?;
        Ok(recipe)
    }

    pub async fn add_custom_extension(
        &self,
        manifest: ExtensionManifest,
    ) -> Result<ExtensionManifest, ExtensionError> {
        if manifest.id.trim().is_empty() || manifest.name.trim().is_empty() {
            return Err(ExtensionError::InvalidConfig {
                id: manifest.id.clone(),
                reason: "Extension id and name cannot be empty".to_string(),
            });
        }

        let mut reg = self.load_registry().await;
        reg.upsert(manifest.clone()).await?;
        Ok(manifest)
    }

    pub async fn remove_extension(
        &self,
        id: &str,
    ) -> Result<Option<ExtensionManifest>, ExtensionError> {
        // Stop any running process before removing from registry
        let _ = self.stop_extension(id).await;
        {
            let mut rt = self.runtime.lock().await;
            rt.remove(id);
        }

        let mut reg = self.load_registry().await;
        reg.remove(id).await
    }

    pub async fn set_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> Result<ExtensionManifest, ExtensionError> {
        if !enabled {
            let _ = self.stop_extension(id).await;
        }
        let mut reg = self.load_registry().await;
        reg.set_enabled(id, enabled).await
    }

    pub async fn set_idle_timeout(
        &self,
        id: &str,
        idle_timeout_secs: u64,
    ) -> Result<ExtensionManifest, ExtensionError> {
        let mut reg = self.load_registry().await;
        let updated = reg.set_idle_timeout(id, idle_timeout_secs).await?;

        let mut rt = self.runtime.lock().await;
        if let Some(entry) = rt.get_mut(id) {
            entry.idle_timeout_secs = idle_timeout_secs;
        }
        Ok(updated)
    }

    /// Starts an extension on demand, verifying liveness/health and emitting `ExtensionStatusChanged`.
    pub async fn start_extension(&self, id: &str) -> Result<ExtensionRuntimeInfo, ExtensionError> {
        let reg = self.load_registry().await;
        let manifest = reg
            .get(id)
            .cloned()
            .ok_or_else(|| ExtensionError::NotFound { id: id.to_string() })?;

        // Check if already running and alive
        {
            let mut rt = self.runtime.lock().await;
            if let Some(entry) = rt.get_mut(id) {
                if entry.status == ExtensionStatus::Running && entry.is_alive().await {
                    let now_secs = current_epoch_secs();
                    entry.last_used_at = Instant::now();
                    entry.last_used_epoch_secs = Some(now_secs);
                    return Ok(ExtensionRuntimeInfo {
                        manifest,
                        status: entry.status,
                        pid: entry.pid,
                        last_used_epoch_secs: entry.last_used_epoch_secs,
                        last_error: entry.last_error.clone(),
                    });
                }
                // Clean up stale handle if any
                entry.terminate().await;
            }
        }

        info!(id = %manifest.id, name = %manifest.name, "Starting extension");
        let now_secs = current_epoch_secs();

        let spawned: Result<(ManagedHandle, Option<u32>), ExtensionError> = match &manifest.kind {
            ExtensionKind::LocalSidecar {
                command,
                args,
                healthcheck_url,
                env,
                ..
            } => {
                reap_orphaned_sidecar(&manifest.id, command).await;
                self.make_room_for(&manifest).await;
                let mut cmd = Command::new(command);
                harness_core::apply_enriched_path(&mut cmd);
                cmd.args(args)
                    .envs(env)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true);

                let mut child = cmd.spawn().map_err(|e| ExtensionError::ProcessSpawn {
                    id: manifest.id.clone(),
                    command: command.clone(),
                    hint: crate::error::missing_launcher_hint(command, &e),
                    source: e,
                })?;

                let pid = child.id();
                record_pid(&manifest.id, pid).await;

                // Keep draining stderr for the process lifetime (a full pipe would block the
                // sidecar) and remember the tail so startup failures are actionable.
                let stderr_tail: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
                if let Some(stderr) = child.stderr.take() {
                    let tail_ref = Arc::clone(&stderr_tail);
                    let rt_id = manifest.id.clone();
                    tokio::spawn(async move {
                        let mut lines = BufReader::new(stderr).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            let clean = line.trim().to_string();
                            if clean.is_empty() {
                                continue;
                            }
                            debug!(sidecar = %rt_id, stderr = %clean, "Sidecar stderr");
                            let mut guard = tail_ref.lock().await;
                            if guard.len() >= 12 {
                                guard.remove(0);
                            }
                            guard.push(clean);
                        }
                    });
                }

                // Brief startup check: verify the process didn't crash on launch (missing module,
                // bad args) before probing the healthcheck.
                let mut exited = None;
                for _ in 0..10 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    if let Ok(Some(status)) = child.try_wait() {
                        exited = Some(status);
                        break;
                    }
                }
                if let Some(exit_status) = exited {
                    tokio::time::sleep(Duration::from_millis(120)).await;
                    let tail = stderr_tail.lock().await.join("\n");
                    let mut reason = format!("Sidecar process exited prematurely with {exit_status}");
                    if !tail.is_empty() {
                        reason.push_str(&format!(":\n{tail}"));
                    }
                    if !manifest.install_hint.trim().is_empty() {
                        reason.push_str(&format!("\nFix: {}", manifest.install_hint.trim()));
                    }
                    self.record_start_error(&manifest, &reason).await;
                    return Err(ExtensionError::HealthcheckFailed {
                        id: manifest.id.clone(),
                        url: healthcheck_url.clone(),
                        reason,
                    });
                }

                if !healthcheck_url.trim().is_empty() {
                    match tokio::time::timeout(
                        Duration::from_secs(2),
                        self.http_client.get(healthcheck_url).send(),
                    )
                    .await
                    {
                        Ok(Ok(resp)) if resp.status().is_success() => {
                            debug!(id = %manifest.id, url = %healthcheck_url, "Sidecar healthcheck passed");
                        }
                        Ok(Ok(resp)) => {
                            debug!(
                                id = %manifest.id,
                                status = %resp.status(),
                                "Sidecar process alive; healthcheck returned non-2xx during warmup"
                            );
                        }
                        Ok(Err(e)) => {
                            debug!(
                                id = %manifest.id,
                                error = %e,
                                "Sidecar process alive; health endpoint still warming up"
                            );
                        }
                        Err(_) => {
                            debug!(id = %manifest.id, "Sidecar health probe timed out during warmup");
                        }
                    }
                }

                Ok((ManagedHandle::Sidecar(child), pid))
            }
            ExtensionKind::McpStdio { command, args, env } => {
                // `spawn_with_env` reports a bare message; check the launcher ourselves so a
                // missing `uvx`/`npx` becomes a 1-click install instead of a cryptic error.
                let launcher_missing = harness_core::find_executable_in_enriched_path(command).is_none()
                    && !std::path::Path::new(command).is_file();
                let transport = StdioTransport::spawn_with_env(command, args, env).map_err(|e| {
                    let hint = if launcher_missing {
                        crate::error::missing_launcher_hint(
                            command,
                            &std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
                        )
                    } else {
                        String::new()
                    };
                    ExtensionError::Mcp {
                        id: manifest.id.clone(),
                        message: format!("Failed to spawn MCP stdio server '{command}': {e}{hint}"),
                    }
                })?;
                let pid = transport.pid().await;
                Ok((ManagedHandle::McpStdio(Arc::new(transport)), pid))
            }
            ExtensionKind::McpSse { url } => {
                if url.trim().is_empty() {
                    return Err(ExtensionError::InvalidConfig {
                        id: manifest.id.clone(),
                        reason: "MCP SSE URL cannot be empty".to_string(),
                    });
                }
                Ok((ManagedHandle::McpSse, None))
            }
        };

        match spawned {
            Ok((handle, pid)) => {
                let entry = ManagedRuntime {
                    status: ExtensionStatus::Running,
                    pid,
                    handle: Some(handle),
                    last_used_at: Instant::now(),
                    last_used_epoch_secs: Some(now_secs),
                    last_error: None,
                    idle_timeout_secs: manifest.idle_timeout_secs,
                    recent_logs: Arc::new(Mutex::new(Vec::new())),
                };

                {
                    let mut rt = self.runtime.lock().await;
                    rt.insert(manifest.id.clone(), entry);
                }

                self.emit_status(
                    &manifest.id,
                    ExtensionStatus::Running,
                    pid,
                    manifest.estimated_ram_mb,
                );

                Ok(ExtensionRuntimeInfo {
                    manifest,
                    status: ExtensionStatus::Running,
                    pid,
                    last_used_epoch_secs: Some(now_secs),
                    last_error: None,
                })
            }
            Err(err) => {
                let err_msg = err.to_string();
                warn!(id = %manifest.id, error = %err_msg, "Extension failed to start");
                {
                    let mut rt = self.runtime.lock().await;
                    rt.insert(
                        manifest.id.clone(),
                        ManagedRuntime {
                            status: ExtensionStatus::Error,
                            pid: None,
                            handle: None,
                            last_used_at: Instant::now(),
                            last_used_epoch_secs: None,
                            last_error: Some(err_msg),
                            idle_timeout_secs: manifest.idle_timeout_secs,
                            recent_logs: Arc::new(Mutex::new(Vec::new())),
                        },
                    );
                }
                self.emit_status(&manifest.id, ExtensionStatus::Error, None, 0);
                Err(err)
            }
        }
    }

    /// Stops every process this supervisor started (call on app quit so no model keeps
    /// holding memory after Dex exits).
    pub async fn stop_all(&self) {
        let ids: Vec<String> = {
            let rt = self.runtime.lock().await;
            rt.iter()
                .filter(|(_, e)| e.handle.is_some())
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in ids {
            let mut rt = self.runtime.lock().await;
            if let Some(entry) = rt.get_mut(&id) {
                entry.terminate().await;
                entry.status = ExtensionStatus::Stopped;
            }
            drop(rt);
            clear_pid(&id).await;
        }
    }

    /// Cleanly terminates an extension and marks it `Stopped`.
    pub async fn stop_extension(&self, id: &str) -> Result<ExtensionRuntimeInfo, ExtensionError> {
        self.unload_extension(id, ExtensionStatus::Stopped).await
    }

    /// Starts a `LocalSidecar` extension and waits up to `startup_timeout` until its
    /// `healthcheck_url` returns HTTP 2xx, streaming `stderr`/`stdout` lines to `EventBus`
    /// (`DexEvent::RuntimeProgress`) and capturing the exact `stderr` tail if startup fails.
    pub async fn start_extension_wait_ready(
        &self,
        id: &str,
        startup_timeout: Duration,
    ) -> Result<ExtensionRuntimeInfo, ExtensionError> {
        let reg = self.load_registry().await;
        let manifest = reg
            .get(id)
            .cloned()
            .ok_or_else(|| ExtensionError::NotFound { id: id.to_string() })?;

        let (command, args, healthcheck_url, env) = match &manifest.kind {
            ExtensionKind::LocalSidecar {
                command,
                args,
                healthcheck_url,
                env,
                ..
            } => (
                command.clone(),
                args.clone(),
                healthcheck_url.clone(),
                env.clone(),
            ),
            _ => return self.start_extension(id).await,
        };

        // Always terminate any stale or previously mounted process for this ID before re-launching
        {
            let mut rt = self.runtime.lock().await;
            if let Some(entry) = rt.get_mut(id) {
                entry.terminate().await;
            }
        }
        reap_orphaned_sidecar(&manifest.id, &command).await;
        self.make_room_for(&manifest).await;

        info!(
            id = %manifest.id,
            command = %command,
            timeout_secs = startup_timeout.as_secs(),
            "Starting local sidecar with live readiness wait"
        );

        let mut cmd = Command::new(&command);
        cmd.args(&args)
            .envs(&env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| ExtensionError::ProcessSpawn {
            id: manifest.id.clone(),
            command: command.clone(),
            hint: crate::error::missing_launcher_hint(&command, &e),
            source: e,
        })?;

        let pid = child.id();
        record_pid(&manifest.id, pid).await;
        let log_tail: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        // Drain stdout & stderr concurrently without blocking the child process pipes
        if let Some(stderr) = child.stderr.take() {
            let tail_ref = Arc::clone(&log_tail);
            let bus_ref = self.event_bus.clone();
            let rt_id = manifest.id.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let clean = line.trim().to_string();
                    if clean.is_empty() {
                        continue;
                    }
                    debug!(sidecar = %rt_id, stderr = %clean, "Sidecar stderr");
                    bus_ref.publish(DexEvent::RuntimeProgress {
                        runtime: rt_id.clone(),
                        stage: "loading".to_string(),
                        message: clean.clone(),
                        done: false,
                        is_error: false,
                    });
                    let mut guard = tail_ref.lock().await;
                    if guard.len() >= 40 {
                        guard.remove(0);
                    }
                    guard.push(clean);
                }
            });
        }

        if let Some(stdout) = child.stdout.take() {
            let tail_ref = Arc::clone(&log_tail);
            let bus_ref = self.event_bus.clone();
            let rt_id = manifest.id.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let clean = line.trim().to_string();
                    if clean.is_empty() {
                        continue;
                    }
                    debug!(sidecar = %rt_id, stdout = %clean, "Sidecar stdout");
                    bus_ref.publish(DexEvent::RuntimeProgress {
                        runtime: rt_id.clone(),
                        stage: "loading".to_string(),
                        message: clean.clone(),
                        done: false,
                        is_error: false,
                    });
                    let mut guard = tail_ref.lock().await;
                    if guard.len() >= 40 {
                        guard.remove(0);
                    }
                    guard.push(clean);
                }
            });
        }

        let start_at = Instant::now();
        let probe_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        if !healthcheck_url.trim().is_empty() {
            loop {
                // Check if process exited prematurely
                match child.try_wait() {
                    Ok(Some(exit_status)) => {
                        // Brief yield so stderr reader task flushes remaining buffered lines
                        tokio::time::sleep(Duration::from_millis(120)).await;
                        let captured = {
                            let guard = log_tail.lock().await;
                            guard.join("\n")
                        };
                        let detail = if captured.is_empty() {
                            format!("Process exited prematurely with {exit_status}")
                        } else {
                            format!("Process exited with {exit_status}:\n{captured}")
                        };
                        self.record_start_error(&manifest, &detail).await;
                        return Err(ExtensionError::HealthcheckFailed {
                            id: manifest.id.clone(),
                            url: healthcheck_url,
                            reason: detail,
                        });
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!(id = %manifest.id, error = %e, "try_wait check failed");
                    }
                }

                if let Ok(resp) = probe_client.get(&healthcheck_url).send().await {
                    if resp.status().is_success() {
                        break;
                    }
                }

                if start_at.elapsed() >= startup_timeout {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    let captured = {
                        let guard = log_tail.lock().await;
                        guard.join("\n")
                    };
                    let detail = if captured.is_empty() {
                        format!(
                            "Timed out after {}s waiting for sidecar health endpoint '{}'",
                            startup_timeout.as_secs(),
                            healthcheck_url
                        )
                    } else {
                        format!(
                            "Timed out after {}s waiting for '{}'. Recent logs:\n{}",
                            startup_timeout.as_secs(),
                            healthcheck_url,
                            captured
                        )
                    };
                    self.record_start_error(&manifest, &detail).await;
                    return Err(ExtensionError::HealthcheckFailed {
                        id: manifest.id.clone(),
                        url: healthcheck_url,
                        reason: detail,
                    });
                }

                tokio::time::sleep(Duration::from_millis(450)).await;
            }
        }

        let now_secs = current_epoch_secs();
        {
            let mut rt = self.runtime.lock().await;
            rt.insert(
                manifest.id.clone(),
                ManagedRuntime {
                    status: ExtensionStatus::Running,
                    pid,
                    handle: Some(ManagedHandle::Sidecar(child)),
                    last_used_at: Instant::now(),
                    last_used_epoch_secs: Some(now_secs),
                    last_error: None,
                    idle_timeout_secs: manifest.idle_timeout_secs,
                    recent_logs: Arc::clone(&log_tail),
                },
            );
        }

        self.emit_status(
            &manifest.id,
            ExtensionStatus::Running,
            pid,
            manifest.estimated_ram_mb,
        );
        self.event_bus.publish(DexEvent::RuntimeProgress {
            runtime: manifest.id.clone(),
            stage: "ready".to_string(),
            message: format!("Model server ready at {healthcheck_url}"),
            done: true,
            is_error: false,
        });

        Ok(ExtensionRuntimeInfo {
            manifest,
            status: ExtensionStatus::Running,
            pid,
            last_used_epoch_secs: Some(now_secs),
            last_error: None,
        })
    }

    async fn record_start_error(&self, manifest: &ExtensionManifest, err_msg: &str) {
        {
            let mut rt = self.runtime.lock().await;
            rt.insert(
                manifest.id.clone(),
                ManagedRuntime {
                    status: ExtensionStatus::Error,
                    pid: None,
                    handle: None,
                    last_used_at: Instant::now(),
                    last_used_epoch_secs: None,
                    last_error: Some(err_msg.to_string()),
                    idle_timeout_secs: manifest.idle_timeout_secs,
                    recent_logs: Arc::new(Mutex::new(vec![err_msg.to_string()])),
                },
            );
        }
        self.emit_status(&manifest.id, ExtensionStatus::Error, None, 0);
        self.event_bus.publish(DexEvent::RuntimeProgress {
            runtime: manifest.id.clone(),
            stage: "error".to_string(),
            message: err_msg.to_string(),
            done: true,
            is_error: true,
        });
    }

    /// Cleanly unloads an idle extension to reclaim RAM and marks it `Sleeping`.
    pub async fn sleep_extension(&self, id: &str) -> Result<ExtensionRuntimeInfo, ExtensionError> {
        self.unload_extension(id, ExtensionStatus::Sleeping).await
    }

    async fn unload_extension(
        &self,
        id: &str,
        target_status: ExtensionStatus,
    ) -> Result<ExtensionRuntimeInfo, ExtensionError> {
        let reg = self.load_registry().await;
        let manifest = reg
            .get(id)
            .cloned()
            .ok_or_else(|| ExtensionError::NotFound { id: id.to_string() })?;

        let last_used = {
            let mut rt = self.runtime.lock().await;
            if let Some(entry) = rt.get_mut(id) {
                if entry.pid.is_some() {
                    clear_pid(id).await;
                }
                entry.terminate().await;
                entry.status = target_status;
                entry.last_error = None;
                entry.last_used_epoch_secs
            } else {
                rt.insert(
                    id.to_string(),
                    ManagedRuntime {
                        status: target_status,
                        pid: None,
                        handle: None,
                        last_used_at: Instant::now(),
                        last_used_epoch_secs: None,
                        last_error: None,
                        idle_timeout_secs: manifest.idle_timeout_secs,
                        recent_logs: Arc::new(Mutex::new(Vec::new())),
                    },
                );
                None
            }
        };

        info!(
            id = %manifest.id,
            status = %target_status,
            freed_ram_mb = manifest.estimated_ram_mb,
            "Unloaded extension process"
        );
        self.emit_status(&manifest.id, target_status, None, 0);

        Ok(ExtensionRuntimeInfo {
            manifest,
            status: target_status,
            pid: None,
            last_used_epoch_secs: last_used,
            last_error: None,
        })
    }

    /// Updates `last_used_at` timestamp whenever a tool belonging to `id` is invoked.
    /// Memory manager: before a local model sidecar starts, unloads the least recently used
    /// running sidecars until the new one fits — both Dex's model budget
    /// ([`MODEL_MEMORY_FRACTION`] of RAM, by manifest estimates) and the memory the OS reports as
    /// available. Unloaded models restart on demand the next time they are used, so heavy
    /// models (chat LLM, FLUX, MusicGen, 3D) can take turns on machines that cannot hold all.
    async fn make_room_for(&self, manifest: &ExtensionManifest) {
        if !matches!(manifest.kind, ExtensionKind::LocalSidecar { .. }) {
            return;
        }
        self.make_room(&manifest.id, &manifest.name, manifest.estimated_ram_mb)
            .await;
    }

    /// Frees memory for a model that is about to load (`need_mb`), unloading least recently
    /// used local sidecars. Also used by one-shot model processes (e.g. FLUX via `mflux`) that
    /// are not supervised sidecars. `for_id` is never unloaded.
    pub async fn make_room(&self, for_id: &str, for_name: &str, need_mb: u64) {
        let need = need_mb;
        if need == 0 {
            return;
        }
        let reg = self.load_registry().await;
        for _ in 0..8 {
            let snapshot = tokio::task::spawn_blocking(harness_core::HardwareProfile::detect_live_memory)
                .await
                .ok();
            let (total_mb, avail_mb) = snapshot
                .map(|s| {
                    (
                        (s.total_memory_gb * 1024.0) as u64,
                        (s.available_memory_gb * 1024.0) as u64,
                    )
                })
                .unwrap_or((0, u64::MAX));
            let mut running: Vec<(String, u64, Instant)> = {
                let rt = self.runtime.lock().await;
                rt.iter()
                    .filter(|(rid, e)| {
                        rid.as_str() != for_id
                            && e.status == ExtensionStatus::Running
                            && matches!(e.handle, Some(ManagedHandle::Sidecar(_)))
                    })
                    .map(|(rid, e)| {
                        let est = reg.get(rid).map(|m| m.estimated_ram_mb).unwrap_or(0);
                        (rid.clone(), est, e.last_used_at)
                    })
                    .collect()
            };
            let used: u64 = running.iter().map(|r| r.1).sum();
            let budget = (total_mb as f64 * model_memory_fraction()) as u64;
            let over_budget = total_mb > 0 && used + need > budget;
            let low_memory = avail_mb < need.saturating_add(MEMORY_HEADROOM_MB);
            if !(over_budget || low_memory) || running.is_empty() {
                return;
            }
            running.sort_by_key(|r| r.2);
            let (victim, freed, _) = running.remove(0);
            info!(
                unloading = %victim,
                freed_mb = freed,
                for_id = %for_id,
                need_mb = need,
                available_mb = avail_mb,
                "Unloading least recently used model to make room"
            );
            self.event_bus.publish(DexEvent::RuntimeProgress {
                runtime: victim.clone(),
                stage: "unloaded".to_string(),
                message: format!(
                    "Unloaded '{victim}' (~{} GB) to make room for {} — it reloads automatically when needed",
                    freed / 1024,
                    for_name
                ),
                done: true,
                is_error: false,
            });
            let _ = self.stop_extension(&victim).await;
            tokio::time::sleep(Duration::from_millis(600)).await;
        }
    }

    pub async fn touch_last_used(&self, id: &str) {
        let mut rt = self.runtime.lock().await;
        if let Some(entry) = rt.get_mut(id) {
            entry.last_used_at = Instant::now();
            entry.last_used_epoch_secs = Some(current_epoch_secs());
        }
    }

    /// Returns an active `StdioTransport` for an `McpStdio` extension,
    /// waking it up lazily if it is currently `Sleeping` or `Stopped`.
    pub async fn get_mcp_transport(
        &self,
        id: &str,
    ) -> Result<Arc<StdioTransport>, ExtensionError> {
        {
            let mut rt = self.runtime.lock().await;
            if let Some(entry) = rt.get_mut(id) {
                if entry.status == ExtensionStatus::Running && entry.is_alive().await {
                    if let Some(ManagedHandle::McpStdio(ref transport)) = entry.handle {
                        entry.last_used_at = Instant::now();
                        entry.last_used_epoch_secs = Some(current_epoch_secs());
                        return Ok(transport.clone());
                    }
                }
            }
        }

        // Lazy start / wake up
        self.start_extension(id).await?;

        let rt = self.runtime.lock().await;
        match rt.get(id).and_then(|e| e.handle.as_ref()) {
            Some(ManagedHandle::McpStdio(transport)) => Ok(transport.clone()),
            _ => Err(ExtensionError::InvalidConfig {
                id: id.to_string(),
                reason: "Extension is not configured as an MCP Stdio transport".to_string(),
            }),
        }
    }

    /// Checks all running extensions and puts any whose idle duration exceeds `idle_timeout_secs`
    /// into `Sleeping` mode to release RAM for local LLMs. Returns the IDs that were unloaded.
    pub async fn reap_idle_extensions(&self) -> Vec<String> {
        let candidates: Vec<String> = {
            let mut rt = self.runtime.lock().await;
            let mut to_sleep = Vec::new();
            for (id, entry) in rt.iter_mut() {
                if entry.status == ExtensionStatus::Running
                    && entry.idle_timeout_secs > 0
                    && entry.last_used_at.elapsed() >= Duration::from_secs(entry.idle_timeout_secs)
                {
                    to_sleep.push(id.clone());
                } else if entry.status == ExtensionStatus::Running && !entry.is_alive().await {
                    // Process died externally
                    entry.status = ExtensionStatus::Stopped;
                    entry.pid = None;
                    entry.handle = None;
                }
            }
            to_sleep
        };

        let mut unloaded = Vec::new();
        for id in candidates {
            info!(id = %id, "Auto-unloading idle extension to reclaim RAM");
            if self.sleep_extension(&id).await.is_ok() {
                unloaded.push(id);
            }
        }
        unloaded
    }

    /// Spawns a background Tokio task that periodically reaps idle extensions until cancelled.
    pub fn spawn_idle_reaper(
        self: &Arc<Self>,
        check_interval: Duration,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let supervisor = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        debug!("Extension idle reaper shutting down");
                        break;
                    }
                    _ = tokio::time::sleep(check_interval) => {
                        let _ = supervisor.reap_idle_extensions().await;
                    }
                }
            }
        })
    }

    /// Lists all installed extensions enriched with their current live runtime status.
    pub async fn list_with_status(&self) -> Vec<ExtensionRuntimeInfo> {
        let reg = self.load_registry().await;
        let mut rt = self.runtime.lock().await;
        let mut result = Vec::with_capacity(reg.extensions.len());

        for manifest in reg.extensions {
            let (status, pid, last_used_epoch_secs, last_error) =
                if let Some(entry) = rt.get_mut(&manifest.id) {
                    if entry.status == ExtensionStatus::Running && !entry.is_alive().await {
                        entry.status = ExtensionStatus::Stopped;
                        entry.pid = None;
                        entry.handle = None;
                    }
                    (
                        entry.status,
                        entry.pid,
                        entry.last_used_epoch_secs,
                        entry.last_error.clone(),
                    )
                } else {
                    (ExtensionStatus::Stopped, None, None, None)
                };

            result.push(ExtensionRuntimeInfo {
                manifest,
                status,
                pid,
                last_used_epoch_secs,
                last_error,
            });
        }

        result
    }

    /// Returns the most recent stdout/stderr lines captured from a supervised sidecar process.
    pub async fn recent_logs(&self, id: &str, max_lines: usize) -> Vec<String> {
        let log_arc = {
            let rt = self.runtime.lock().await;
            rt.get(id).map(|entry| Arc::clone(&entry.recent_logs))
        };
        let Some(logs) = log_arc else {
            return Vec::new();
        };
        let guard = logs.lock().await;
        let take_n = max_lines.min(guard.len());
        guard[guard.len().saturating_sub(take_n)..].to_vec()
    }

    /// Returns the current runtime status of an extension ID.
    pub async fn status_of(&self, id: &str) -> ExtensionStatus {
        let mut rt = self.runtime.lock().await;
        if let Some(entry) = rt.get_mut(id) {
            if entry.status == ExtensionStatus::Running && !entry.is_alive().await {
                entry.status = ExtensionStatus::Stopped;
                entry.pid = None;
                entry.handle = None;
            }
            entry.status
        } else {
            ExtensionStatus::Stopped
        }
    }

    fn emit_status(
        &self,
        extension_id: &str,
        status: ExtensionStatus,
        pid: Option<u32>,
        estimated_ram_mb: u64,
    ) {
        self.event_bus.publish(DexEvent::ExtensionStatusChanged {
            extension_id: extension_id.to_string(),
            status: status.to_string(),
            pid,
            estimated_ram_mb,
        });
    }
}

fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ExtensionCategory;

    #[tokio::test]
    async fn test_supervisor_auto_unload_on_idle_timeout() {
        let temp_dir =
            std::env::temp_dir().join(format!("dex_sup_idle_test_{}", std::process::id()));
        let manifest_path = temp_dir.join("extensions.json");

        let bus = EventBus::new(32);
        let mut rx = bus.subscribe();
        let supervisor = Arc::new(ExtensionSupervisor::with_manifest_path(
            bus.clone(),
            &manifest_path,
        ));

        let sidecar = ExtensionManifest {
            id: "test-sleep-sidecar".to_string(),
            name: "Sleep Sidecar".to_string(),
            category: ExtensionCategory::Custom,
            description: "Harmless sleep process to verify idle auto-unload".to_string(),
            kind: ExtensionKind::LocalSidecar {
                command: "/bin/sleep".to_string(),
                args: vec!["30".to_string()],
                port: 0,
                healthcheck_url: String::new(),
                env: HashMap::new(),
            },
            estimated_ram_mb: 512,
            idle_timeout_secs: 1,
            install_hint: String::new(),
            enabled: true,
            tags: vec!["test".to_string()],
        };

        supervisor
            .add_custom_extension(sidecar)
            .await
            .expect("add custom extension");

        // Start extension and verify Running + PID
        let info = supervisor
            .start_extension("test-sleep-sidecar")
            .await
            .expect("start sidecar");
        assert_eq!(info.status, ExtensionStatus::Running);
        assert!(info.pid.is_some());

        // Immediately reaping must NOT unload it yet
        let unloaded_early = supervisor.reap_idle_extensions().await;
        assert!(unloaded_early.is_empty());
        assert_eq!(
            supervisor.status_of("test-sleep-sidecar").await,
            ExtensionStatus::Running
        );

        // Wait past the 1s idle timeout
        tokio::time::sleep(Duration::from_millis(1150)).await;

        let unloaded = supervisor.reap_idle_extensions().await;
        assert_eq!(unloaded, vec!["test-sleep-sidecar".to_string()]);
        assert_eq!(
            supervisor.status_of("test-sleep-sidecar").await,
            ExtensionStatus::Sleeping
        );

        // Verify both Running and Sleeping events were published on EventBus
        let mut saw_running = false;
        let mut saw_sleeping = false;
        while let Ok(ev) = rx.try_recv() {
            if let DexEvent::ExtensionStatusChanged {
                extension_id,
                status,
                ..
            } = ev
            {
                if extension_id == "test-sleep-sidecar" && status == "running" {
                    saw_running = true;
                }
                if extension_id == "test-sleep-sidecar" && status == "sleeping" {
                    saw_sleeping = true;
                }
            }
        }
        assert!(saw_running, "Expected ExtensionStatusChanged(running)");
        assert!(saw_sleeping, "Expected ExtensionStatusChanged(sleeping)");

        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }
}
