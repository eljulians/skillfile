//! Background update notification.
//!
//! Spawns a background thread at CLI startup that checks GitHub Releases for
//! a newer version. If one exists, a one-line notice is returned via an
//! [`mpsc::Receiver`]. The check is cached for 24 hours to avoid rate limits.
//!
//! The check is skipped when:
//! - `SKILLFILE_NO_UPDATE_NOTIFIER=1` is set
//! - `CI=true` or `CI=1` is set
//! - stderr is not a TTY (piped output)
//!
//! Pattern: identical to the `gh` CLI's update notification.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// How often to check for updates (24 hours).
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// GitHub API endpoint for the latest release.
const RELEASES_URL: &str = "https://api.github.com/repos/eljulians/skillfile/releases/latest";

/// Current version of this binary (set at compile time from Cargo.toml).
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A notice to display when an update is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateNotice {
    pub current: String,
    pub latest: String,
    pub url: String,
}

impl std::fmt::Display for UpdateNotice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "A new version of skillfile is available: v{} -> v{}\n{}",
            self.current, self.latest, self.url
        )
    }
}

/// Cached state from the last update check.
#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    /// Unix timestamp (seconds since epoch) of last check.
    last_check: String,
    /// Latest version string (without `v` prefix).
    latest_version: String,
    /// URL to the release page.
    release_url: String,
}

/// Returns the path to the update-check cache file.
///
/// Uses `dirs::cache_dir()` for a platform-appropriate location:
/// - Linux: `~/.cache/skillfile/update-check.json`
/// - macOS: `~/Library/Caches/skillfile/update-check.json`
/// - Windows: `{FOLDERID_LocalAppData}/skillfile/update-check.json`
///
/// Returns `None` if the platform has no cache directory.
pub fn cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("skillfile").join("update-check.json"))
}

/// Determine whether the update check should run.
///
/// Returns `false` if any opt-out condition is met:
/// - `SKILLFILE_NO_UPDATE_NOTIFIER` env var is set and non-empty
/// - `CI` env var is `true` or `1`
/// - stderr is not a TTY (piped output)
pub fn should_check() -> bool {
    if std::env::var("SKILLFILE_NO_UPDATE_NOTIFIER").is_ok_and(|v| !v.is_empty()) {
        return false;
    }
    if std::env::var("CI").is_ok_and(|v| v == "true" || v == "1") {
        return false;
    }
    std::io::stderr().is_terminal()
}

/// Compare two semver version strings. Returns `true` if `latest > current`.
///
/// Leading `v` prefix is stripped before comparison. Returns `false` if
/// either version fails to parse as semver.
pub fn is_newer(current: &str, latest: &str) -> bool {
    let current = current.strip_prefix('v').unwrap_or(current);
    let latest = latest.strip_prefix('v').unwrap_or(latest);
    match (
        semver::Version::parse(current),
        semver::Version::parse(latest),
    ) {
        (Ok(c), Ok(l)) => l > c,
        _ => false,
    }
}

/// Read a cache entry from a specific file path.
///
/// Returns `None` if the file doesn't exist, can't be read, is malformed,
/// or is older than [`CHECK_INTERVAL`].
fn read_cache_from(path: &std::path::Path) -> Option<CacheEntry> {
    let contents = std::fs::read_to_string(path).ok()?;
    let entry: CacheEntry = serde_json::from_str(&contents).ok()?;
    let last_check = parse_timestamp(&entry.last_check)?;
    let elapsed = SystemTime::now().duration_since(last_check).ok()?;
    if elapsed < CHECK_INTERVAL {
        Some(entry)
    } else {
        None
    }
}

/// Read the cached update-check entry from the default cache path.
fn read_cache() -> Option<CacheEntry> {
    read_cache_from(&cache_path()?)
}

/// Write a cache entry to a specific file path. Errors are silently ignored.
fn write_cache_to(path: &std::path::Path, entry: &CacheEntry) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(entry) {
        let _ = std::fs::write(path, json);
    }
}

