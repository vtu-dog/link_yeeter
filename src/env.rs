//! Environment variables used throughout the project.

use std::collections::HashSet;
use std::sync::LazyLock;

/// The allowlist of websites to permit downloads from.
///
/// Format: `site1.com,site2.net,site3.edu`.
pub static ALLOWLIST: LazyLock<HashSet<String>> = LazyLock::new(|| {
    std::env::var("ALLOWLIST")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
});

/// Application instance's maintainer handle.
pub static MAINTAINER: LazyLock<Option<String>> =
    LazyLock::new(|| std::env::var("MAINTAINER").ok().map(|s| format!("@{s}")));

/// Maximum file size allowed for processing, in MB (default: 200).
pub static MAX_FILESIZE: LazyLock<u64> = LazyLock::new(|| {
    std::env::var("MAX_FILESIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200)
});

/// Maximum file size allowed when in fallback mode.
pub static FALLBACK_FILESIZE: LazyLock<u64> = LazyLock::new(|| {
    std::env::var("FALLBACK_FILESIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(*MAX_FILESIZE * 5)
});

/// Supported log output formats.
#[non_exhaustive]
pub enum LogFormat {
    /// Tree-structured output via `tracing_forest` (default).
    Forest,
    /// JSON output (for piping to tools like `hl`).
    Json,
    /// Unrecognised value from the environment.
    Unknown(String),
    /// The `LOG_FORMAT` environment variable was not set.
    Unset,
}

/// The log output format, controlled by the `LOG_FORMAT` environment variable.
///
/// Recognised values (case-insensitive): `json`, `forest`.
pub static LOG_FORMAT: LazyLock<LogFormat> =
    LazyLock::new(|| match std::env::var("LOG_FORMAT").as_deref() {
        Ok(v) if v.eq_ignore_ascii_case("json") => LogFormat::Json,
        Ok(v) if v.eq_ignore_ascii_case("forest") => LogFormat::Forest,
        Ok(v) => LogFormat::Unknown(v.to_owned()),
        Err(_) => LogFormat::Unset,
    });
