use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::cache_monitor::format_size;

// ============================================================================
// SAFETY: Hardcoded cache path - NEVER accept user input for paths
// ============================================================================
const CACHE_FOLDER_NAME: &str = "com.apple.coresymbolicationd";

/// Get the ONLY allowed cache path - hardcoded for safety
/// This function constructs the path from known safe components
fn get_safe_cache_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME environment variable must be set");
    PathBuf::from(home)
        .join("Library")
        .join("Caches")
        .join(CACHE_FOLDER_NAME)
}

/// SAFETY CHECK: Verify a path is exactly the allowed cache location
/// Returns error if path doesn't match expected location
fn verify_safe_path(path: &PathBuf) -> Result<(), CleanError> {
    let expected = get_safe_cache_path();

    // Canonicalize both paths to resolve any symlinks or .. components
    let canonical_expected = expected.canonicalize().unwrap_or_else(|_| expected.clone());
    let canonical_path = path.canonicalize().unwrap_or_else(|_| path.clone());

    // Strict equality check
    if canonical_path != canonical_expected {
        return Err(CleanError::SafetyViolation(format!(
            "Path '{}' does not match expected cache location '{}'",
            path.display(),
            expected.display()
        )));
    }

    // Additional check: ensure path contains expected folder name
    if !path.to_string_lossy().contains(CACHE_FOLDER_NAME) {
        return Err(CleanError::SafetyViolation(format!(
            "Path does not contain expected folder name '{}'",
            CACHE_FOLDER_NAME
        )));
    }

    Ok(())
}

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanResult {
    pub success: bool,
    pub bytes_freed: u64,
    pub bytes_freed_display: String,
    pub files_removed: u64,
    pub timestamp: u64,
    pub message: String,
    pub requires_password: bool,
    pub was_dry_run: bool,
    pub items_found: Vec<DeletionItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletionItem {
    pub path: String,
    pub size: u64,
    pub size_display: String,
    pub is_directory: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CleanError {
    SafetyViolation(String),
    CacheNotFound(String),
    RemovalFailed(String),
    Unknown(String),
}

impl std::fmt::Display for CleanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CleanError::SafetyViolation(msg) => write!(f, "SAFETY VIOLATION: {}", msg),
            CleanError::CacheNotFound(msg) => write!(f, "Cache not found: {}", msg),
            CleanError::RemovalFailed(msg) => write!(f, "Failed to remove cache: {}", msg),
            CleanError::Unknown(msg) => write!(f, "Unknown error: {}", msg),
        }
    }
}

// ============================================================================
// Logging
// ============================================================================

fn get_log_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join("Library")
        .join("Logs")
        .join("SymbolSweep")
        .join("deletions.log")
}

pub fn log_deletion(message: &str) {
    let log_path = get_log_path();

    // Ensure log directory exists
    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let timestamp = chrono_format_now();
    let log_line = format!("[{}] {}\n", timestamp, message);

    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = file.write_all(log_line.as_bytes());
    }
}

fn chrono_format_now() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Simple timestamp format without external crate
    let secs_per_day = 86400;
    let secs_per_hour = 3600;
    let secs_per_min = 60;

    let days_since_epoch = now / secs_per_day;
    let time_of_day = now % secs_per_day;

    let hours = time_of_day / secs_per_hour;
    let minutes = (time_of_day % secs_per_hour) / secs_per_min;
    let seconds = time_of_day % secs_per_min;

    // Approximate date calculation (good enough for logging)
    let years = 1970 + (days_since_epoch / 365);
    let remaining_days = days_since_epoch % 365;
    let months = remaining_days / 30 + 1;
    let days = remaining_days % 30 + 1;

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        years, months, days, hours, minutes, seconds
    )
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

// ============================================================================
// Cache Analysis (for dry run)
// ============================================================================

/// Analyze what would be deleted (dry run)
pub fn analyze_cache() -> Result<Vec<DeletionItem>, CleanError> {
    let cache_path = get_safe_cache_path();

    // Safety check
    verify_safe_path(&cache_path)?;

    if !cache_path.exists() {
        return Ok(Vec::new());
    }

    let mut items = Vec::new();

    // SAFETY: Only read direct children of the cache folder
    // NO recursive operations, NO wildcards
    let entries = fs::read_dir(&cache_path)
        .map_err(|e| CleanError::RemovalFailed(format!("Cannot read directory: {}", e)))?;

    for entry in entries.flatten() {
        let entry_path = entry.path();

        // Double-check each entry is within the safe path
        if !entry_path.starts_with(&cache_path) {
            log_deletion(&format!("SAFETY: Skipped suspicious path: {}", entry_path.display()));
            continue;
        }

        let is_directory = entry_path.is_dir();
        let size = if is_directory {
            get_dir_size(&entry_path)
        } else {
            entry.metadata().map(|m| m.len()).unwrap_or(0)
        };

        items.push(DeletionItem {
            path: entry_path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            size,
            size_display: format_size(size),
            is_directory,
        });
    }

    Ok(items)
}