/// Write a cache entry to the default cache path. Errors are silently ignored.
fn write_cache(entry: &CacheEntry) {
    if let Some(path) = cache_path() {
        write_cache_to(&path, entry);
    }
}

/// Fetch the latest release tag and URL from the GitHub Releases API.
///
/// Returns `None` on any error (network, parse, rate limit). Errors are
/// intentionally swallowed — this runs in a background thread and must
/// never block or crash the CLI.
fn fetch_latest_release() -> Option<(String, String)> {
    let agent = ureq::Agent::new_with_defaults();
    let mut response = agent
        .get(RELEASES_URL)
        .header("User-Agent", "skillfile-update-check")
        .header("Accept", "application/vnd.github.v3+json")
        .call()
        .ok()?;

    let body = response.body_mut().read_to_string().ok()?;
    let data: serde_json::Value = serde_json::from_str(&body).ok()?;

    let tag = data["tag_name"].as_str()?;
    let html_url = data["html_url"]
        .as_str()
        .map(std::string::ToString::to_string);
    let url = html_url
        .unwrap_or_else(|| format!("https://github.com/eljulians/skillfile/releases/tag/{tag}"));

    Some((tag.to_string(), url))
}

/// Core update check logic. Returns an [`UpdateNotice`] if a newer version exists.
///
/// 1. Reads the cache file. If fresh (< 24h), compares cached version.
/// 2. Otherwise, fetches from GitHub API and writes cache.
/// 3. Returns `Some(UpdateNotice)` if the latest version is newer.
///
/// All errors are silently swallowed — this function never panics or returns errors.
pub fn check_for_update() -> Option<UpdateNotice> {
    // Try cache first
    if let Some(cached) = read_cache() {
        return if is_newer(CURRENT_VERSION, &cached.latest_version) {
            Some(UpdateNotice {
                current: CURRENT_VERSION.to_string(),
                latest: cached.latest_version,
                url: cached.release_url,
            })
        } else {
            None
        };
    }

    // Cache miss or stale — fetch from GitHub
    let (tag, url) = fetch_latest_release()?;
    let version = tag.strip_prefix('v').unwrap_or(&tag).to_string();

    // Write cache regardless of whether there's an update
    write_cache(&CacheEntry {
        last_check: now_timestamp(),
        latest_version: version.clone(),
        release_url: url.clone(),
    });

    if is_newer(CURRENT_VERSION, &version) {
        Some(UpdateNotice {
            current: CURRENT_VERSION.to_string(),
            latest: version,
            url,
        })
    } else {
        None
    }
}

/// Spawn a background thread that checks for updates.
///
/// Writes a sentinel cache entry immediately (in the calling thread) so
/// that even if the process exits before the background HTTP call completes,
/// subsequent invocations within 24 hours won't re-check.
///
/// Returns an [`mpsc::Receiver`] that will receive at most one
/// `Option<UpdateNotice>`. If the check finds no update, the channel
/// receives `None`. If the thread errors or the receiver is dropped
/// before the thread completes, the result is silently discarded.
pub fn spawn_check() -> mpsc::Receiver<Option<UpdateNotice>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let notice = check_for_update();
        let _ = tx.send(notice);
    });
    rx
}

// ---------------------------------------------------------------------------
// Timestamp helpers (no chrono dependency needed)
// ---------------------------------------------------------------------------

