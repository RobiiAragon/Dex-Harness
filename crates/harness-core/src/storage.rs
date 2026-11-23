use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

static STORAGE_ROOT_OVERRIDE: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

fn override_slot() -> &'static RwLock<Option<PathBuf>> {
    STORAGE_ROOT_OVERRIDE.get_or_init(|| RwLock::new(None))
}

/// Returns the user's global `~/.dex` directory (falling back to `$TMPDIR/dex` if `$HOME` is unavailable).
pub fn home_dex_dir() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".dex"))
        .unwrap_or_else(|| std::env::temp_dir().join("dex"))
}

/// Returns true if `path` is a known read-only macOS/Unix system root (such as `/` inherited by `.app` bundles).
pub fn is_unwritable_system_root(path: &Path) -> bool {
    if path.as_os_str().is_empty() {
        return true;
    }
    if path == Path::new("/") {
        return true;
    }
    let s = path.to_string_lossy();
    s == "/System"
        || s.starts_with("/System/")
        || s == "/Applications"
        || s.contains(".app/Contents")
}

/// Configures the process-wide default Dex storage root (typically `<active_project>/.dex` or `~/.dex`).
/// Calling this in `Dex.app` guarantees all crates resolve `.dex/...` artifacts into a writable directory
/// even when macOS launches the `.app` bundle with `cwd = /`.
pub fn set_default_storage_root(root: impl Into<PathBuf>) {
    let candidate = root.into();
    let safe_root = if is_unwritable_system_root(&candidate) {
        home_dex_dir()
    } else if candidate.file_name().and_then(|n| n.to_str()) == Some(".dex") {
        candidate
    } else {
        candidate.join(".dex")
    };
    if let Ok(mut guard) = override_slot().write() {
        *guard = Some(safe_root);
    }
}

/// Clears any process-wide storage root override (primarily useful for tests).
pub fn clear_default_storage_root() {
    if let Ok(mut guard) = override_slot().write() {
        *guard = None;
    }
}

/// Pure resolver choosing the writable `.dex` storage root from:
/// 1. Explicit `preferred_workspace` (if writable)
/// 2. Process-wide override (`set_default_storage_root`)
/// 3. `DEX_STORAGE_ROOT` environment variable
/// 4. Process `cwd` (ONLY when not `/` or read-only bundle path)
/// 5. Global `~/.dex` fallback
pub fn resolve_dex_storage_root(
    preferred_workspace: Option<&Path>,
    cwd: Option<&Path>,
) -> PathBuf {
    if let Some(ws) = preferred_workspace {
        if !is_unwritable_system_root(ws) {
            return if ws.file_name().and_then(|n| n.to_str()) == Some(".dex") {
                ws.to_path_buf()
            } else {
                ws.join(".dex")
            };
        }
    }

    if let Ok(guard) = override_slot().read() {
        if let Some(ref ov) = *guard {
            if !is_unwritable_system_root(ov) {
                return ov.clone();
            }
        }
    }

    if let Ok(env_root) = std::env::var("DEX_STORAGE_ROOT") {
        let trimmed = env_root.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed);
            if !is_unwritable_system_root(&p) {
                return p;
            }
        }
    }

    if let Some(c) = cwd {
        if !is_unwritable_system_root(c) {
            return c.join(".dex");
        }
    }

    home_dex_dir()
}

/// Returns the active writable `.dex` storage root for the current process.
pub fn dex_storage_root() -> PathBuf {
    let cwd = std::env::current_dir().ok();
    resolve_dex_storage_root(None, cwd.as_deref())
}

/// Resolves a `.dex` relative subpath (accepting either `"media/images"` or `".dex/media/images"`)
/// onto the active writable storage root (`<project>/.dex/...` or `~/.dex/...`), never under `/`.
pub fn resolve_dex_subpath(subpath: impl AsRef<Path>) -> PathBuf {
    let raw = subpath.as_ref();
    if raw.is_absolute() {
        return raw.to_path_buf();
    }
    let stripped = raw.strip_prefix(".dex").unwrap_or(raw);
    dex_storage_root().join(stripped)
}

/// Resolves an arbitrary path safely:
/// - Absolute paths are returned verbatim (honoring caller-specified `output_path`).
/// - Relative `.dex/...` paths are anchored to `dex_storage_root()`.
/// - Other relative paths fall back to `dex_storage_root()` when `cwd` is `/` or read-only.
pub fn resolve_safe_path(raw: impl AsRef<Path>) -> PathBuf {
    let p = raw.as_ref();
    if p.is_absolute() {
        return p.to_path_buf();
    }
    if p.starts_with(".dex") {
        return resolve_dex_subpath(p);
    }
    let cwd = std::env::current_dir().ok();
    match cwd {
        Some(ref c) if !is_unwritable_system_root(c) => p.to_path_buf(),
        _ => dex_storage_root().join(p),
    }
}