/// Get directory size (only for directories within the cache folder)
fn get_dir_size(path: &std::path::Path) -> u64 {
    let safe_cache = get_safe_cache_path();

    // SAFETY: Only calculate size for paths within our cache folder
    if !path.starts_with(&safe_cache) {
        return 0;
    }

    let mut size: u64 = 0;

    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let entry_path = entry.path();

            // Double-check we're still within bounds
            if !entry_path.starts_with(&safe_cache) {
                continue;
            }

            if entry_path.is_dir() {
                size += get_dir_size(&entry_path);
            } else if let Ok(metadata) = entry.metadata() {
                size += metadata.len();
            }
        }
    }

    size
}

// ============================================================================
// Cache Cleaning
// ============================================================================

/// Clean the cache with full safety checks
///
/// SAFETY GUARANTEES:
/// - Only deletes from ~/Library/Caches/com.apple.coresymbolicationd
/// - Path is hardcoded, never from user input
/// - Verifies path before any deletion
/// - Logs every deletion with timestamp
/// - No wildcards or recursive deletes outside the exact folder
pub fn clean_cache(dry_run: bool) -> Result<CleanResult, CleanError> {
    let cache_path = get_safe_cache_path();

    // SAFETY CHECK 1: Verify path is exactly what we expect
    verify_safe_path(&cache_path)?;

    // Check if cache exists
    if !cache_path.exists() {
        return Ok(CleanResult {
            success: true,
            bytes_freed: 0,
            bytes_freed_display: "0 B".to_string(),
            files_removed: 0,
            timestamp: current_timestamp(),
            message: "Cache directory does not exist - nothing to clean".to_string(),
            requires_password: false,
            was_dry_run: dry_run,
            items_found: Vec::new(),
        });
    }

    // Analyze what we would delete
    let items = analyze_cache()?;
    let total_size: u64 = items.iter().map(|i| i.size).sum();
    let total_count = items.len() as u64;

    // Nothing to clean — return silently without logging
    if total_count == 0 && !dry_run {
        return Ok(CleanResult {
            success: true,
            bytes_freed: 0,
            bytes_freed_display: "0 B".to_string(),
            files_removed: 0,
            timestamp: current_timestamp(),
            message: "Nothing to clean".to_string(),
            requires_password: false,
            was_dry_run: false,
            items_found: Vec::new(),
        });
    }

    // Only log operations that will actually do something
    log_deletion(&format!(
        "=== {} STARTED ===",
        if dry_run { "DRY RUN" } else { "CLEAN OPERATION" }
    ));
    log_deletion(&format!("Target path: {}", cache_path.display()));
    log_deletion(&format!(
        "Found {} items totaling {}",
        total_count,
        format_size(total_size)
    ));

    // If dry run, return analysis without deleting
    if dry_run {
        log_deletion("DRY RUN - No files were deleted");
        log_deletion("=== DRY RUN COMPLETE ===");

        return Ok(CleanResult {
            success: true,
            bytes_freed: total_size,
            bytes_freed_display: format_size(total_size),
            files_removed: total_count,
            timestamp: current_timestamp(),
            message: format!(
                "Dry run: would delete {} ({} items)",
                format_size(total_size),
                total_count
            ),
            requires_password: false,
            was_dry_run: true,
            items_found: items,
        });
    }

    // Delete each item individually (no recursive wildcards)
    let mut bytes_freed: u64 = 0;
    let mut files_removed: u64 = 0;

    let entries = fs::read_dir(&cache_path)
        .map_err(|e| CleanError::RemovalFailed(format!("Cannot read directory: {}", e)))?;

    for entry in entries.flatten() {
        let entry_path = entry.path();

        // SAFETY CHECK 2: Verify each entry is within the cache folder
        if !entry_path.starts_with(&cache_path) {
            log_deletion(&format!("SAFETY: Refused to delete path outside cache: {}", entry_path.display()));
            continue;
        }

        // SAFETY CHECK 3: Verify the full path still contains our expected folder
        if !entry_path.to_string_lossy().contains(CACHE_FOLDER_NAME) {
            log_deletion(&format!("SAFETY: Refused to delete - path missing expected folder: {}", entry_path.display()));
            continue;
        }

        let is_dir = entry_path.is_dir();
        let size = if is_dir {
            get_dir_size(&entry_path)
        } else {
            entry.metadata().map(|m| m.len()).unwrap_or(0)
        };

        // Perform deletion
        let result = if is_dir {
            fs::remove_dir_all(&entry_path)
        } else {
            fs::remove_file(&entry_path)
        };

        match result {
            Ok(()) => {
                bytes_freed += size;
                files_removed += 1;
                log_deletion(&format!(
                    "DELETED: {} ({}, {})",
                    entry_path.file_name().unwrap_or_default().to_string_lossy(),
                    format_size(size),
                    if is_dir { "directory" } else { "file" }
                ));
            }
            Err(e) => {
                log_deletion(&format!(
                    "FAILED to delete {}: {}",
                    entry_path.display(),
                    e
                ));
            }
        }
    }

    log_deletion(&format!(
        "Clean complete: freed {} ({} items removed)",
        format_size(bytes_freed),
        files_removed
    ));
    log_deletion("=== CLEAN OPERATION COMPLETE ===");

    Ok(CleanResult {
        success: true,
        bytes_freed,
        bytes_freed_display: format_size(bytes_freed),
        files_removed,
        timestamp: current_timestamp(),
        message: format!(
            "Cleaned {} ({} items)",
            format_size(bytes_freed),
            files_removed
        ),
        requires_password: false,
        was_dry_run: false,
        items_found: items,
    })
}

