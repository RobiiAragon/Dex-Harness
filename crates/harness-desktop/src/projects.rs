use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

const DIALOG_TIMEOUT_SECS: u64 = 180;
const GIT_INIT_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectEntry {
    pub id: String,
    pub name: String,
    pub path: String,
    pub last_opened_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectsStore {
    #[serde(default)]
    pub active_project_id: Option<String>,
    #[serde(default)]
    pub projects: Vec<ProjectEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectsResponse {
    pub active_project_id: Option<String>,
    pub active_workspace_dir: String,
    pub projects: Vec<ProjectEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentMeta {
    pub path: String,
    pub filename: String,
    pub size_bytes: u64,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<String>,
    #[serde(default)]
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatSession {
    pub id: String,
    pub title: String,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub messages: Vec<SessionMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSessionSummary {
    pub id: String,
    pub title: String,
    pub updated_at: u64,
    pub message_count: usize,
}

pub fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

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

pub fn slugify_id(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        format!("project-{}", now_epoch_secs())
    } else {
        trimmed
    }
}

pub fn classify_attachment_kind(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "heic" | "tiff" => "image",
        "pdf" | "docx" | "doc" | "pptx" | "xlsx" | "rtf" | "epub" => "document",
        "glb" | "gltf" | "obj" | "fbx" | "stl" | "usdz" => "mesh_3d",
        "wav" | "mp3" | "m4a" | "flac" | "ogg" | "aiff" => "audio",
        "mp4" | "mov" | "mkv" | "webm" => "video",
        "md" | "txt" | "json" | "toml" | "yaml" | "yml" | "rs" | "ts" | "tsx" | "js" | "jsx"
        | "py" | "swift" | "c" | "cpp" | "h" | "hpp" | "go" | "java" | "cs" | "css" | "html"
        | "sh" | "sql" | "xml" | "csv" => "code",
        _ => "file",
    }
}

pub async fn inspect_attachment_file(raw_path: &str) -> Result<AttachmentMeta, String> {
    let path = expand_tilde(raw_path);
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("Could not access file '{}': {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("'{}' is not a regular file", path.display()));
    }
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("attachment")
        .to_string();
    let kind = classify_attachment_kind(&path).to_string();
    Ok(AttachmentMeta {
        path: path.to_string_lossy().to_string(),
        filename,
        size_bytes: meta.len(),
        kind,
    })
}

impl ProjectsStore {
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".dex")
            .join("projects.json")
    }

    pub async fn load_or_default() -> Self {
        Self::load_from(&Self::default_path()).await
    }

    pub async fn load_from(path: &Path) -> Self {
        if !tokio::fs::try_exists(path).await.unwrap_or(false) {
            return Self::default();
        }
        match tokio::fs::read_to_string(path).await {
            Ok(raw) => serde_json::from_str::<Self>(&raw).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub async fn save(&self) -> Result<(), String> {
        self.save_to(&Self::default_path()).await
    }

    pub async fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create directory '{}': {e}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize projects store: {e}"))?;
        tokio::fs::write(path, json)
            .await
            .map_err(|e| format!("Failed to write '{}': {e}", path.display()))?;
        Ok(())
    }

    pub fn active_project(&self) -> Option<&ProjectEntry> {
        let active_id = self.active_project_id.as_deref()?;
        self.projects.iter().find(|p| p.id == active_id)
    }

    pub fn upsert_and_activate(&mut self, name: String, path: PathBuf) -> ProjectEntry {
        let normalized = path.to_string_lossy().to_string();
        let now = now_epoch_secs();

        if let Some(existing) = self.projects.iter_mut().find(|p| p.path == normalized) {
            existing.last_opened_at = now;
            if !name.trim().is_empty() {
                existing.name = name;
            }
            let entry = existing.clone();
            self.active_project_id = Some(entry.id.clone());
            self.sort_recent();
            return entry;
        }

        let base_id = slugify_id(&name);
        let mut id = base_id.clone();
        let mut suffix = 2usize;
        while self.projects.iter().any(|p| p.id == id) {
            id = format!("{base_id}-{suffix}");
            suffix += 1;
        }

        let entry = ProjectEntry {
            id: id.clone(),
            name,
            path: normalized,
            last_opened_at: now,
        };
        self.projects.push(entry.clone());
        self.active_project_id = Some(id);
        self.sort_recent();
        entry
    }

    pub fn select_by_id(&mut self, id: &str) -> Option<ProjectEntry> {
        let now = now_epoch_secs();
        let found = self.projects.iter_mut().find(|p| p.id == id)?;
        found.last_opened_at = now;
        let cloned = found.clone();
        self.active_project_id = Some(cloned.id.clone());
        self.sort_recent();
        Some(cloned)
    }

    pub fn remove_by_id(&mut self, id: &str) -> bool {
        let before = self.projects.len();
        self.projects.retain(|p| p.id != id);
        if self.active_project_id.as_deref() == Some(id) {
            self.active_project_id = self.projects.first().map(|p| p.id.clone());
        }
        self.projects.len() != before
    }

    fn sort_recent(&mut self) {
        self.projects
            .sort_by_key(|p| std::cmp::Reverse(p.last_opened_at));
    }
}

/// Creates a new project directory, initializes `<project>/.dex/sessions/`, and optionally runs `git init`.
pub async fn create_project_on_disk(
    name: &str,
    parent_dir: Option<&str>,
    init_git: bool,
) -> Result<(String, PathBuf), String> {
    let clean_name = name.trim();
    if clean_name.is_empty()
        || clean_name.contains('/')
        || clean_name.contains('\\')
        || clean_name.contains("..")
    {
        return Err("Project name must be a non-empty folder name without path separators".to_string());
    }

    let base = match parent_dir.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => expand_tilde(p),
        None => dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Projects"),
    };

    let project_path = base.join(clean_name);
    tokio::fs::create_dir_all(project_path.join(".dex").join("sessions"))
        .await
        .map_err(|e| format!("Failed to create project folder '{}': {e}", project_path.display()))?;

    if init_git && !tokio::fs::try_exists(project_path.join(".git")).await.unwrap_or(false) {
        let git_res = tokio::time::timeout(
            Duration::from_secs(GIT_INIT_TIMEOUT_SECS),
            Command::new("git")
                .arg("init")
                .current_dir(&project_path)
                .output(),
        )
        .await;
        if let Ok(Ok(out)) = git_res {
            if !out.status.success() {
                tracing::warn!(
                    path = %project_path.display(),
                    stderr = %String::from_utf8_lossy(&out.stderr),
                    "git init returned non-zero status"
                );
            }
        }
    }

    Ok((clean_name.to_string(), project_path))
}

/// Ensures `<project>/.dex/sessions/` exists inside an opened directory.
pub async fn ensure_project_dex_dirs(project_dir: &Path) -> Result<(), String> {
    if !tokio::fs::try_exists(project_dir).await.unwrap_or(false) {
        return Err(format!(
            "Project directory does not exist: '{}'",
            project_dir.display()
        ));
    }
    let sessions = project_dir.join(".dex").join("sessions");
    tokio::fs::create_dir_all(&sessions)
        .await
        .map_err(|e| format!("Failed to initialize '{}': {e}", sessions.display()))?;
    Ok(())
}

// =========================================================================
// Chat Session Persistence (`<project>/.dex/sessions/*.json` or global `~/.dex/sessions/*.json`)
// =========================================================================

pub fn global_sessions_dir() -> PathBuf {
    harness_core::home_dex_dir().join("sessions")
}

/// Resolves the sessions directory: `<project>/.dex/sessions` when a project is active,
/// or `~/.dex/sessions` globally when no project is open.
pub fn resolve_sessions_dir(active_project_dir: Option<&Path>) -> PathBuf {
    match active_project_dir {
        Some(dir) if !harness_core::is_unwritable_system_root(dir) => {
            dir.join(".dex").join("sessions")
        }
        _ => global_sessions_dir(),
    }
}

fn sanitize_session_id(session_id: &str) -> Result<String, String> {
    let trimmed = session_id.trim();
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains("..")
    {
        return Err(format!("Invalid session ID '{session_id}'"));
    }
    Ok(trimmed.to_string())
}

pub async fn list_sessions_in_dir(dir: &Path) -> Result<Vec<ChatSessionSummary>, String> {
    if !tokio::fs::try_exists(dir).await.unwrap_or(false) {
        return Ok(Vec::new());
    }

    let mut read_dir = tokio::fs::read_dir(dir)
        .await
        .map_err(|e| format!("Failed to read sessions directory '{}': {e}", dir.display()))?;

    let mut summaries = Vec::new();
    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(raw) = tokio::fs::read_to_string(&p).await {
            if let Ok(session) = serde_json::from_str::<ChatSession>(&raw) {
                summaries.push(ChatSessionSummary {
                    id: session.id,
                    title: session.title,
                    updated_at: session.updated_at,
                    message_count: session.messages.len(),
                });
            }
        }
    }

    summaries.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
    Ok(summaries)
}

