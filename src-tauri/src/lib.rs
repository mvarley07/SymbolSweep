// SymbolSweep - macOS menu bar app for coresymbolicationd cache management

mod cache_cleaner;
mod cache_monitor;
mod scheduler;
mod tray;

use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
use tauri_plugin_autostart::MacosLauncher;

use cache_cleaner::{clean_cache, get_log_file_path, reindex_spotlight, CleanResult};
use cache_monitor::{get_cache_status, get_combined_cache_status, get_simulated_status, is_daemon_running, CacheStatus};
use scheduler::{time_since_last_clean, Settings};
use tray::{create_tray, send_notification, update_tray_icon};

/// App state for sharing across commands
pub struct AppState {
    pub settings: Arc<Mutex<Settings>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            settings: Arc::new(Mutex::new(Settings::load())),
        }
    }
}

// ============================================================================
// Tauri Commands - Cache Monitoring
// ============================================================================

/// Get current cache status (respects debug mode)
#[tauri::command]
fn get_status(state: tauri::State<AppState>) -> CacheStatus {
    let settings = state.settings.lock().unwrap();
    if settings.debug_mode {
        get_simulated_status(settings.debug_simulated_size)
    } else {
        get_cache_status()
    }
}

/// Get combined cache status (user + system caches)
#[tauri::command]
fn get_combined_status() -> CacheStatus {
    get_combined_cache_status()
}

/// Check if coresymbolicationd daemon is running
#[tauri::command]
fn get_daemon_status() -> bool {
    is_daemon_running()
}

// ============================================================================
// Tauri Commands - Cache Cleaning
// ============================================================================

