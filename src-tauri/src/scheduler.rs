use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::cache_cleaner::{clean_cache, CleanResult};
use crate::cache_monitor::{get_cache_status, CacheState, CRITICAL_THRESHOLD, WARNING_THRESHOLD};

/// Settings for auto-clean behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Enable auto-clean when threshold is reached
    pub auto_clean_on_threshold: bool,
    /// Threshold in bytes for auto-clean (default: 5GB)
    pub auto_clean_threshold: u64,
    /// Enable scheduled auto-clean
    pub auto_clean_scheduled: bool,
    /// Interval in seconds for scheduled clean (default: 6 hours)
    pub auto_clean_interval_secs: u64,
    /// Show notifications
    pub show_notifications: bool,
    /// Launch at login
    pub launch_at_login: bool,
    /// Last clean timestamp
    pub last_clean_timestamp: u64,
    /// Monitoring interval in seconds
    pub monitor_interval_secs: u64,
    /// Debug mode - simulate cache sizes
    #[serde(default)]
    pub debug_mode: bool,
    /// Simulated cache size in bytes (only used when debug_mode is true)
    #[serde(default)]
    pub debug_simulated_size: u64,
    /// First run completed - hide welcome screen after first launch
    #[serde(default)]
    pub first_run_completed: bool,
    /// First clean confirmed - user has acknowledged the safety message
    #[serde(default)]
    pub first_clean_confirmed: bool,
    /// Project root directories to scan for dev artifacts
    #[serde(default = "default_dev_scan_roots")]
    pub dev_scan_roots: Vec<String>,
    /// Timestamp of last clean that actually freed >0 bytes (for UI display)
    #[serde(default)]
    pub last_real_clean_timestamp: u64,
    /// Bytes freed in the last real clean (for UI summary line)
    #[serde(default)]
    pub last_real_clean_freed: u64,
    /// Consecutive autoclean failure count (persisted across restarts)
    #[serde(default)]
    pub consecutive_autoclean_failures: u32,
    /// License key (stored after successful activation)
    #[serde(default)]
    pub license_key: Option<String>,
    /// Instance ID returned by LemonSqueezy on activation
    #[serde(default)]
    pub license_instance_id: Option<String>,
    /// Unix timestamp of last successful license validation
    #[serde(default)]
    pub license_last_validated: u64,
}

fn default_dev_scan_roots() -> Vec<String> {
    crate::dev_scanner::default_scan_roots()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_clean_on_threshold: true,
            auto_clean_threshold: CRITICAL_THRESHOLD,
            auto_clean_scheduled: false,
            auto_clean_interval_secs: 6 * 60 * 60, // 6 hours
            show_notifications: true,
            launch_at_login: false,
            last_clean_timestamp: 0,
            monitor_interval_secs: 60, // 1 minute
            debug_mode: false,
            debug_simulated_size: 0,
            first_run_completed: false,
            first_clean_confirmed: false,
            dev_scan_roots: default_dev_scan_roots(),
            last_real_clean_timestamp: 0,
            last_real_clean_freed: 0,
            consecutive_autoclean_failures: 0,
            license_key: None,
            license_instance_id: None,
            license_last_validated: 0,
        }
    }
}

impl Settings {
    /// Get the settings file path
    fn file_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/Users".to_string());
        PathBuf::from(home)
            .join("Library/Application Support/com.mvarley07.symbolsweep")
            .join("settings.json")
    }

    /// Load settings from disk
    pub fn load() -> Self {
        let path = Self::file_path();
        let mut settings = if path.exists() {
            match fs::read_to_string(&path) {
                Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
                Err(_) => Self::default(),
            }
        } else {
            Self::default()
        };

        // Clamp stale debug-only intervals: if debug mode is off but interval
        // is below the minimum non-debug value (1 hour), reset to 1 hour.
        // This fixes intervals left over from a previous debug session.
        if !settings.debug_mode && settings.auto_clean_interval_secs < 3600 {
            settings.auto_clean_interval_secs = 3600;
            let _ = settings.save();
        }

        settings
    }

    /// Save settings to disk
    pub fn save(&self) -> Result<(), String> {
        let path = Self::file_path();

        // Ensure directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create settings directory: {}", e))?;
        }

        let content = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize settings: {}", e))?;

        fs::write(&path, content).map_err(|e| format!("Failed to write settings: {}", e))
    }

    /// Update clean timestamps. Always updates scheduling timestamp.
    /// Only updates the "real clean" display fields when bytes_freed > 0.
    /// Resets the autoclean failure counter — any successful clean clears the alarm.
    pub fn record_clean(&mut self, bytes_freed: u64) {
        self.last_clean_timestamp = current_timestamp();
        self.consecutive_autoclean_failures = 0;
        if bytes_freed > 0 {
            self.last_real_clean_timestamp = current_timestamp();
            self.last_real_clean_freed = bytes_freed;
        }
        let _ = self.save();
    }

    /// Record an autoclean failure (increment counter, persist).
    pub fn record_autoclean_failure(&mut self) {
        self.consecutive_autoclean_failures += 1;
        let _ = self.save();
    }

    /// A key and this Mac's activation are stored, and the key has passed a
    /// validation with Lemon Squeezy (activation counts). A key rejected on
    /// revalidation is cleared, so it stops counting; a revalidation that
    /// can't reach the server keeps it (fail open).
    pub fn is_licensed(&self) -> bool {
        self.license_key.is_some() && self.license_instance_id.is_some() && self.license_last_validated > 0
    }

    /// Free scan mode: scanning is free, cleaning and deleting need a license.
    /// A dry run deletes nothing, so it never does.
    pub fn require_license(&self, dry_run: bool) -> Result<(), String> {
        if dry_run || self.is_licensed() {
            Ok(())
        } else {
            Err(LICENSE_REQUIRED.to_string())
        }
    }

    /// Whether automatic cache cleaning should run now. Licensed only.
    pub fn autoclean_due(&self, cache_size_bytes: u64, now: u64) -> bool {
        if !self.is_licensed() {
            return false;
        }
        let threshold_clean = self.auto_clean_on_threshold && cache_size_bytes >= self.auto_clean_threshold;
        let scheduled_clean = self.auto_clean_scheduled
            && now.saturating_sub(self.last_clean_timestamp) >= self.auto_clean_interval_secs;
        threshold_clean || scheduled_clean
    }
}

