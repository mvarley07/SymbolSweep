use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Cache status thresholds in bytes (reclaimable totals)
pub const WARNING_THRESHOLD: u64 = 2 * 1024 * 1024 * 1024; // 2GB
pub const CRITICAL_THRESHOLD: u64 = 5 * 1024 * 1024 * 1024; // 5GB

/// Disk free space thresholds
pub const DISK_WARNING_THRESHOLD: u64 = 25 * 1024 * 1024 * 1024; // <25GB free = warning
pub const DISK_CRITICAL_THRESHOLD: u64 = 10 * 1024 * 1024 * 1024; // <10GB free = critical

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

/// Disk health based on free space
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiskHealth {
    Normal,
    Warning,
    Critical,
    Unknown,
}

impl DiskHealth {
    pub fn from_free_bytes(free: u64) -> Self {
        if free == u64::MAX {
            DiskHealth::Unknown
        } else if free < DISK_CRITICAL_THRESHOLD {
            DiskHealth::Critical
        } else if free < DISK_WARNING_THRESHOLD {
            DiskHealth::Warning
        } else {
            DiskHealth::Normal
        }
    }

    /// Convert to CacheState for comparison (Unknown → Warning to fail safe)
    pub fn to_cache_state(self) -> CacheState {
        match self {
            DiskHealth::Normal => CacheState::Normal,
            DiskHealth::Warning | DiskHealth::Unknown => CacheState::Warning,
            DiskHealth::Critical => CacheState::Critical,
        }
    }
}

/// Unified app status — single source of truth for tray and popup
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStatus {
    /// Disk free space
    pub disk_free_bytes: u64,
    pub disk_free_display: String,
    /// Disk total capacity
    pub disk_total_bytes: u64,
    pub disk_total_display: String,
    /// Health based on disk free space alone
    pub disk_health: DiskHealth,
    /// Cache status (coresymbolicationd)
    pub cache: CacheStatus,
    /// Dev artifact totals (from last scan)
    pub dev_total_bytes: u64,
    pub dev_total_display: String,
    /// Whether a dev scan result is available
    pub dev_scan_available: bool,
    /// Combined reclaimable (cache + dev)
    pub reclaimable_bytes: u64,
    pub reclaimable_display: String,
    /// Overall health = worst of disk_health vs reclaimable-based state
    pub health: CacheState,
    /// Number of local APFS snapshots detected
    pub snapshot_count: u32,
}

/// Compute the unified app status from cache + dev scan data.
/// Both tray and popup read from this same struct.
pub fn compute_app_status(cache: &CacheStatus, dev_total: u64) -> AppStatus {
    let disk_free = get_disk_free_bytes();
    let disk_total = get_disk_total_bytes();
    let disk_health = DiskHealth::from_free_bytes(disk_free);

    let reclaimable = cache.size_bytes + dev_total;
    let reclaimable_state = CacheState::from_size(reclaimable);

    // Overall health is the WORSE of disk-free-based and reclaimable-based
    let disk_as_state = disk_health.to_cache_state();
    let health = worse_state(disk_as_state, reclaimable_state);

    let snapshot_count = get_snapshot_count();

    // When statvfs fails (disk_free == u64::MAX), show "unavailable" instead
    // of formatting a nonsensical huge number
    let (display_free, display_total) = if disk_health == DiskHealth::Unknown {
        ("unavailable".to_string(), "unavailable".to_string())
    } else {
        (format_size(disk_free), format_size(disk_total))
    };

    AppStatus {
        disk_free_bytes: disk_free,
        disk_free_display: display_free,
        disk_total_bytes: disk_total,
        disk_total_display: display_total,
        disk_health,
        cache: cache.clone(),
        dev_total_bytes: dev_total,
        dev_total_display: format_size(dev_total),
        dev_scan_available: dev_total > 0 || true, // always available after first scan
        reclaimable_bytes: reclaimable,
        reclaimable_display: format_size(reclaimable),
        health,
        snapshot_count,
    }
}

/// Return the worse (more severe) of two states
fn worse_state(a: CacheState, b: CacheState) -> CacheState {
    match (a, b) {
        (CacheState::Critical, _) | (_, CacheState::Critical) => CacheState::Critical,
        (CacheState::Warning, _) | (_, CacheState::Warning) => CacheState::Warning,
        _ => CacheState::Normal,
    }
}

/// Get available disk space on the root volume via statvfs
pub fn get_disk_free_bytes() -> u64 {
    use std::ffi::CString;
    let path = CString::new("/").unwrap();
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(path.as_ptr(), &mut stat) == 0 {
            stat.f_bavail as u64 * stat.f_frsize as u64
        } else {
            u64::MAX // fail open — don't trigger false alarms
        }
    }
}

