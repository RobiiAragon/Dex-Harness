//! Machine-readable "1-click install" markers for missing-dependency errors.
//!
//! Every error that means "a backend is not installed" embeds `[dex-install:<runtime-id>]`
//! (the id understood by `harness_models::RuntimeKind::from_str_loose` and
//! `dex models install-runtime`). Dex.app turns the marker into an **Install** button, the CLI
//! offers to install and retry, and the agent can tell the user exactly what is missing —
//! so non-expert users never have to read an install guide.

const PREFIX: &str = "[dex-install:";

/// Formats the marker for a runtime id, e.g. `[dex-install:ffmpeg]`.
pub fn install_marker(runtime_id: &str) -> String {
    format!("{PREFIX}{}]", runtime_id.trim())
}

/// Appends the marker to an install hint (idempotent).
pub fn with_install_marker(hint: impl Into<String>, runtime_id: &str) -> String {
    let hint = hint.into();
    let marker = install_marker(runtime_id);
    if hint.contains(&marker) {
        hint
    } else {
        format!("{hint} {marker}")
    }
}

/// Extracts every runtime id referenced by markers in `text`, in order and without duplicates.
pub fn parse_install_markers(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(PREFIX) {
        let after = &rest[start + PREFIX.len()..];
        let Some(end) = after.find(']') else {
            break;
        };
        let id = after[..end].trim();
        let valid = !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if valid && !ids.iter().any(|x: &String| x == id) {
            ids.push(id.to_string());
        }
        rest = &after[end..];
    }
    ids
}

/// Removes markers from user-facing text.
pub fn strip_install_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(PREFIX) {
        out.push_str(&rest[..start]);
        match rest[start..].find(']') {
            Some(end) => rest = &rest[start + end + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_roundtrip() {
        let hint = with_install_marker("FFmpeg is missing.", "ffmpeg");
        assert_eq!(hint, "FFmpeg is missing. [dex-install:ffmpeg]");
        assert_eq!(with_install_marker(hint.clone(), "ffmpeg"), hint);
        let text = format!("{hint} also {} and {}", install_marker("gen3d"), install_marker("ffmpeg"));
        assert_eq!(parse_install_markers(&text), vec!["ffmpeg", "gen3d"]);
        assert_eq!(strip_install_markers(&hint), "FFmpeg is missing.");
        assert!(parse_install_markers("[dex-install:bad id] [dex-install:").is_empty());
    }
}