pub async fn save_session_in_dir(
    dir: &Path,
    mut session: ChatSession,
) -> Result<ChatSession, String> {
    let clean_id = sanitize_session_id(&session.id)?;
    session.id = clean_id.clone();
    let now = now_epoch_secs();
    if session.created_at == 0 {
        session.created_at = now;
    }
    session.updated_at = now;

    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| format!("Failed to create sessions directory '{}': {e}", dir.display()))?;

    let target = dir.join(format!("{clean_id}.json"));
    let json = serde_json::to_string_pretty(&session)
        .map_err(|e| format!("Failed to serialize session: {e}"))?;
    tokio::fs::write(&target, json)
        .await
        .map_err(|e| format!("Failed to write session '{}': {e}", target.display()))?;

    Ok(session)
}

pub async fn load_session_in_dir(dir: &Path, session_id: &str) -> Result<ChatSession, String> {
    let clean_id = sanitize_session_id(session_id)?;
    let target = dir.join(format!("{clean_id}.json"));
    let raw = tokio::fs::read_to_string(&target)
        .await
        .map_err(|e| format!("Could not read session '{}': {e}", target.display()))?;
    serde_json::from_str::<ChatSession>(&raw)
        .map_err(|e| format!("Corrupted session JSON '{}': {e}", target.display()))
}

