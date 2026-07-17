use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Cache status thresholds in bytes (reclaimable totals)
pub const WARNING_THRESHOLD: u64 = 2 * 1024 * 1024 * 1024; // 2GB
pub const CRITICAL_THRESHOLD: u64 = 5 * 1024 * 1024 * 1024; // 5GB

/// Disk free space thresholds
pub const DISK_WARNING_THRESHOLD: u64 = 25 * 1024 * 1024 * 1024; // <25GB free = warning
pub const DISK_CRITICAL_THRESHOLD: u64 = 10 * 1024 * 1024 * 1024; // <10GB free = critical

/// Display smoothing: only update the shown disk-free number when the reading
/// changes by more than this amount. Prevents visible jitter from normal APFS
/// churn (snapshots, purgeable reclamation) while keeping big changes responsive.
const DISK_FREE_DISPLAY_THRESHOLD: u64 = 512 * 1024 * 1024; // 0.5 GB
static LAST_DISPLAYED_DISK_FREE: Mutex<u64> = Mutex::new(0);

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

/// Cleanup state — driven by reclaimable size, not disk pressure.
/// This is a cleanup signal ("here's what's piled up"), not a data-risk alarm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CleanState {
    /// < 1 GB reclaimable — nothing meaningful to sweep
    Clean,
    /// 1–10 GB reclaimable — some junk, actionable
    Moderate,
    /// >= 10 GB reclaimable — significant junk, worth a sweep
    Heavy,
    /// Cache alone >= 20 GB — coresymbolicationd is running away
    Runaway,
}

/// Reclaimable size thresholds for cleanup states
pub const CLEAN_MODERATE_THRESHOLD: u64 = 1 * 1024 * 1024 * 1024; // 1 GB
pub const CLEAN_HEAVY_THRESHOLD: u64 = 10 * 1024 * 1024 * 1024; // 10 GB

/// Cache-specific threshold: coresymbolicationd alone at this size is a runaway.
pub const RUNAWAY_THRESHOLD: u64 = 20 * 1024 * 1024 * 1024; // 20 GB