/// Get the log file path (for UI display)
pub fn get_log_file_path() -> String {
    get_log_path().to_string_lossy().to_string()
}

// ============================================================================
// Tests — run with: cargo test --lib -- --test-threads=1
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper: create a dummy file inside the coresymbolicationd cache directory.
    fn create_dummy_content(name: &str, size: usize) -> PathBuf {
        let cache_path = get_safe_cache_path();
        fs::create_dir_all(&cache_path).unwrap();
        let file_path = cache_path.join(name);
        let content = vec![0xABu8; size];
        fs::write(&file_path, &content).unwrap();
        file_path
    }

    // ----------------------------------------------------------------
    // Test 1: autoclean deletes dummy content (before / after proof)
    // ----------------------------------------------------------------
    #[test]
    fn test_clean_deletes_dummy_content() {
        // Arrange: create dummy files in coresymbolicationd
        let file1 = create_dummy_content("_test_dummy_1.dat", 1024);
        let file2 = create_dummy_content("_test_dummy_2.dat", 2048);

        assert!(file1.exists(), "BEFORE: dummy file 1 must exist");
        assert!(file2.exists(), "BEFORE: dummy file 2 must exist");

        // Act: trigger autoclean (same code path as scheduler)
        let result = clean_cache(false).expect("clean_cache should succeed");

        // Assert: files are gone, bytes freed
        assert!(!file1.exists(), "AFTER: dummy file 1 should be deleted");
        assert!(!file2.exists(), "AFTER: dummy file 2 should be deleted");
        assert!(result.bytes_freed > 0, "Should have freed >0 bytes, got {}", result.bytes_freed);
        assert!(result.files_removed > 0, "Should have removed >0 items");
        assert!(result.success);
        assert!(!result.was_dry_run);

        println!("PASS: clean deleted dummy content. bytes_freed={}, files_removed={}",
            result.bytes_freed, result.files_removed);
    }

    // ----------------------------------------------------------------
    // Test 2: threshold – fires over, doesn't fire under
    // ----------------------------------------------------------------
    #[test]
    fn test_threshold_respected() {
        use crate::scheduler::{Scheduler, Settings};

        // Create dummy content so the cache is non-empty
        let dummy = create_dummy_content("_test_threshold.dat", 10 * 1024); // 10 KB
        assert!(dummy.exists());

        // --- Under threshold: set threshold way above actual size → must NOT fire ---
        let mut settings_under = Settings::default();
        settings_under.auto_clean_on_threshold = true;
        settings_under.auto_clean_threshold = 100 * 1024 * 1024 * 1024; // 100 GB
        let sched_under = Scheduler::new(settings_under);
        assert!(
            !sched_under.should_auto_clean_threshold(),
            "Must NOT auto-clean when cache is under threshold"
        );

        // --- Over threshold: set threshold to 1 byte → must fire ---
        let mut settings_over = Settings::default();
        settings_over.auto_clean_on_threshold = true;
        settings_over.auto_clean_threshold = 1; // 1 byte
        let sched_over = Scheduler::new(settings_over);
        assert!(
            sched_over.should_auto_clean_threshold(),
            "MUST auto-clean when cache is over threshold"
        );

        // Clean up
        let _ = fs::remove_file(&dummy);
        println!("PASS: threshold respected (fires over, doesn't fire under)");
    }

    // ----------------------------------------------------------------
    // Test 3: CANNOT reach Rebuildable / Reinstall / Review or other caches
    // ----------------------------------------------------------------
    #[test]
    fn test_cannot_reach_other_caches() {
        let home = std::env::var("HOME").unwrap();

        // Every path that autoclean must NEVER accept
        let forbidden_paths: Vec<PathBuf> = vec![
            // Package-manager caches (Safe tier – manual Clean Now only)
            PathBuf::from(&home).join("Library/Caches/Homebrew"),
            PathBuf::from(&home).join(".npm/_cacache"),
            PathBuf::from(&home).join(".cargo/registry"),
            PathBuf::from(&home).join("Library/Caches/pip"),
            // Rebuildable tier
            PathBuf::from(&home).join("Library/Caches/com.apple.dt.Xcode"),
            PathBuf::from(&home).join("Library/Developer/Xcode/DerivedData"),
            // Reinstall tier
            PathBuf::from("/Applications"),
            // Review tier
            PathBuf::from(&home).join("Documents"),
            PathBuf::from(&home).join("Desktop"),
            // Other system caches
            PathBuf::from(&home).join("Library/Caches"),
            PathBuf::from(&home).join("Library/Caches/com.apple.Safari"),
            // Traversal / escape attempts
            PathBuf::from(&home).join("Library/Caches/com.apple.coresymbolicationd/../.."),
            PathBuf::from("/tmp"),
            PathBuf::from("/"),
        ];

        for path in &forbidden_paths {
            let result = verify_safe_path(&path.clone());
            assert!(
                result.is_err(),
                "SAFETY FAILURE: verify_safe_path accepted forbidden path '{}'",
                path.display()
            );
            match result {
                Err(CleanError::SafetyViolation(_)) => { /* correct */ }
                other => panic!(
                    "Expected SafetyViolation for '{}', got: {:?}",
                    path.display(),
                    other
                ),
            }
        }

        // Also verify the ONLY accepted path is coresymbolicationd
        let ok_path = get_safe_cache_path();
        if ok_path.exists() {
            assert!(
                verify_safe_path(&ok_path).is_ok(),
                "The real coresymbolicationd path should be accepted"
            );
        }

        println!("PASS: {} forbidden paths correctly rejected", forbidden_paths.len());
    }

    // ----------------------------------------------------------------
    // Test 4: logs only when bytes_freed > 0
    // ----------------------------------------------------------------
    #[test]
    fn test_no_log_when_nothing_to_clean() {
        // First clean to ensure the cache is empty
        let _ = clean_cache(false);

        // Count cache-cleaner log lines (=== ... ===) BEFORE the no-op clean.
        // We check these specific markers rather than total file size because
        // dev_scanner tests share the same log file and may write concurrently.
        let log_path = get_log_path();
        let count_markers = |contents: &str| -> usize {
            contents.lines().filter(|l| l.contains("=== CLEAN OPERATION")).count()
        };
        let log_before = fs::read_to_string(&log_path).unwrap_or_default();
        let markers_before = count_markers(&log_before);

        // Clean again – cache is empty, so nothing should happen
        let result = clean_cache(false).expect("clean_cache should succeed on empty dir");
        assert_eq!(result.bytes_freed, 0, "Should free 0 bytes on empty cache");
        assert_eq!(result.files_removed, 0, "Should remove 0 files on empty cache");

        // No new CLEAN OPERATION markers should have been written
        let log_after = fs::read_to_string(&log_path).unwrap_or_default();
        let markers_after = count_markers(&log_after);
        assert_eq!(
            markers_before, markers_after,
            "No clean-operation log entries should be written when nothing was cleaned (before={}, after={})",
            markers_before, markers_after
        );

        println!("PASS: no log entry when bytes_freed = 0");
    }
}