/// The error a clean or delete returns without a license; the UI shows the unlock sheet
pub const LICENSE_REQUIRED: &str = "license_required";

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

/// Scheduler state
pub struct Scheduler {
    settings: Arc<Mutex<Settings>>,
    running: Arc<Mutex<bool>>,
}

impl Scheduler {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings: Arc::new(Mutex::new(settings)),
            running: Arc::new(Mutex::new(false)),
        }
    }

    /// Get current settings
    pub fn get_settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    /// Update settings
    pub fn update_settings(&self, new_settings: Settings) -> Result<(), String> {
        let mut settings = self.settings.lock().unwrap();
        *settings = new_settings;
        settings.save()
    }

    /// Check if auto-clean should run based on threshold
    pub fn should_auto_clean_threshold(&self) -> bool {
        let settings = self.settings.lock().unwrap();
        if !settings.auto_clean_on_threshold {
            return false;
        }

        let status = get_cache_status();
        status.size_bytes >= settings.auto_clean_threshold
    }

    /// Check if scheduled auto-clean should run
    pub fn should_auto_clean_scheduled(&self) -> bool {
        let settings = self.settings.lock().unwrap();
        if !settings.auto_clean_scheduled {
            return false;
        }

        let now = current_timestamp();
        let elapsed = now.saturating_sub(settings.last_clean_timestamp);
        elapsed >= settings.auto_clean_interval_secs
    }

    /// Perform auto-clean if conditions are met
    /// Returns Some(CleanResult) if clean was performed, None otherwise
    pub fn check_and_auto_clean(&self) -> Option<CleanResult> {
        let should_clean = self.should_auto_clean_threshold() || self.should_auto_clean_scheduled();

        if should_clean {
            match clean_cache(false) {
                Ok(result) => {
                    // Update last clean timestamp
                    let mut settings = self.settings.lock().unwrap();
                    settings.record_clean(result.bytes_freed);
                    Some(result)
                }
                Err(_) => None,
            }
        } else {
            None
        }
    }

    /// Start the scheduler loop (call from a background thread)
    pub fn start(&self, callback: impl Fn(SchedulerEvent) + Send + 'static) {
        let settings = Arc::clone(&self.settings);
        let running = Arc::clone(&self.running);

        // Set running flag
        *running.lock().unwrap() = true;

        std::thread::spawn(move || {
            while *running.lock().unwrap() {
                let interval = {
                    let s = settings.lock().unwrap();
                    s.monitor_interval_secs
                };

                // Get current cache status
                let status = get_cache_status();
                callback(SchedulerEvent::CacheStatusUpdate(status.clone()));

                // Check if auto-clean should run
                let should_clean_threshold = {
                    let s = settings.lock().unwrap();
                    s.auto_clean_on_threshold && status.size_bytes >= s.auto_clean_threshold
                };

                let should_clean_scheduled = {
                    let s = settings.lock().unwrap();
                    if !s.auto_clean_scheduled {
                        false
                    } else {
                        let now = current_timestamp();
                        let elapsed = now.saturating_sub(s.last_clean_timestamp);
                        elapsed >= s.auto_clean_interval_secs
                    }
                };

                if should_clean_threshold || should_clean_scheduled {
                    callback(SchedulerEvent::AutoCleanTriggered);

                    match clean_cache(false) {
                        Ok(result) => {
                            // Update last clean timestamp
                            let mut s = settings.lock().unwrap();
                            s.record_clean(result.bytes_freed);
                            callback(SchedulerEvent::AutoCleanCompleted(result));
                        }
                        Err(e) => {
                            callback(SchedulerEvent::AutoCleanFailed(e.to_string()));
                        }
                    }
                }

                // Check for state changes that need notifications
                if status.state == CacheState::Warning {
                    callback(SchedulerEvent::WarningThresholdReached);
                } else if status.state == CacheState::Critical {
                    callback(SchedulerEvent::CriticalThresholdReached);
                }

                // Sleep until next check
                std::thread::sleep(Duration::from_secs(interval));
            }
        });
    }

    /// Stop the scheduler
    pub fn stop(&self) {
        *self.running.lock().unwrap() = false;
    }

    /// Check if scheduler is running
    pub fn is_running(&self) -> bool {
        *self.running.lock().unwrap()
    }
}