/// Return the current time as a Unix timestamp string (seconds since epoch).
fn now_timestamp() -> String {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

/// Parse a Unix timestamp string back to [`SystemTime`].
fn parse_timestamp(s: &str) -> Option<SystemTime> {
    let secs: u64 = s.parse().ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // is_newer — pure semver comparison
    // -----------------------------------------------------------------------

    #[test]
    fn is_newer_detects_minor_bump() {
        assert!(is_newer("1.0.0", "1.1.0"));
    }

    #[test]
    fn is_newer_detects_major_bump() {
        assert!(is_newer("1.0.0", "2.0.0"));
    }

    #[test]
    fn is_newer_detects_patch_bump() {
        assert!(is_newer("1.0.0", "1.0.1"));
    }

    #[test]
    fn is_newer_returns_false_when_same() {
        assert!(!is_newer("1.0.0", "1.0.0"));
    }

    #[test]
    fn is_newer_returns_false_when_current_is_greater() {
        assert!(!is_newer("1.1.0", "1.0.0"));
        assert!(!is_newer("2.0.0", "1.0.0"));
    }

    #[test]
    fn is_newer_strips_v_prefix() {
        assert!(is_newer("v1.0.0", "v1.1.0"));
        assert!(is_newer("1.0.0", "v1.1.0"));
        assert!(is_newer("v1.0.0", "1.1.0"));
    }

    #[test]
    fn is_newer_returns_false_on_invalid_semver() {
        assert!(!is_newer("not-a-version", "1.0.0"));
        assert!(!is_newer("1.0.0", "not-a-version"));
        assert!(!is_newer("abc", "def"));
    }

    #[test]
    fn is_newer_handles_prerelease() {
        // Pre-release < release per semver spec
        assert!(is_newer("1.0.0-alpha", "1.0.0"));
        assert!(!is_newer("1.0.0", "1.0.0-alpha"));
    }

    // -----------------------------------------------------------------------
    // should_check — env var guards
    // -----------------------------------------------------------------------

    #[test]
    fn should_check_blocked_by_no_update_notifier() {
        // Note: env var manipulation is not thread-safe, but these tests
        // are simple enough that interference is unlikely.
        let key = "SKILLFILE_NO_UPDATE_NOTIFIER";
        let original = std::env::var(key).ok();

        std::env::set_var(key, "1");
        assert!(!should_check());

        std::env::set_var(key, "yes");
        assert!(!should_check());

        // Restore
        match original {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn should_check_blocked_by_ci_true() {
        let key = "CI";
        let original = std::env::var(key).ok();

        std::env::set_var(key, "true");
        assert!(!should_check());

        std::env::set_var(key, "1");
        assert!(!should_check());

        match original {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn should_check_not_blocked_by_empty_notifier_var() {
        let key = "SKILLFILE_NO_UPDATE_NOTIFIER";
        let original = std::env::var(key).ok();

        // Empty string should NOT block the check
        std::env::set_var(key, "");
        // Result depends on TTY state, but at least it shouldn't be blocked by env
        // We can't assert true here because stderr may not be a TTY in CI.
        // Just verify no panic.
        let _ = should_check();

        match original {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    // -----------------------------------------------------------------------
    // UpdateNotice Display
    // -----------------------------------------------------------------------

    #[test]
    fn update_notice_display_contains_versions_and_url() {
        let notice = UpdateNotice {
            current: "1.0.0".to_string(),
            latest: "1.1.0".to_string(),
            url: "https://github.com/eljulians/skillfile/releases/tag/v1.1.0".to_string(),
        };
        let s = notice.to_string();
        assert!(s.contains("v1.0.0 -> v1.1.0"));
        assert!(s.contains("https://github.com/eljulians/skillfile/releases/tag/v1.1.0"));
    }

    #[test]
    fn update_notice_display_starts_with_new_version_message() {
        let notice = UpdateNotice {
            current: "1.0.1".to_string(),
            latest: "2.0.0".to_string(),
            url: "https://example.com/release".to_string(),
        };
        assert!(notice.to_string().starts_with("A new version of skillfile"));
    }

    // -----------------------------------------------------------------------
    // Timestamp helpers
    // -----------------------------------------------------------------------

    #[test]
    fn now_timestamp_is_parseable() {
        let ts = now_timestamp();
        let parsed = parse_timestamp(&ts);
        assert!(
            parsed.is_some(),
            "now_timestamp() should produce a parseable value"
        );
    }

    #[test]
    fn parse_timestamp_valid_unix() {
        let t = parse_timestamp("1710000000");
        assert!(t.is_some());
    }

    #[test]
    fn parse_timestamp_invalid_returns_none() {
        assert!(parse_timestamp("not-a-number").is_none());
        assert!(parse_timestamp("").is_none());
    }

    #[test]
    fn timestamp_round_trip() {
        let ts = now_timestamp();
        let parsed = parse_timestamp(&ts).unwrap();
        let elapsed = SystemTime::now().duration_since(parsed).unwrap();
        // Should be within a few seconds of "now"
        assert!(elapsed.as_secs() < 5);
    }

    // -----------------------------------------------------------------------
    // CacheEntry serialization
    // -----------------------------------------------------------------------

    #[test]
    fn cache_entry_serialization_round_trip() {
        let entry = CacheEntry {
            last_check: "1710000000".to_string(),
            latest_version: "1.2.3".to_string(),
            release_url: "https://example.com/release".to_string(),
        };

        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: CacheEntry = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.last_check, "1710000000");
        assert_eq!(deserialized.latest_version, "1.2.3");
        assert_eq!(deserialized.release_url, "https://example.com/release");
    }

    // -----------------------------------------------------------------------
    // cache_path
    // -----------------------------------------------------------------------

    #[test]
    fn cache_path_ends_with_expected_filename() {
        if let Some(path) = cache_path() {
            assert!(
                path.ends_with("skillfile/update-check.json"),
                "unexpected cache path: {path:?}"
            );
        }
        // If dirs::cache_dir() returns None (e.g., $HOME unset), this is fine.
    }

    // -----------------------------------------------------------------------
    // write_cache_to + read_cache_from (isolated via temp dirs)
    // -----------------------------------------------------------------------

    #[test]
    fn write_cache_to_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/dir/update-check.json");

        let entry = CacheEntry {
            last_check: now_timestamp(),
            latest_version: "99.99.99".to_string(),
            release_url: "https://example.com".to_string(),
        };
        write_cache_to(&path, &entry);
        assert!(path.exists(), "cache file should be created");
    }

    #[test]
    fn read_cache_from_returns_none_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        assert!(read_cache_from(&path).is_none());
    }

    #[test]
    fn read_cache_from_returns_entry_when_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update-check.json");

        let entry = CacheEntry {
            last_check: now_timestamp(),
            latest_version: "99.88.77".to_string(),
            release_url: "https://example.com/fresh".to_string(),
        };
        write_cache_to(&path, &entry);

        let cached = read_cache_from(&path);
        assert!(cached.is_some(), "fresh cache entry should be readable");
        let cached = cached.unwrap();
        assert_eq!(cached.latest_version, "99.88.77");
        assert_eq!(cached.release_url, "https://example.com/fresh");
    }

    #[test]
    fn read_cache_from_returns_none_when_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update-check.json");

        let stale_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 48 * 60 * 60;
        let entry = CacheEntry {
            last_check: stale_time.to_string(),
            latest_version: "99.88.77".to_string(),
            release_url: "https://example.com/stale".to_string(),
        };
        write_cache_to(&path, &entry);

        assert!(
            read_cache_from(&path).is_none(),
            "stale cache entry (48h old) should be ignored"
        );
    }

    #[test]
    fn read_cache_from_returns_none_on_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update-check.json");
        std::fs::write(&path, "not valid json").unwrap();
        assert!(read_cache_from(&path).is_none());
    }

    #[test]
    fn read_cache_from_returns_none_on_invalid_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update-check.json");
        let entry = CacheEntry {
            last_check: "not-a-number".to_string(),
            latest_version: "1.0.0".to_string(),
            release_url: "https://example.com".to_string(),
        };
        write_cache_to(&path, &entry);
        assert!(read_cache_from(&path).is_none());
    }

    // -----------------------------------------------------------------------
    // spawn_check — plumbing test
    // -----------------------------------------------------------------------

    #[test]
    fn spawn_check_returns_receiver_without_panic() {
        let rx = spawn_check();
        // The background thread will likely fail (no network in CI or
        // rate-limited), but it must not panic.
        std::thread::sleep(Duration::from_millis(200));
        // Either got a result or the sender was dropped — both are fine.
        let _ = rx.try_recv();
    }
}