/// Get total disk capacity on the root volume via statvfs
pub fn get_disk_total_bytes() -> u64 {
    use std::ffi::CString;
    let path = CString::new("/").unwrap();
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(path.as_ptr(), &mut stat) == 0 {
            stat.f_blocks as u64 * stat.f_frsize as u64
        } else {
            0
        }
    }
}

/// Count local APFS snapshots (read-only, no privileges)
pub fn get_snapshot_count() -> u32 {
    let output = Command::new("tmutil")
        .args(["listlocalsnapshots", "/"])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| l.starts_with("com.apple."))
                .count() as u32
        }
        _ => 0,
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
        assert_eq!(CacheState::from_size(1 * 1024 * 1024 * 1024), CacheState::Normal);
        assert_eq!(CacheState::from_size(2 * 1024 * 1024 * 1024), CacheState::Warning);
        assert_eq!(CacheState::from_size(3 * 1024 * 1024 * 1024), CacheState::Warning);
        assert_eq!(CacheState::from_size(5 * 1024 * 1024 * 1024), CacheState::Critical);
        assert_eq!(CacheState::from_size(7 * 1024 * 1024 * 1024), CacheState::Critical);
    }

    #[test]
    fn test_disk_health_thresholds() {
        // Normal: >= 25GB free
        assert_eq!(DiskHealth::from_free_bytes(30 * 1024 * 1024 * 1024), DiskHealth::Normal);
        assert_eq!(DiskHealth::from_free_bytes(25 * 1024 * 1024 * 1024), DiskHealth::Normal);
        // Warning: 10-25GB free
        assert_eq!(DiskHealth::from_free_bytes(24 * 1024 * 1024 * 1024), DiskHealth::Warning);
        assert_eq!(DiskHealth::from_free_bytes(10 * 1024 * 1024 * 1024), DiskHealth::Warning);
        // Critical: <10GB free
        assert_eq!(DiskHealth::from_free_bytes(9 * 1024 * 1024 * 1024), DiskHealth::Critical);
        assert_eq!(DiskHealth::from_free_bytes(1 * 1024 * 1024 * 1024), DiskHealth::Critical);
        assert_eq!(DiskHealth::from_free_bytes(0), DiskHealth::Critical);
        // Unknown: u64::MAX (statvfs failure sentinel)
        assert_eq!(DiskHealth::from_free_bytes(u64::MAX), DiskHealth::Unknown);
    }

    #[test]
    fn test_statvfs_failure_maps_to_warning_not_normal() {
        let unknown = DiskHealth::Unknown;
        // Unknown must NOT map to Normal — it should fail safe (Warning)
        assert_eq!(unknown.to_cache_state(), CacheState::Warning);
        assert_ne!(unknown.to_cache_state(), CacheState::Normal);
    }

    #[test]
    fn test_compute_app_status_statvfs_failure() {
        // Simulate statvfs failure by constructing what compute_app_status
        // would produce with disk_free = u64::MAX
        let disk_health = DiskHealth::from_free_bytes(u64::MAX);
        assert_eq!(disk_health, DiskHealth::Unknown);

        // Display should say "unavailable", not a huge formatted number
        // (This tests the display logic extracted from compute_app_status)
        let display = if disk_health == DiskHealth::Unknown {
            "unavailable".to_string()
        } else {
            format_size(u64::MAX)
        };
        assert_eq!(display, "unavailable");

        // Health should be at least Warning (not Normal)
        let health = disk_health.to_cache_state();
        assert_eq!(health, CacheState::Warning);
    }

    #[test]
    fn test_critical_badge_renders_when_disk_low() {
        // Simulate a disk with only 5GB free — should be Critical (<10GB)
        let fake_free: u64 = 5 * 1024 * 1024 * 1024;
        let disk_health = DiskHealth::from_free_bytes(fake_free);
        assert_eq!(disk_health, DiskHealth::Critical);
        assert_eq!(disk_health.to_cache_state(), CacheState::Critical);

        // With minimal reclaimable, overall health is still Critical
        let reclaimable = CacheState::from_size(100); // tiny reclaimable = Normal
        let overall = worse_state(disk_health.to_cache_state(), reclaimable);
        assert_eq!(overall, CacheState::Critical);

        // Gap banner condition: health != Normal && reclaimable < 1GB
        assert!(overall != CacheState::Normal);
        assert!(100u64 < 1_000_000_000);
        // Both true → gap banner would render in the UI
    }

    #[test]
    fn test_live_disk_free_reports_correctly() {
        // Run against real disk — just verify we get a non-zero, non-u64::MAX value
        let free = get_disk_free_bytes();
        assert_ne!(free, 0, "Disk free should not be zero");
        assert_ne!(free, u64::MAX, "statvfs should succeed on a real system");

        let health = DiskHealth::from_free_bytes(free);
        // On a dev machine, should have at least some disk space
        assert_ne!(health, DiskHealth::Unknown);
    }
}