/// Events emitted by the scheduler
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SchedulerEvent {
    CacheStatusUpdate(crate::cache_monitor::CacheStatus),
    WarningThresholdReached,
    CriticalThresholdReached,
    AutoCleanTriggered,
    AutoCleanCompleted(CleanResult),
    AutoCleanFailed(String),
}

/// Format duration for display
pub fn format_duration(secs: u64) -> String {
    if secs < 60 {
        if secs == 1 {
            "1 second".to_string()
        } else {
            format!("{} seconds", secs)
        }
    } else if secs < 3600 {
        let mins = secs / 60;
        if mins == 1 {
            "1 minute".to_string()
        } else {
            format!("{} minutes", mins)
        }
    } else if secs < 86400 {
        let hours = secs / 3600;
        if hours == 1 {
            "1 hour".to_string()
        } else {
            format!("{} hours", hours)
        }
    } else {
        let days = secs / 86400;
        if days == 1 {
            "1 day".to_string()
        } else {
            format!("{} days", days)
        }
    }
}

/// Get time since last real clean (one that actually freed >0 bytes)
pub fn time_since_last_clean(settings: &Settings) -> String {
    let ts = if settings.last_real_clean_timestamp > 0 {
        settings.last_real_clean_timestamp
    } else if settings.last_clean_timestamp > 0 {
        // Backward compat: fall back to old field if no real clean recorded yet
        settings.last_clean_timestamp
    } else {
        return "Never".to_string();
    };

    let now = current_timestamp();
    let elapsed = now.saturating_sub(ts);
    format!("{} ago", format_duration(elapsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn licensed() -> Settings {
        let mut s = Settings::default();
        s.license_key = Some("KEY".to_string());
        s.license_instance_id = Some("INSTANCE".to_string());
        s.license_last_validated = 1;
        s
    }

    #[test]
    fn test_licensed_needs_key_and_activation() {
        assert!(!Settings::default().is_licensed());
        let mut key_only = Settings::default();
        key_only.license_key = Some("KEY".to_string());
        assert!(!key_only.is_licensed(), "a key without this Mac's activation isn't a license");
        let mut never_validated = licensed();
        never_validated.license_last_validated = 0;
        assert!(!never_validated.is_licensed(), "a key that never passed validation isn't a license");
        assert!(licensed().is_licensed());
    }

    #[test]
    fn test_free_mode_blocks_cleaning_but_not_dry_runs() {
        let free = Settings::default();
        assert_eq!(free.require_license(false), Err(LICENSE_REQUIRED.to_string()));
        assert_eq!(free.require_license(true), Ok(()), "a dry run deletes nothing");
        assert_eq!(licensed().require_license(false), Ok(()));
        assert_eq!(licensed().require_license(true), Ok(()));
    }

    #[test]
    fn test_autoclean_runs_only_when_licensed() {
        let now = 1_000_000;
        let mut free = Settings::default();
        free.auto_clean_on_threshold = true;
        free.auto_clean_threshold = 100;
        free.auto_clean_scheduled = true;
        free.auto_clean_interval_secs = 60;
        free.last_clean_timestamp = 0;
        assert!(!free.autoclean_due(1_000, now), "over threshold and overdue, but no license");

        let mut paid = licensed();
        paid.auto_clean_on_threshold = true;
        paid.auto_clean_threshold = 100;
        paid.auto_clean_scheduled = false;
        assert!(paid.autoclean_due(1_000, now), "over threshold");
        assert!(!paid.autoclean_due(10, now), "under threshold, not scheduled");

        paid.auto_clean_on_threshold = false;
        paid.auto_clean_scheduled = true;
        paid.auto_clean_interval_secs = 60;
        paid.last_clean_timestamp = now - 30;
        assert!(!paid.autoclean_due(10, now), "scheduled, not yet due");
        paid.last_clean_timestamp = now - 60;
        assert!(paid.autoclean_due(10, now), "scheduled and due");
    }
}
