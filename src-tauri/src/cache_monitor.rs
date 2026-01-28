use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Cache status thresholds in bytes
pub const WARNING_THRESHOLD: u64 = 5 * 1024 * 1024 * 1024; // 5GB
pub const CRITICAL_THRESHOLD: u64 = 10 * 1024 * 1024 * 1024; // 10GB

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheState {
    Normal,
    Warning,
    Critical,
}

impl CacheState {
    pub fn from_size(size_bytes: u64) -> Self {
        if size_bytes >= CRITICAL_THRESHOLD {
            CacheState::Critical
        } else if size_bytes >= WARNING_THRESHOLD {
            CacheState::Warning
        } else {
            CacheState::Normal
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            CacheState::Normal => "normal",
            CacheState::Warning => "warning",
            CacheState::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStatus {
    pub size_bytes: u64,
    pub size_display: String,
    pub state: CacheState,
    pub path: String,
    pub exists: bool,
    pub file_count: u64,
    pub last_checked: u64,
}

impl Default for CacheStatus {
    fn default() -> Self {
        Self {
            size_bytes: 0,
            size_display: "0 B".to_string(),
            state: CacheState::Normal,
            path: get_cache_path().to_string_lossy().to_string(),
            exists: false,
            file_count: 0,
            last_checked: current_timestamp(),
        }
    }
}

/// Get the coresymbolicationd cache path
/// Note: There are two possible locations:
/// - User cache: ~/Library/Caches/com.apple.coresymbolicationd
/// - System cache: /System/Library/Caches/com.apple.coresymbolicationd (requires root)
pub fn get_cache_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users".to_string());
    PathBuf::from(home).join("Library/Caches/com.apple.coresymbolicationd")
}

/// Get the system-level cache path (requires elevated privileges)
pub fn get_system_cache_path() -> PathBuf {
    PathBuf::from("/System/Library/Caches/com.apple.coresymbolicationd")
}

/// Get current Unix timestamp
fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

/// Format bytes into human-readable string
/// Shows GB when ≥1000 MB, MB for 1-999, KB for small, B for tiny
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const GB_THRESHOLD: u64 = 1000 * MB; // Show GB at 1000 MB, not 1024

    if bytes >= GB_THRESHOLD {
        // Show as GB, omit decimal if whole number
        let value = bytes as f64 / GB as f64;
        let rounded = (value * 10.0).round() / 10.0; // Round to 1 decimal
        if (rounded - rounded.floor()).abs() < 0.01 {
            format!("{:.0} GB", rounded) // Whole number: "1 GB", "2 GB"
        } else {
            format!("{:.1} GB", rounded) // Decimal: "1.5 GB", "2.3 GB"
        }
    } else if bytes >= MB {
        // Show as MB (1-999 range)
        let value = bytes / MB;
        format!("{} MB", value)
    } else if bytes >= KB {
        // Show as KB
        format!("{} KB", bytes / KB)
    } else if bytes > 0 {
        format!("{} B", bytes)
    } else {
        "0 B".to_string()
    }
}

/// Add commas to numbers (e.g., 1250 -> "1,250")
fn format_with_commas(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

/// Calculate directory size recursively
fn calculate_dir_size(path: &PathBuf) -> (u64, u64) {
    let mut total_size: u64 = 0;
    let mut file_count: u64 = 0;

    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                let (sub_size, sub_count) = calculate_dir_size(&entry_path);
                total_size += sub_size;
                file_count += sub_count;
            } else if let Ok(metadata) = entry.metadata() {
                total_size += metadata.len();
                file_count += 1;
            }
        }
    }

    (total_size, file_count)
}

/// Get cache status using native Rust filesystem operations
pub fn get_cache_status() -> CacheStatus {
    let cache_path = get_cache_path();
    let exists = cache_path.exists();

    if !exists {
        return CacheStatus {
            exists: false,
            path: cache_path.to_string_lossy().to_string(),
            last_checked: current_timestamp(),
            ..Default::default()
        };
    }

    let (size_bytes, file_count) = calculate_dir_size(&cache_path);
    let state = CacheState::from_size(size_bytes);
    let size_display = format_size(size_bytes);

    CacheStatus {
        size_bytes,
        size_display,
        state,
        path: cache_path.to_string_lossy().to_string(),
        exists,
        file_count,
        last_checked: current_timestamp(),
    }
}

/// Check if coresymbolicationd daemon is running
pub fn is_daemon_running() -> bool {
    let output = Command::new("pgrep")
        .arg("-x")
        .arg("coresymbolicationd")
        .output();

    match output {
        Ok(result) => result.status.success(),
        Err(_) => false,
    }
}

/// Get combined cache status (user + system if accessible)
pub fn get_combined_cache_status() -> CacheStatus {
    let user_status = get_cache_status();

    // Try to also check system cache (may fail without privileges)
    let system_path = get_system_cache_path();
    let system_size = if system_path.exists() {
        let (size, _) = calculate_dir_size(&system_path);
        size
    } else {
        0
    };

    let total_size = user_status.size_bytes + system_size;
    let state = CacheState::from_size(total_size);

    CacheStatus {
        size_bytes: total_size,
        size_display: format_size(total_size),
        state,
        file_count: user_status.file_count,
        ..user_status
    }
}

/// Create a simulated cache status for debug/testing purposes
pub fn get_simulated_status(size_bytes: u64) -> CacheStatus {
    let state = CacheState::from_size(size_bytes);
    let size_display = format_size(size_bytes);

    CacheStatus {
        size_bytes,
        size_display,
        state,
        path: "[Debug Mode]".to_string(),
        exists: true,
        file_count: (size_bytes / (1024 * 1024)) as u64, // Fake ~1 file per MB
        last_checked: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(500), "500 B");
        assert_eq!(format_size(1024), "1 KB");
        assert_eq!(format_size(1048576), "1 MB");
        assert_eq!(format_size(500 * 1024 * 1024), "500 MB");
        assert_eq!(format_size(999 * 1024 * 1024), "999 MB");
        // 1000 MB should show as GB (threshold)
        assert_eq!(format_size(1000 * 1024 * 1024), "1 GB");
        assert_eq!(format_size(1073741824), "1 GB"); // 1024 MB = 1 GB
        assert_eq!(format_size(5368709120), "5 GB");
    }

    #[test]
    fn test_cache_state_from_size() {
        assert_eq!(CacheState::from_size(0), CacheState::Normal);
        assert_eq!(CacheState::from_size(4 * 1024 * 1024 * 1024), CacheState::Normal);
        assert_eq!(CacheState::from_size(5 * 1024 * 1024 * 1024), CacheState::Warning);
        assert_eq!(CacheState::from_size(7 * 1024 * 1024 * 1024), CacheState::Warning);
        assert_eq!(CacheState::from_size(10 * 1024 * 1024 * 1024), CacheState::Critical);
        assert_eq!(CacheState::from_size(15 * 1024 * 1024 * 1024), CacheState::Critical);
    }
}
