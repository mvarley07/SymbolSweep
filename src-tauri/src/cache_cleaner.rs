use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
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
    PermissionDenied(String),
    DaemonKillFailed(String),
    CacheNotFound(String),
    RemovalFailed(String),
    Unknown(String),
}

impl std::fmt::Display for CleanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CleanError::SafetyViolation(msg) => write!(f, "SAFETY VIOLATION: {}", msg),
            CleanError::PermissionDenied(msg) => write!(f, "Permission denied: {}", msg),
            CleanError::DaemonKillFailed(msg) => write!(f, "Failed to stop daemon: {}", msg),
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

fn log_deletion(message: &str) {
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
// Daemon Control
// ============================================================================

/// Stop the coresymbolicationd daemon
fn stop_daemon() -> Result<(), CleanError> {
    let result = Command::new("killall")
        .arg("-9")
        .arg("coresymbolicationd")
        .output();

    match result {
        Ok(output) => {
            if output.status.success() || output.status.code() == Some(1) {
                log_deletion("Stopped coresymbolicationd daemon");
                Ok(())
            } else {
                stop_daemon_with_privileges()
            }
        }
        Err(e) => Err(CleanError::DaemonKillFailed(e.to_string())),
    }
}

/// Stop daemon using osascript with administrator privileges
fn stop_daemon_with_privileges() -> Result<(), CleanError> {
    let script = r#"do shell script "killall -9 coresymbolicationd 2>/dev/null || true" with administrator privileges"#;

    let result = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output();

    match result {
        Ok(output) => {
            if output.status.success() {
                log_deletion("Stopped coresymbolicationd daemon (with privileges)");
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("User canceled") {
                    Err(CleanError::PermissionDenied("User cancelled authentication".to_string()))
                } else {
                    Err(CleanError::DaemonKillFailed(stderr.to_string()))
                }
            }
        }
        Err(e) => Err(CleanError::DaemonKillFailed(e.to_string())),
    }
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

    log_deletion(&format!(
        "=== {} STARTED ===",
        if dry_run { "DRY RUN" } else { "CLEAN OPERATION" }
    ));
    log_deletion(&format!("Target path: {}", cache_path.display()));

    // Check if cache exists
    if !cache_path.exists() {
        log_deletion("Cache directory does not exist - nothing to clean");
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

    // ACTUAL DELETION - Stop daemon first
    if let Err(e) = stop_daemon() {
        log_deletion(&format!("Warning: Could not stop daemon: {}", e));
        // Continue anyway - daemon might not be running
    }

    // Wait for daemon to stop
    std::thread::sleep(std::time::Duration::from_secs(1));

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

/// Reindex Spotlight (optional, helps clean orphaned APFS document IDs)
pub fn reindex_spotlight() -> Result<(), CleanError> {
    log_deletion("Requesting Spotlight reindex");

    let script = r#"do shell script "mdutil -E /" with administrator privileges"#;

    let result = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output();

    match result {
        Ok(output) => {
            if output.status.success() {
                log_deletion("Spotlight reindex initiated");
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("User canceled") {
                    Err(CleanError::PermissionDenied("User cancelled authentication".to_string()))
                } else {
                    Ok(()) // Don't fail on this - it's optional
                }
            }
        }
        Err(_) => Ok(()), // Don't fail on this - it's optional
    }
}

/// Get the log file path (for UI display)
pub fn get_log_file_path() -> String {
    get_log_path().to_string_lossy().to_string()
}
