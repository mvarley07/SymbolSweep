use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::cache_cleaner::{clean_cache, CleanResult};
use crate::cache_monitor::{get_cache_status, CacheState, WARNING_THRESHOLD};

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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_clean_on_threshold: false,
            auto_clean_threshold: WARNING_THRESHOLD,
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
        if path.exists() {
            match fs::read_to_string(&path) {
                Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
                Err(_) => Self::default(),
            }
        } else {
            Self::default()
        }
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

    /// Update last clean timestamp
    pub fn record_clean(&mut self) {
        self.last_clean_timestamp = current_timestamp();
        let _ = self.save();
    }
}

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
                    settings.record_clean();
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
                            s.record_clean();
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

/// Get time since last clean
pub fn time_since_last_clean(settings: &Settings) -> String {
    if settings.last_clean_timestamp == 0 {
        return "Never".to_string();
    }

    let now = current_timestamp();
    let elapsed = now.saturating_sub(settings.last_clean_timestamp);
    format!("{} ago", format_duration(elapsed))
}
