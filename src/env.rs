//! Environment variables used throughout the project.
//!
//! Call [`validate`] at startup to force every variable: required ones panic
//! with a clear message when missing, and present-but-unparseable values
//! panic instead of silently falling back to a default.

use std::collections::HashSet;
use std::sync::LazyLock;

/// The allowlist of websites to permit downloads from (required).
///
/// Format: `site1.com,site2.net,site3.edu`.
pub static ALLOWLIST: LazyLock<HashSet<String>> = LazyLock::new(|| {
    let raw = std::env::var("ALLOWLIST")
        .expect("ALLOWLIST environment variable should be set (comma-separated domains)");

    let set: HashSet<String> = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    assert!(
        !set.is_empty(),
        "ALLOWLIST should contain at least one domain"
    );

    set
});

/// Application instance's maintainer handle (optional).
pub static MAINTAINER: LazyLock<Option<String>> =
    LazyLock::new(|| std::env::var("MAINTAINER").ok().map(|s| format!("@{s}")));

/// Maximum file size allowed for processing, in MB (required).
pub static MAX_FILESIZE: LazyLock<u64> = LazyLock::new(|| {
    let raw = std::env::var("MAX_FILESIZE")
        .expect("MAX_FILESIZE environment variable should be set (a size in MB)");

    raw.parse().unwrap_or_else(|_| {
        panic!("MAX_FILESIZE should be a whole number of MB, got {raw:?}");
    })
});

/// Maximum file size allowed when in fallback mode, in MB
/// (optional, defaults to [`MAX_FILESIZE`] `* 5`).
pub static FALLBACK_FILESIZE: LazyLock<u64> = LazyLock::new(|| {
    std::env::var("FALLBACK_FILESIZE").map_or_else(
        |_| *MAX_FILESIZE * 5,
        |raw| {
            raw.parse().unwrap_or_else(|_| {
                panic!("FALLBACK_FILESIZE should be a whole number of MB, got {raw:?}");
            })
        },
    )
});

/// Supported log output formats.
pub enum LogFormat {
    /// Tree-structured output via `tracing_forest` (default).
    Forest,
    /// JSON output (for piping to tools like `hl`).
    Json,
}

/// The log output format (optional, defaults to [`LogFormat::Forest`]).
///
/// Recognised values (case-insensitive): `json`, `forest`.
pub static LOG_FORMAT: LazyLock<LogFormat> =
    LazyLock::new(|| match std::env::var("LOG_FORMAT").as_deref() {
        Ok(v) if v.eq_ignore_ascii_case("json") => LogFormat::Json,
        Ok(v) if v.eq_ignore_ascii_case("forest") => LogFormat::Forest,
        Ok(v) => panic!("unknown LOG_FORMAT: {v:?} (expected \"json\" or \"forest\")"),
        Err(_) => LogFormat::Forest,
    });

/// Forces evaluation of every environment variable, panicking early with a
/// clear message on missing required values or unparseable optional ones.
pub fn validate() {
    LazyLock::force(&ALLOWLIST);
    LazyLock::force(&MAINTAINER);
    LazyLock::force(&MAX_FILESIZE);
    LazyLock::force(&FALLBACK_FILESIZE);
    LazyLock::force(&LOG_FORMAT);
}
