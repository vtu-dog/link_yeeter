//! Environment variables used throughout the project.

use std::collections::HashSet;
use std::sync::LazyLock;

/// The allowlist of websites to permit downloads from.
/// Env var format: `site1.com,site2.net,site3.edu`.
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

/// Maximum file size allowed for processing, in megabytes.
pub static MAX_FILESIZE: LazyLock<u64> = LazyLock::new(|| {
    std::env::var("MAX_FILESIZE")
        .ok()
        .map(|s| s.parse().unwrap_or(200))
        .unwrap()
});

/// Maximum file size allowed when in fallback mode.
pub static FALLBACK_FILESIZE: LazyLock<u64> = LazyLock::new(|| *MAX_FILESIZE * 5);