pub async fn rename_session_in_dir(
    dir: &Path,
    session_id: &str,
    new_title: &str,
) -> Result<ChatSession, String> {
    let clean_title = new_title.trim();
    if clean_title.is_empty() {
        return Err("Session title cannot be empty".to_string());
    }
    let mut session = load_session_in_dir(dir, session_id).await?;
    session.title = clean_title.chars().take(80).collect();
    save_session_in_dir(dir, session).await
}

pub async fn delete_session_in_dir(dir: &Path, session_id: &str) -> Result<(), String> {
    let clean_id = sanitize_session_id(session_id)?;
    let target = dir.join(format!("{clean_id}.json"));
    if tokio::fs::try_exists(&target).await.unwrap_or(false) {
        tokio::fs::remove_file(&target)
            .await
            .map_err(|e| format!("Failed to delete session '{}': {e}", target.display()))?;
    }
    Ok(())
}

// =========================================================================
// Native macOS Folder & Multi-File Pickers via `osascript`
// =========================================================================

fn sanitize_applescript_string(raw: &str) -> String {
    raw.chars()
        .filter(|&c| c != '"' && c != '\\' && c != '\n' && c != '\r' && c != '\0')
        .take(120)
        .collect()
}

pub fn build_pick_folder_applescript(title: Option<&str>) -> String {
    let prompt = title
        .map(sanitize_applescript_string)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Select Folder for Dex".to_string());
    format!(r#"POSIX path of (choose folder with prompt "{prompt}")"#)
}

pub fn build_pick_file_applescript(title: Option<&str>, extensions: Option<&[String]>) -> String {
    let prompt = title
        .map(sanitize_applescript_string)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Select File for Dex".to_string());

    let clean_exts: Vec<String> = extensions
        .unwrap_or(&[])
        .iter()
        .map(|ext| {
            ext.trim()
                .trim_start_matches('.')
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase()
        })
        .filter(|s| !s.is_empty())
        .collect();

    if clean_exts.is_empty() {
        format!(r#"POSIX path of (choose file with prompt "{prompt}")"#)
    } else {
        let type_list = clean_exts
            .iter()
            .map(|e| format!(r#""{e}""#))
            .collect::<Vec<_>>()
            .join(", ");
        format!(r#"POSIX path of (choose file with prompt "{prompt}" of type {{{type_list}}})"#)
    }
}

#[cfg(target_os = "macos")]
pub async fn pick_folder_dialog_native(title: Option<&str>) -> Result<Option<PathBuf>, String> {
    let script = build_pick_folder_applescript(title);
    let output = tokio::time::timeout(
        Duration::from_secs(DIALOG_TIMEOUT_SECS),
        Command::new("osascript").args(["-e", &script]).output(),
    )
    .await
    .map_err(|_| "Folder selection dialog timed out".to_string())?
    .map_err(|e| format!("Failed to launch native folder picker: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("-128") || stderr.to_lowercase().contains("cancel") {
            return Ok(None);
        }
        return Err(format!("Folder picker error: {}", stderr.trim()));
    }

    let picked = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if picked.is_empty() {
        return Ok(None);
    }
    let clean = picked.trim_end_matches('/');
    Ok(Some(PathBuf::from(if clean.is_empty() { "/" } else { clean })))
}

#[cfg(not(target_os = "macos"))]
pub async fn pick_folder_dialog_native(_title: Option<&str>) -> Result<Option<PathBuf>, String> {
    Err("Native osascript folder dialog is only supported on macOS".to_string())
}

pub async fn pick_folder_native() -> Result<Option<PathBuf>, String> {
    pick_folder_dialog_native(Some("Select Project Folder for Dex")).await
}

#[cfg(target_os = "macos")]
pub async fn pick_file_dialog_native(
    title: Option<&str>,
    extensions: Option<&[String]>,
) -> Result<Option<PathBuf>, String> {
    let script = build_pick_file_applescript(title, extensions);
    let output = tokio::time::timeout(
        Duration::from_secs(DIALOG_TIMEOUT_SECS),
        Command::new("osascript").args(["-e", &script]).output(),
    )
    .await
    .map_err(|_| "File selection dialog timed out".to_string())?
    .map_err(|e| format!("Failed to launch native file picker: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("-128") || stderr.to_lowercase().contains("cancel") {
            return Ok(None);
        }
        return Err(format!("File picker error: {}", stderr.trim()));
    }

    let picked = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if picked.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(picked)))
}

#[cfg(not(target_os = "macos"))]
pub async fn pick_file_dialog_native(
    _title: Option<&str>,
    _extensions: Option<&[String]>,
) -> Result<Option<PathBuf>, String> {
    Err("Native osascript file dialog is only supported on macOS".to_string())
}

#[cfg(target_os = "macos")]
pub async fn pick_attachment_files_native() -> Result<Vec<AttachmentMeta>, String> {
    let script = r#"
set chosenFiles to choose file with prompt "Attach files to Dex" with multiple selections allowed
set outLines to {}
repeat with f in chosenFiles
    set end of outLines to POSIX path of f
end repeat
set AppleScript's text item delimiters to linefeed
return outLines as text
"#;
    let output = tokio::time::timeout(
        Duration::from_secs(DIALOG_TIMEOUT_SECS),
        Command::new("osascript").args(["-e", script]).output(),
    )
    .await
    .map_err(|_| "File selection dialog timed out".to_string())?
    .map_err(|e| format!("Failed to launch native file picker: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("-128") || stderr.to_lowercase().contains("cancel") {
            return Ok(Vec::new());
        }
        return Err(format!("File picker error: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut results = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(meta) = inspect_attachment_file(trimmed).await {
            results.push(meta);
        }
    }
    Ok(results)
}

#[cfg(not(target_os = "macos"))]
pub async fn pick_attachment_files_native() -> Result<Vec<AttachmentMeta>, String> {
    Err("Native osascript file dialog is only supported on macOS".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_projects_store_persistence_and_crud() {
        let temp_root = std::env::temp_dir().join(format!("dex_projects_test_{}", std::process::id()));
        let store_path = temp_root.join("projects.json");
        let _ = tokio::fs::remove_dir_all(&temp_root).await;

        let mut store = ProjectsStore::load_from(&store_path).await;
        assert!(store.projects.is_empty());
        assert!(store.active_project_id.is_none());

        let (proj_name, proj_dir) = create_project_on_disk(
            "My Demo App",
            Some(temp_root.to_string_lossy().as_ref()),
            false,
        )
        .await
        .expect("create project dir");
        assert!(proj_dir.join(".dex/sessions").is_dir());

        let entry1 = store.upsert_and_activate(proj_name, proj_dir.clone());
        assert_eq!(entry1.id, "my-demo-app");
        assert_eq!(store.active_project_id.as_deref(), Some("my-demo-app"));
        store.save_to(&store_path).await.expect("save store");

        // Reload from disk and verify exact persistence
        let mut reloaded = ProjectsStore::load_from(&store_path).await;
        assert_eq!(reloaded.projects.len(), 1);
        assert_eq!(reloaded.active_project_id.as_deref(), Some("my-demo-app"));
        assert_eq!(reloaded.projects[0].path, proj_dir.to_string_lossy());

        // Add second project & switch back to first
        let entry2 = reloaded.upsert_and_activate("Second".to_string(), temp_root.join("second"));
        assert_eq!(reloaded.active_project_id.as_deref(), Some(entry2.id.as_str()));

        let switched = reloaded.select_by_id("my-demo-app").expect("select first");
        assert_eq!(switched.id, "my-demo-app");
        assert_eq!(reloaded.active_project_id.as_deref(), Some("my-demo-app"));

        // Remove active project -> falls back to remaining project
        assert!(reloaded.remove_by_id("my-demo-app"));
        assert_eq!(reloaded.projects.len(), 1);
        assert_eq!(reloaded.active_project_id.as_deref(), Some("second"));

        let _ = tokio::fs::remove_dir_all(&temp_root).await;
    }

    #[tokio::test]
    async fn test_chat_session_persistence() {
        let temp_proj = std::env::temp_dir().join(format!("dex_session_test_{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&temp_proj).await;
        tokio::fs::create_dir_all(&temp_proj).await.expect("mkdir");

        let session = ChatSession {
            id: "sess-001".to_string(),
            title: "Build Rust API".to_string(),
            created_at: 100,
            updated_at: 100,
            messages: vec![
                SessionMessage {
                    role: "user".to_string(),
                    content: "Create a health endpoint".to_string(),
                    attachments: vec!["/tmp/spec.pdf".to_string()],
                    timestamp: 101,
                },
                SessionMessage {
                    role: "assistant".to_string(),
                    content: "Done!".to_string(),
                    attachments: Vec::new(),
                    timestamp: 102,
                },
            ],
        };

        let dir = resolve_sessions_dir(Some(&temp_proj));
        save_session_in_dir(&dir, session.clone())
            .await
            .expect("save session");

        let list = list_sessions_in_dir(&dir).await.expect("list sessions");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "sess-001");
        assert_eq!(list[0].message_count, 2);

        let loaded = load_session_in_dir(&dir, "sess-001")
            .await
            .expect("load session");
        assert_eq!(loaded.title, "Build Rust API");
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].attachments, vec!["/tmp/spec.pdf".to_string()]);

        // Rename & delete session
        let renamed = rename_session_in_dir(&dir, "sess-001", "Refactored API")
            .await
            .expect("rename session");
        assert_eq!(renamed.title, "Refactored API");

        delete_session_in_dir(&dir, "sess-001")
            .await
            .expect("delete session");
        let after_del = list_sessions_in_dir(&dir).await.expect("list after delete");
        assert!(after_del.is_empty());

        // When no project is open (or root is `/`), sessions resolve globally to `~/.dex/sessions`
        assert_eq!(resolve_sessions_dir(None), global_sessions_dir());
        assert_eq!(resolve_sessions_dir(Some(Path::new("/"))), global_sessions_dir());

        let _ = tokio::fs::remove_dir_all(&temp_proj).await;
    }

    #[test]
    fn test_pick_dialog_applescript_builders_sanitize_inputs() {
        let folder_script = build_pick_folder_applescript(Some("Select \"Unity\"\nFolder"));
        assert!(folder_script.contains("Select UnityFolder"));
        assert!(!folder_script.contains('\n'));

        let file_script = build_pick_file_applescript(
            Some("Pick Audio"),
            Some(&[".wav".to_string(), "MP3!".to_string(), "m4a".to_string()]),
        );
        assert!(file_script.contains(r#"of type {"wav", "mp3", "m4a"}"#));

        let any_file = build_pick_file_applescript(None, None);
        assert!(!any_file.contains("of type"));
    }
}