/// Clean the cache (with full safety checks)
#[tauri::command]
fn clean(app: tauri::AppHandle, state: tauri::State<AppState>, dry_run: bool) -> Result<CleanResult, String> {
    match clean_cache(dry_run) {
        Ok(result) => {
            // Update last clean timestamp only if not a dry run
            if !dry_run && result.success {
                if let Ok(mut settings) = state.settings.lock() {
                    settings.record_clean();
                    // Reset debug simulated size to 0 after clean (makes debug mode more realistic)
                    if settings.debug_mode {
                        settings.debug_simulated_size = 0;
                        let _ = settings.save();
                        // Notify frontend to refresh settings
                        let _ = app.emit("settings-updated", settings.clone());
                    }
                }
                // Update tray icon immediately after clean (use real status since debug size is now 0)
                let status = get_cache_status();
                let _ = update_tray_icon(&app, &status);
                // Emit status update so frontend refreshes
                let _ = app.emit("cache-status-update", &status);
            }
            Ok(result)
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Get the deletion log file path
#[tauri::command]
fn get_log_path() -> String {
    get_log_file_path()
}

/// Reindex Spotlight (requires password)
#[tauri::command]
fn reindex() -> Result<(), String> {
    reindex_spotlight().map_err(|e| e.to_string())
}

// ============================================================================
// Tauri Commands - Settings
// ============================================================================

/// Get current settings
#[tauri::command]
fn get_settings(state: tauri::State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

/// Update settings
#[tauri::command]
fn update_settings(app: tauri::AppHandle, state: tauri::State<AppState>, settings: Settings) -> Result<(), String> {
    let mut current = state.settings.lock().unwrap();

    // Check if launch_at_login changed
    let launch_changed = current.launch_at_login != settings.launch_at_login;
    let new_launch_value = settings.launch_at_login;

    *current = settings.clone();
    current.save()?;

    // Update tray immediately when settings change (especially debug mode)
    let status = if settings.debug_mode {
        get_simulated_status(settings.debug_simulated_size)
    } else {
        get_cache_status()
    };
    let _ = update_tray_icon(&app, &status);

    // Handle launch at login change
    if launch_changed {
        use tauri_plugin_autostart::ManagerExt;
        let autostart_manager = app.autolaunch();
        if new_launch_value {
            let _ = autostart_manager.enable();
        } else {
            let _ = autostart_manager.disable();
        }
    }

    Ok(())
}

/// Get time since last clean
#[tauri::command]
fn get_last_clean_time(state: tauri::State<AppState>) -> String {
    let settings = state.settings.lock().unwrap();
    time_since_last_clean(&settings)
}

/// Quit the application
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

/// Test notification (debug only) - sends a test notification via terminal-notifier
#[tauri::command]
fn test_notification() {
    #[cfg(target_os = "macos")]
    {
        let result = std::process::Command::new("terminal-notifier")
            .arg("-title")
            .arg("SymbolSweep Test")
            .arg("-message")
            .arg("Notifications are working!")
            .arg("-sound")
            .arg("default")
            .output();

        match result {
            Ok(output) => {
                if output.status.success() {
                    eprintln!("Notification sent via terminal-notifier");
                } else {
                    eprintln!("terminal-notifier failed: {}", String::from_utf8_lossy(&output.stderr));
                }
            }
            Err(e) => eprintln!("Failed to run terminal-notifier: {}", e),
        }
    }
}

/// Open macOS System Settings to the Notifications pane
#[tauri::command]
fn open_notification_settings() {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.Notifications-Settings.extension")
            .spawn();
    }
}

// ============================================================================
// App Entry Point
// ============================================================================

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // When a second instance tries to launch, show the existing window
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--hidden"])))
        .manage(AppState::default())
        .setup(|app| {
            // Create system tray
            // IMPORTANT: Store the tray icon to prevent it from being dropped
            // Box::leak keeps it alive for the entire app lifetime
            let tray = create_tray(app.handle())?;
            Box::leak(Box::new(tray));

            // Get initial status and update tray (respecting debug mode)
            let state = app.state::<AppState>();
            let initial_status = {
                let settings = state.settings.lock().unwrap();
                if settings.debug_mode {
                    get_simulated_status(settings.debug_simulated_size)
                } else {
                    get_cache_status()
                }
            };
            let _ = update_tray_icon(app.handle(), &initial_status);

            // Sync autostart state with saved setting
            {
                use tauri_plugin_autostart::ManagerExt;
                let autostart_manager = app.autolaunch();
                let settings = state.settings.lock().unwrap();
                if settings.launch_at_login {
                    let _ = autostart_manager.enable();
                } else {
                    let _ = autostart_manager.disable();
                }
            }

            // Set up background monitoring
            let app_handle = app.handle().clone();
            let state = app.state::<AppState>();
            let settings = Arc::clone(&state.settings);

            std::thread::spawn(move || {
                loop {
                    // Get monitoring interval (read before sleep)
                    let interval = {
                        let s = settings.lock().unwrap();
                        s.monitor_interval_secs
                    };

                    // Sleep first (so we don't immediately check on startup)
                    std::thread::sleep(std::time::Duration::from_secs(interval));

                    // Get current status (read debug settings FRESH after sleep)
                    let status = {
                        let s = settings.lock().unwrap();
                        if s.debug_mode {
                            get_simulated_status(s.debug_simulated_size)
                        } else {
                            get_cache_status()
                        }
                    };

                    // Update tray icon
                    let _ = update_tray_icon(&app_handle, &status);

                    // Emit status update to frontend
                    let _ = app_handle.emit("cache-status-update", &status);

                    // Check for auto-clean conditions
                    let should_auto_clean = {
                        let s = settings.lock().unwrap();
                        let threshold_clean = s.auto_clean_on_threshold
                            && status.size_bytes >= s.auto_clean_threshold;

                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();

                        let scheduled_clean = s.auto_clean_scheduled
                            && (now - s.last_clean_timestamp) >= s.auto_clean_interval_secs;

                        threshold_clean || scheduled_clean
                    };

                    if should_auto_clean {
                        // Notify about auto-clean
                        send_notification(
                            &app_handle,
                            "SymbolSweep",
                            &format!("Auto-cleaning cache ({})...", status.size_display),
                        );

                        // Perform clean
                        if let Ok(result) = clean_cache(false) {
                            // Update last clean timestamp and reset debug size
                            if let Ok(mut s) = settings.lock() {
                                s.record_clean();
                                // Reset debug simulated size to 0 after clean
                                if s.debug_mode {
                                    s.debug_simulated_size = 0;
                                    let _ = s.save();
                                    // Notify frontend to refresh settings
                                    let _ = app_handle.emit("settings-updated", s.clone());
                                }
                            }

                            // Update tray to show clean state
                            let clean_status = get_cache_status();
                            let _ = update_tray_icon(&app_handle, &clean_status);
                            // Emit status update so frontend refreshes
                            let _ = app_handle.emit("cache-status-update", &clean_status);

                            // Emit clean result
                            let _ = app_handle.emit("auto-clean-completed", &result);

                            // Notify about completion
                            send_notification(
                                &app_handle,
                                "SymbolSweep",
                                &format!("Freed {}", result.bytes_freed_display),
                            );
                        }
                    }

                    // Check for warning/critical thresholds and notify
                    let show_notifications = settings.lock().unwrap().show_notifications;
                    if show_notifications {
                        match status.state {
                            cache_monitor::CacheState::Warning => {
                                // Only notify once per session (could track with a flag)
                            }
                            cache_monitor::CacheState::Critical => {
                                send_notification(
                                    &app_handle,
                                    "SymbolSweep - Critical",
                                    &format!("Cache at {} - cleaning recommended!", status.size_display),
                                );
                            }
                            _ => {}
                        }
                    }
                }
            });

            // Configure window for menu bar app behavior
            if let Some(window) = app.get_webview_window("main") {
                // Hide window initially (will show when tray icon clicked)
                let _ = window.hide();

                // Make window float above others
                let _ = window.set_always_on_top(true);

                // Note: LSUIElement in Info.plist handles hiding from dock

                // Hide window when it loses focus (click outside)
                let window_clone = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::Focused(false) = event {
                        // Small delay to prevent race with tray click
                        let w = window_clone.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(100));
                            let _ = w.hide();
                        });
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            get_combined_status,
            get_daemon_status,
            clean,
            get_log_path,
            reindex,
            get_settings,
            update_settings,
            get_last_clean_time,
            quit_app,
            test_notification,
            open_notification_settings,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Handle dock icon click on macOS
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                if let Some(window) = app.get_webview_window("main") {
                    // Position near tray - use TopRight as fallback since TrayBottomCenter can panic
                    use tauri_plugin_positioner::{Position, WindowExt};
                    let _ = window.move_window(Position::TopRight);
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        });
}