/// Builds a deduplicated list of executable search directories suitable for macOS `.app` bundles,
/// ensuring Homebrew, Cargo, local user binaries, Dex's managed MLX venv, and Blender are always discoverable.
pub fn enriched_path_entries(home: &Path, existing_path: &str) -> Vec<PathBuf> {
    let mut ordered = vec![
        home.join(".dex/runtimes/mlx-venv/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".local/bin"),
        home.join(".cargo/bin"),
    ];

    #[cfg(target_os = "macos")]
    {
        ordered.push(PathBuf::from("/Applications/Blender.app/Contents/MacOS"));
        ordered.push(home.join("Applications/Blender.app/Contents/MacOS"));
    }

    for dir in std::env::split_paths(existing_path) {
        if !dir.as_os_str().is_empty() {
            ordered.push(dir);
        }
    }

    let mut seen = HashSet::new();
    let mut deduped = Vec::with_capacity(ordered.len());
    for dir in ordered {
        if seen.insert(dir.clone()) {
            deduped.push(dir);
        }
    }
    deduped
}

/// Returns an enriched `$PATH` string including Homebrew, Cargo, `~/.local/bin`, and `~/.dex/runtimes/mlx-venv/bin`.
pub fn enriched_path_env() -> String {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let existing = std::env::var("PATH").unwrap_or_default();
    let entries = enriched_path_entries(&home, &existing);
    std::env::join_paths(&entries)
        .map(|os| os.to_string_lossy().to_string())
        .unwrap_or_else(|_| {
            entries
                .iter()
                .map(|p| p.to_string_lossy())
                .collect::<Vec<_>>()
                .join(":")
        })
}

/// Applies the enriched `$PATH` environment variable onto an async `tokio::process::Command`.
pub fn apply_enriched_path(cmd: &mut tokio::process::Command) {
    cmd.env("PATH", enriched_path_env());
}

/// Applies the enriched `$PATH` environment variable onto a synchronous `std::process::Command`.
pub fn apply_enriched_path_std(cmd: &mut std::process::Command) {
    cmd.env("PATH", enriched_path_env());
}

/// Searches for an executable file by name across the enriched `$PATH` directories without spawning `which`.
pub fn find_executable_in_enriched_path(binary: &str) -> Option<PathBuf> {
    let raw = Path::new(binary);
    if raw.is_absolute() && raw.is_file() {
        return Some(raw.to_path_buf());
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let existing = std::env::var("PATH").unwrap_or_default();
    for dir in enriched_path_entries(&home, &existing) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_storage_root_when_cwd_is_slash() {
        clear_default_storage_root();
        let resolved = resolve_dex_storage_root(None, Some(Path::new("/")));
        assert_ne!(resolved, PathBuf::from("/.dex"));
        assert!(
            resolved.ends_with(".dex") || resolved.ends_with("dex"),
            "Expected fallback to ~/.dex, got {:?}",
            resolved
        );

        // macOS .app bundle contents path must also fall back to ~/.dex
        let from_bundle = resolve_dex_storage_root(
            Some(Path::new("/Applications/Dex.app/Contents/MacOS")),
            Some(Path::new("/")),
        );
        assert_eq!(from_bundle, home_dex_dir());

        // Valid project workspace must be honored
        let proj = Path::new("/tmp/dex_sample_workspace");
        let resolved_proj = resolve_dex_storage_root(Some(proj), Some(Path::new("/")));
        assert_eq!(resolved_proj, PathBuf::from("/tmp/dex_sample_workspace/.dex"));
    }

    #[test]
    fn test_resolve_safe_path_preserves_absolute_output_path() {
        let abs = PathBuf::from("/Users/robii/Projects/demo/robot_concept.png");
        assert_eq!(resolve_safe_path(&abs), abs);

        let sub = resolve_dex_subpath(".dex/media/images");
        assert!(sub.ends_with("media/images"));
        assert!(!sub.starts_with("/.dex"));
    }

    #[test]
    fn test_enriched_path_contains_homebrew_and_mlx_venv() {
        let home = Path::new("/Users/tester");
        let entries = enriched_path_entries(home, "/usr/bin:/bin");
        assert_eq!(entries[0], PathBuf::from("/Users/tester/.dex/runtimes/mlx-venv/bin"));
        assert!(entries.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(entries.contains(&PathBuf::from("/usr/local/bin")));
        assert!(entries.contains(&PathBuf::from("/Users/tester/.local/bin")));
        assert!(entries.contains(&PathBuf::from("/Users/tester/.cargo/bin")));
        assert!(entries.contains(&PathBuf::from("/usr/bin")));
    }
}
