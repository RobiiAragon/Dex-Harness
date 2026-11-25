use thiserror::Error;

#[derive(Error, Debug)]
pub enum ExtensionError {
    #[error("I/O error at '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("JSON serialization error for '{context}': {source}")]
    Json {
        context: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("Extension '{id}' not found in registry")]
    NotFound { id: String },

    #[error("Catalog recipe '{id}' does not exist")]
    CatalogNotFound { id: String },

    #[error("Failed to spawn process for extension '{id}' (command '{command}'): {source}{hint}")]
    ProcessSpawn {
        id: String,
        command: String,
        #[source]
        source: std::io::Error,
        /// Install marker for a missing launcher (see [`missing_launcher_hint`]).
        hint: String,
    },

    #[error("Healthcheck failed for extension '{id}' at '{url}': {reason}")]
    HealthcheckFailed {
        id: String,
        url: String,
        reason: String,
    },

    #[error("MCP bridge error for extension '{id}': {message}")]
    Mcp { id: String, message: String },

    #[error("Invalid extension configuration for '{id}': {reason}")]
    InvalidConfig { id: String, reason: String },

    #[error("Operation timed out for extension '{id}' after {timeout_secs}s")]
    Timeout { id: String, timeout_secs: u64 },
}

/// When an extension's launcher (`uvx`, `npx`, `blender`…) is missing, returns a sentence with a
/// `[dex-install:<id>]` marker so Dex.app / the CLI can offer a 1-click install. Empty otherwise.
pub fn missing_launcher_hint(command: &str, err: &std::io::Error) -> String {
    if err.kind() != std::io::ErrorKind::NotFound {
        return String::new();
    }
    let leaf = std::path::Path::new(command.trim())
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(command)
        .to_lowercase();
    let runtime = match leaf.as_str() {
        "uv" | "uvx" => "uv",
        "npx" | "npm" | "node" => "node",
        "blender" => "blender",
        "ffmpeg" | "ffprobe" => "ffmpeg",
        _ => return String::new(),
    };
    format!(
        ". `{leaf}` is not installed. {}",
        harness_core::install_marker(runtime)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_launchers_get_install_markers() {
        let nf = std::io::Error::new(std::io::ErrorKind::NotFound, "x");
        assert!(missing_launcher_hint("uvx", &nf).ends_with("[dex-install:uv]"));
        assert!(missing_launcher_hint("/usr/local/bin/npx", &nf).ends_with("[dex-install:node]"));
        assert_eq!(missing_launcher_hint("python3", &nf), "");
        let denied = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "x");
        assert_eq!(missing_launcher_hint("uvx", &denied), "");
    }
}