impl CleanState {
    pub fn from_reclaimable(bytes: u64) -> Self {
        if bytes >= CLEAN_HEAVY_THRESHOLD {
            CleanState::Heavy
        } else if bytes >= CLEAN_MODERATE_THRESHOLD {
            CleanState::Moderate
        } else {
            CleanState::Clean
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            CleanState::Clean => "clean",
            CleanState::Moderate => "moderate",
            CleanState::Heavy => "heavy",
            CleanState::Runaway => "runaway",
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
    /// Cleanup state — driven by reclaimable size, not disk pressure
    pub clean_state: CleanState,
    /// Whether the disk-gap banner should show (disk low + SS can't fix most of it)
    pub show_gap_banner: bool,
    /// Number of local APFS snapshots detected
    pub snapshot_count: u32,
    /// Whether the first dev artifact scan has completed
    pub dev_scan_complete: bool,
    /// Whether autoclean has failed 3+ consecutive times
    pub autoclean_failing: bool,
}

/// Compute the unified app status from cache + dev scan data.
/// Both tray and popup read from this same struct.
pub fn compute_app_status(cache: &CacheStatus, dev_total: u64, dev_scan_complete: bool, consecutive_autoclean_failures: u32) -> AppStatus {
    let disk_free = get_disk_free_bytes();
    let disk_total = get_disk_total_bytes();
    let disk_health = DiskHealth::from_free_bytes(disk_free);

    let reclaimable = cache.size_bytes + dev_total;

    // Cleanup state driven by reclaimable size — not disk pressure.
    // A power user with a large steady-state dev cache should see an
    // informational "here's what's available" signal, not a panic state.
    let clean_state = CleanState::from_reclaimable(reclaimable);

    // Override: if the cache ALONE is >= 20 GB, this is a runaway.
    // This is the exact emergency SS was built to prevent — keyed off
    // cache.size_bytes, not the combined total, so dev artifacts can't mask it.
    let clean_state = if cache.size_bytes >= RUNAWAY_THRESHOLD {
        CleanState::Runaway
    } else {
        clean_state
    };

    // Gap banner: disk is genuinely low AND SS can't fix most of it.
    // On a healthy disk (>= 25 GB free), the banner never fires regardless
    // of reclaimable size.
    let show_gap_banner = matches!(disk_health, DiskHealth::Warning | DiskHealth::Critical)
        && reclaimable < 1_000_000_000;

    let snapshot_count = get_snapshot_count();

    // Display smoothing: only update the shown disk-free value when the reading
    // moves by more than 0.5 GB from the last displayed value. The raw
    // disk_free_bytes stays accurate for DiskHealth / gap-banner logic.
    //
    // Smoothing is disabled when disk health is Warning/Critical — accuracy
    // matters more than stability when the user is monitoring a low-disk
    // situation, and it prevents the displayed number contradicting the
    // gap banner (e.g. banner says "low" while smoothed number shows "fine").
    let displayed_free = if disk_health == DiskHealth::Unknown {
        disk_free // will be formatted as "unavailable" below
    } else {
        let mut last = LAST_DISPLAYED_DISK_FREE.lock().unwrap();
        let snap = *last == 0
            || disk_health != DiskHealth::Normal
            || disk_free.abs_diff(*last) >= DISK_FREE_DISPLAY_THRESHOLD;
        if snap {
            *last = disk_free;
        }
        *last
    };

    let (display_free, display_total) = if disk_health == DiskHealth::Unknown {
        ("unavailable".to_string(), "unavailable".to_string())
    } else {
        (format_size(displayed_free), format_size(disk_total))
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
        dev_scan_available: true,
        reclaimable_bytes: reclaimable,
        reclaimable_display: format_size(reclaimable),
        clean_state,
        show_gap_banner,
        snapshot_count,
        dev_scan_complete,
        autoclean_failing: consecutive_autoclean_failures >= 3,
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
        assert_eq!(format_size(1000 * 1024 * 1024), "1 GB");
        assert_eq!(format_size(1073741824), "1 GB");
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
    fn test_clean_state_from_reclaimable() {
        // Clean: < 1 GB
        assert_eq!(CleanState::from_reclaimable(0), CleanState::Clean);
        assert_eq!(CleanState::from_reclaimable(500 * 1024 * 1024), CleanState::Clean);
        // Moderate: 1-10 GB
        assert_eq!(CleanState::from_reclaimable(1 * 1024 * 1024 * 1024), CleanState::Moderate);
        assert_eq!(CleanState::from_reclaimable(5 * 1024 * 1024 * 1024), CleanState::Moderate);
        assert_eq!(CleanState::from_reclaimable(9 * 1024 * 1024 * 1024), CleanState::Moderate);
        // Heavy: >= 10 GB
        assert_eq!(CleanState::from_reclaimable(10 * 1024 * 1024 * 1024), CleanState::Heavy);
        assert_eq!(CleanState::from_reclaimable(13 * 1024 * 1024 * 1024), CleanState::Heavy);
        assert_eq!(CleanState::from_reclaimable(50 * 1024 * 1024 * 1024), CleanState::Heavy);
    }

    #[test]
    fn test_disk_health_thresholds() {
        assert_eq!(DiskHealth::from_free_bytes(30 * 1024 * 1024 * 1024), DiskHealth::Normal);
        assert_eq!(DiskHealth::from_free_bytes(25 * 1024 * 1024 * 1024), DiskHealth::Normal);
        assert_eq!(DiskHealth::from_free_bytes(24 * 1024 * 1024 * 1024), DiskHealth::Warning);
        assert_eq!(DiskHealth::from_free_bytes(10 * 1024 * 1024 * 1024), DiskHealth::Warning);
        assert_eq!(DiskHealth::from_free_bytes(9 * 1024 * 1024 * 1024), DiskHealth::Critical);
        assert_eq!(DiskHealth::from_free_bytes(1 * 1024 * 1024 * 1024), DiskHealth::Critical);
        assert_eq!(DiskHealth::from_free_bytes(0), DiskHealth::Critical);
        assert_eq!(DiskHealth::from_free_bytes(u64::MAX), DiskHealth::Unknown);
    }

    #[test]
    fn test_statvfs_failure_display() {
        let disk_health = DiskHealth::from_free_bytes(u64::MAX);
        assert_eq!(disk_health, DiskHealth::Unknown);

        let display = if disk_health == DiskHealth::Unknown {
            "unavailable".to_string()
        } else {
            format_size(u64::MAX)
        };
        assert_eq!(display, "unavailable");
    }

    #[test]
    fn test_48gb_free_13gb_reclaimable_calm_actionable() {
        // Scenario: healthy disk (48 GB free), lots of reclaimable junk (13 GB)
        let disk_free: u64 = 48 * 1024 * 1024 * 1024;
        let reclaimable: u64 = 13 * 1024 * 1024 * 1024;

        // Badge must be Heavy (calm actionable), NOT any panic state
        let clean_state = CleanState::from_reclaimable(reclaimable);
        assert_eq!(clean_state, CleanState::Heavy,
            "13 GB reclaimable should be Heavy (calm actionable), not any panic state");

        // Disk health is Normal — disk is fine
        let disk_health = DiskHealth::from_free_bytes(disk_free);
        assert_eq!(disk_health, DiskHealth::Normal);

        // Gap banner must NOT fire (disk is healthy)
        let gap_banner = matches!(disk_health, DiskHealth::Warning | DiskHealth::Critical)
            && reclaimable < 1_000_000_000;
        assert!(!gap_banner, "Gap banner must NOT fire with 48 GB free");
    }

    #[test]
    fn test_8gb_free_13gb_reclaimable_actionable_no_gap_banner() {
        // Scenario: low disk (8 GB free), but SS has plenty to clean (13 GB)
        let disk_free: u64 = 8 * 1024 * 1024 * 1024;
        let reclaimable: u64 = 13 * 1024 * 1024 * 1024;

        // Badge: 13 GB reclaimable -> Heavy
        let clean_state = CleanState::from_reclaimable(reclaimable);
        assert_eq!(clean_state, CleanState::Heavy,
            "13 GB reclaimable should be Heavy");

        // Disk health: 8 GB free -> Critical (disk is genuinely low)
        let disk_health = DiskHealth::from_free_bytes(disk_free);
        assert_eq!(disk_health, DiskHealth::Critical);

        // Gap banner must NOT fire — SS has plenty to offer (13 GB >= 1 GB)
        let gap_banner = matches!(disk_health, DiskHealth::Warning | DiskHealth::Critical)
            && reclaimable < 1_000_000_000;
        assert!(!gap_banner,
            "Gap banner must NOT fire when SS has 13 GB to clean — SS can help");

        // Disk-free line SHOULD show low state (disk_health != Normal)
        assert_ne!(disk_health, DiskHealth::Normal,
            "Disk-free context line should show low-state coloring");
    }

    #[test]
    fn test_8gb_free_200mb_reclaimable_gap_banner_fires() {
        // Scenario: low disk (8 GB free), SS can't help much (200 MB)
        let disk_free: u64 = 8 * 1024 * 1024 * 1024;
        let reclaimable: u64 = 200 * 1024 * 1024;

        let clean_state = CleanState::from_reclaimable(reclaimable);
        assert_eq!(clean_state, CleanState::Clean);

        let disk_health = DiskHealth::from_free_bytes(disk_free);
        assert_eq!(disk_health, DiskHealth::Critical);

        // Gap banner SHOULD fire — disk low AND SS can't fix it
        let gap_banner = matches!(disk_health, DiskHealth::Warning | DiskHealth::Critical)
            && reclaimable < 1_000_000_000;
        assert!(gap_banner,
            "Gap banner should fire when disk is low and SS has little to clean");
    }

    #[test]
    fn test_live_disk_free_reports_correctly() {
        let free = get_disk_free_bytes();
        assert_ne!(free, 0, "Disk free should not be zero");
        assert_ne!(free, u64::MAX, "statvfs should succeed on a real system");

        let health = DiskHealth::from_free_bytes(free);
        assert_ne!(health, DiskHealth::Unknown);
    }

    /// Helper to construct a test CacheStatus with a given size
    fn test_cache(size_bytes: u64) -> CacheStatus {
        CacheStatus {
            size_bytes,
            size_display: format_size(size_bytes),
            state: CacheState::from_size(size_bytes),
            path: "[Test]".to_string(),
            exists: true,
            file_count: 100,
            last_checked: 0,
        }
    }

    #[test]
    fn test_runaway_21gb_cache_no_dev() {
        let status = compute_app_status(&test_cache(21 * 1024 * 1024 * 1024), 0, true, 0);
        assert_eq!(status.clean_state, CleanState::Runaway,
            "21 GB cache alone should be Runaway");
    }

    #[test]
    fn test_runaway_21gb_cache_with_15gb_dev() {
        let status = compute_app_status(
            &test_cache(21 * 1024 * 1024 * 1024),
            15 * 1024 * 1024 * 1024,
            true,
            0,
        );
        assert_eq!(status.clean_state, CleanState::Runaway,
            "21 GB cache + 15 GB dev should still be Runaway (cache-specific, not combined)");
    }

    #[test]
    fn test_19gb_cache_is_not_runaway() {
        let status = compute_app_status(&test_cache(19 * 1024 * 1024 * 1024), 0, true, 0);
        assert_eq!(status.clean_state, CleanState::Heavy,
            "19 GB cache should be Heavy, not Runaway");
    }

    #[test]
    fn test_20gb_boundary_is_runaway() {
        let status = compute_app_status(&test_cache(20 * 1024 * 1024 * 1024), 0, true, 0);
        assert_eq!(status.clean_state, CleanState::Runaway,
            "Exactly 20 GB cache should be Runaway (>= threshold)");
    }

    #[test]
    fn test_autoclean_failing_under_threshold() {
        let status = compute_app_status(&test_cache(0), 0, true, 2);
        assert!(!status.autoclean_failing,
            "2 consecutive failures should NOT trigger the banner");
    }

    #[test]
    fn test_autoclean_failing_at_threshold() {
        let status = compute_app_status(&test_cache(0), 0, true, 3);
        assert!(status.autoclean_failing,
            "3 consecutive failures should trigger the banner");
    }

    #[test]
    fn test_autoclean_failing_above_threshold() {
        let status = compute_app_status(&test_cache(0), 0, true, 10);
        assert!(status.autoclean_failing,
            "10 consecutive failures should trigger the banner");
    }
}
