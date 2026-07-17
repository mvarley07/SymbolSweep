// SymbolSweep - macOS menu bar app for coresymbolicationd cache management

mod cache_cleaner;
mod cache_monitor;
mod dev_scanner;
mod scheduler;
mod tray;

use std::sync::{Arc, Mutex};
use tauri::{Emitter, Listener, Manager};
use tauri_plugin_autostart::MacosLauncher;

use cache_cleaner::{clean_cache, get_log_file_path, log_deletion, CleanResult};
use cache_monitor::{
    compute_app_status, get_cache_status, get_combined_cache_status, get_simulated_status,
    is_daemon_running, AppStatus, CacheStatus,
};
use dev_scanner::{DevDeleteResult, DevScanResult, PurgeResult, SsTrashInfo};
use scheduler::{time_since_last_clean, Settings};
use tray::{create_tray, send_notification, update_tray};

/// App state for sharing across commands
pub struct AppState {
    pub settings: Arc<Mutex<Settings>>,
    /// Latest dev artifact scan result (populated on startup and on-demand)
    pub dev_scan_result: Arc<Mutex<Option<DevScanResult>>>,
    /// Guard to prevent concurrent dev scans
    pub dev_scan_in_progress: Arc<std::sync::atomic::AtomicBool>,
    /// Epoch seconds of the last completed dev artifact scan
    pub last_dev_scan_epoch: Arc<std::sync::atomic::AtomicU64>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            settings: Arc::new(Mutex::new(Settings::load())),
            dev_scan_result: Arc::new(Mutex::new(None)),
            dev_scan_in_progress: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_dev_scan_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }
}

/// RAII guard that clears `dev_scan_in_progress` on drop.
/// Prevents the AtomicBool from staying true forever if a panic or early
/// return occurs between the compare_exchange(false→true) and the manual
/// store(false). Covers the same silent-failure class as the autoclean fix.
struct DevScanGuard(Arc<std::sync::atomic::AtomicBool>);

impl Drop for DevScanGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
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

/// Get the unified app status — single source of truth for tray and popup
#[tauri::command]
fn get_app_status(state: tauri::State<AppState>) -> AppStatus {
    let cache = {
        let settings = state.settings.lock().unwrap();
        if settings.debug_mode {
            get_simulated_status(settings.debug_simulated_size)
        } else {
            get_cache_status()
        }
    };
    let (dev_total, dev_scan_complete) = state
        .dev_scan_result
        .lock()
        .ok()
        .and_then(|s| s.as_ref().map(|r| (r.total_bytes, true)))
        .unwrap_or((0, false));
    let failures = state.settings.lock().unwrap().consecutive_autoclean_failures;
    compute_app_status(&cache, dev_total, dev_scan_complete, failures)
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
async fn clean(app: tauri::AppHandle, state: tauri::State<'_, AppState>, dry_run: bool) -> Result<CleanResult, String> {
    // Offload blocking file I/O to background thread
    let result = tauri::async_runtime::spawn_blocking(move || clean_cache(dry_run))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

    // State updates are fast — run after I/O completes
    if !dry_run && result.success {
        if let Ok(mut settings) = state.settings.lock() {
            settings.record_clean(result.bytes_freed);
            // Reset debug simulated size to 0 after clean (makes debug mode more realistic)
            if settings.debug_mode {
                settings.debug_simulated_size = 0;
                let _ = settings.save();
                // Notify frontend to refresh settings
                let _ = app.emit("settings-updated", settings.clone());
            }
        }
        // Compute unified status and update both tray and frontend
        let cache = {
            let s = state.settings.lock().unwrap();
            if s.debug_mode { get_simulated_status(s.debug_simulated_size) } else { get_cache_status() }
        };
        let (dev_total, dev_scan_complete) = state.dev_scan_result.lock()
            .ok()
            .and_then(|s| s.as_ref().map(|r| (r.total_bytes, true)))
            .unwrap_or((0, false));
        let failures = state.settings.lock().unwrap().consecutive_autoclean_failures;
        let app_status = compute_app_status(&cache, dev_total, dev_scan_complete, failures);
        let _ = update_tray(&app, &app_status);
        let _ = app.emit("app-status-update", &app_status);
    }
    Ok(result)
}

/// Get the deletion log file path
#[tauri::command]
fn get_log_path() -> String {
    get_log_file_path()
}

/// Read recent deletion log entries (last 50 lines)
#[tauri::command]
fn get_recent_deletions() -> Vec<String> {
    let log_path = std::path::PathBuf::from(get_log_file_path());
    if !log_path.exists() {
        return Vec::new();
    }
    match std::fs::read_to_string(&log_path) {
        Ok(content) => {
            let lines: Vec<String> = content
                .lines()
                .rev()
                .take(50)
                .map(|l| l.to_string())
                .collect();
            lines
        }
        Err(_) => Vec::new(),
    }
}

// ============================================================================
// Tauri Commands - Dev Artifact Scanning
// ============================================================================

/// Run a dev artifact scan (detection only, no deletion)
#[tauri::command]
async fn scan_dev(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> Result<DevScanResult, String> {
    use std::sync::atomic::Ordering;

    // Prevent concurrent scans
    if state
        .dev_scan_in_progress
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        // Return cached result if scan already in progress
        let cached = state.dev_scan_result.lock().unwrap();
        return cached
            .clone()
            .ok_or_else(|| "Scan already in progress".to_string());
    }
    let _guard = DevScanGuard(Arc::clone(&state.dev_scan_in_progress));

    let roots = {
        let s = state.settings.lock().unwrap();
        s.dev_scan_roots.clone()
    };

    // Offload filesystem walk to background thread
    let scan_result = tauri::async_runtime::spawn_blocking(move || {
        dev_scanner::scan_dev_artifacts(&roots)
    }).await;

    let result = scan_result.map_err(|e| e.to_string())?;

    // Cache the result and record scan timestamp
    {
        let mut cached = state.dev_scan_result.lock().unwrap();
        *cached = Some(result.clone());
    }
    state.last_dev_scan_epoch.store(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
        Ordering::SeqCst,
    );

    // Compute unified status and update tray + emit
    let cache_status = {
        let s = state.settings.lock().unwrap();
        if s.debug_mode {
            get_simulated_status(s.debug_simulated_size)
        } else {
            get_cache_status()
        }
    };
    let failures = state.settings.lock().unwrap().consecutive_autoclean_failures;
    let app_status = compute_app_status(&cache_status, result.total_bytes, true, failures);
    let _ = update_tray(&app, &app_status);
    let _ = app.emit("app-status-update", &app_status);

    Ok(result)
}

/// Get the cached dev scan result without running a new scan
#[tauri::command]
fn get_dev_scan_result(state: tauri::State<AppState>) -> Option<DevScanResult> {
    state.dev_scan_result.lock().unwrap().clone()
}

/// Delete specific dev artifacts by path (bulk — Safe tier only), then re-scan
#[tauri::command]
async fn delete_dev_artifacts(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    paths: Vec<String>,
) -> Result<DevDeleteResult, String> {
    delete_dev_artifacts_inner(&app, &state, paths, false).await
}

/// Delete specific dev artifacts by path (manual — user-initiated, allows non-Safe except Ask)
#[tauri::command]
async fn delete_dev_artifacts_manual(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    paths: Vec<String>,
) -> Result<DevDeleteResult, String> {
    delete_dev_artifacts_inner(&app, &state, paths, true).await
}

async fn delete_dev_artifacts_inner(
    app: &tauri::AppHandle,
    state: &tauri::State<'_, AppState>,
    paths: Vec<String>,
    manual: bool,
) -> Result<DevDeleteResult, String> {
    // Extract needed data from state (fast, sync)
    let known_artifacts = {
        let cached = state.dev_scan_result.lock().unwrap();
        match cached.as_ref() {
            Some(result) => result.artifacts.clone(),
            None => return Err("No scan result available. Run a scan first.".to_string()),
        }
    };
    let roots = {
        let s = state.settings.lock().unwrap();
        s.dev_scan_roots.clone()
    };

    // Offload heavy deletion + re-scan to background thread
    let (delete_result, new_scan) = tauri::async_runtime::spawn_blocking(move || {
        let del = if manual {
            dev_scanner::delete_dev_artifacts_manual(&paths, &known_artifacts)
        } else {
            dev_scanner::delete_dev_artifacts(&paths, &known_artifacts)
        };
        let scan = dev_scanner::scan_dev_artifacts(&roots);
        (del, scan)
    })
    .await
    .map_err(|e| e.to_string())?;

    // Update cached result (fast)
    {
        let mut cached = state.dev_scan_result.lock().unwrap();
        *cached = Some(new_scan.clone());
    }

    // Compute unified status and update tray + emit
    let cache_status = {
        let s = state.settings.lock().unwrap();
        if s.debug_mode {
            get_simulated_status(s.debug_simulated_size)
        } else {
            get_cache_status()
        }
    };
    let failures = state.settings.lock().unwrap().consecutive_autoclean_failures;
    let app_status = compute_app_status(&cache_status, new_scan.total_bytes, true, failures);
    let _ = update_tray(app, &app_status);
    let _ = app.emit("app-status-update", &app_status);

    // Notify frontend of updated scan result (full artifact list)
    let _ = app.emit("dev-scan-ready", &new_scan);

    Ok(delete_result)
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
fn update_settings(app: tauri::AppHandle, state: tauri::State<AppState>, mut settings: Settings) -> Result<(), String> {
    // Clamp debug-only intervals when debug mode is off
    if !settings.debug_mode && settings.auto_clean_interval_secs < 3600 {
        settings.auto_clean_interval_secs = 3600;
    }

    let mut current = state.settings.lock().unwrap();

    // Check if launch_at_login changed
    let launch_changed = current.launch_at_login != settings.launch_at_login;
    let new_launch_value = settings.launch_at_login;

    *current = settings.clone();
    current.save()?;

    // Update tray immediately when settings change (especially debug mode)
    let cache = if settings.debug_mode {
        get_simulated_status(settings.debug_simulated_size)
    } else {
        get_cache_status()
    };
    let (dev_total, dev_scan_complete) = state.dev_scan_result.lock()
        .ok()
        .and_then(|s| s.as_ref().map(|r| (r.total_bytes, true)))
        .unwrap_or((0, false));
    let failures = current.consecutive_autoclean_failures;
    drop(current); // release lock before compute
    let app_status = compute_app_status(&cache, dev_total, dev_scan_complete, failures);
    let _ = update_tray(&app, &app_status);
    let _ = app.emit("app-status-update", &app_status);

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

/// Get the freed size from the last real clean (>0 bytes).
/// Returns the human-readable size string, or empty string if no real clean on record.
#[tauri::command]
fn get_last_clean_freed(state: tauri::State<AppState>) -> String {
    let settings = state.settings.lock().unwrap();
    if settings.last_real_clean_freed > 0 {
        cache_monitor::format_size(settings.last_real_clean_freed)
    } else {
        String::new()
    }
}

/// Quit the application
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

/// Test notification (debug only) - uses the same notification path as real notifications
#[tauri::command]
fn test_notification(app: tauri::AppHandle) {
    tray::send_notification(&app, "SymbolSweep Test", "Notifications are working!");
}

/// Check for updates and install if available
#[tauri::command]
async fn check_for_update(app: tauri::AppHandle) -> Result<String, String> {
    use tauri_plugin_updater::UpdaterExt;

    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await {
        Ok(Some(update)) => {
            let version = update.version.clone();
            update
                .download_and_install(|_, _| {}, || {})
                .await
                .map_err(|e| format!("Install failed: {}", e))?;
            Ok(format!("v{} installed -- restart to apply", version))
        }
        Ok(None) => Ok("up_to_date".to_string()),
        Err(e) => Err(format!("Update check failed: {}", e)),
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

/// Open macOS System Settings to the Storage pane
#[tauri::command]
fn open_storage_settings() {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.settings.Storage")
            .spawn();
    }
}

/// Open the Trash folder in Finder
#[tauri::command]
fn open_trash() {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg(std::path::Path::new(&std::env::var("HOME").unwrap_or_default()).join(".Trash"))
            .spawn();
    }
}

/// Get info about SS items still sitting in Trash
#[tauri::command]
fn get_ss_trash_info() -> SsTrashInfo {
    dev_scanner::get_ss_trash_info()
}

/// Permanently delete only the items SS moved to Trash — never touches other Trash contents
#[tauri::command]
async fn purge_ss_trash() -> Result<PurgeResult, String> {
    tauri::async_runtime::spawn_blocking(dev_scanner::purge_ss_trash)
        .await
        .map_err(|e| e.to_string())
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
                let _ = app.emit("window-shown", ());
            }
        }))
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--hidden"])))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState::default())
        .setup(|app| {
            // Create system tray IMMEDIATELY with retry logic
            // No initial delay - tray should appear as fast as possible
            // IMPORTANT: Store the tray icon to prevent it from being dropped
            // Box::leak keeps it alive for the entire app lifetime
            let mut tray_result = create_tray(app.handle());
            let mut retries = 0;
            const MAX_RETRIES: u32 = 5;

            while tray_result.is_err() && retries < MAX_RETRIES {
                retries += 1;
                eprintln!("Tray creation failed, retry {}/{}", retries, MAX_RETRIES);
                std::thread::sleep(std::time::Duration::from_millis(100));
                tray_result = create_tray(app.handle());
            }

            let tray = tray_result?;
            Box::leak(Box::new(tray));

            // Tray is now visible — get initial status in background
            let app_handle_init = app.handle().clone();
            let state = app.state::<AppState>();
            let settings_init = Arc::clone(&state.settings);
            let dev_result_init = Arc::clone(&state.dev_scan_result);
            let dev_in_progress_init = Arc::clone(&state.dev_scan_in_progress);
            let dev_scan_epoch_init = Arc::clone(&state.last_dev_scan_epoch);
            std::thread::spawn(move || {
                // Phase 1: Quick — get coresymbolicationd cache status
                let initial_cache = {
                    let settings = settings_init.lock().unwrap();
                    if settings.debug_mode {
                        get_simulated_status(settings.debug_simulated_size)
                    } else {
                        get_cache_status()
                    }
                };
                // Emit unified AppStatus (dev_total=0, scan not yet complete)
                let failures = settings_init.lock().unwrap().consecutive_autoclean_failures;
                let app_status = compute_app_status(&initial_cache, 0, false, failures);
                let _ = update_tray(&app_handle_init, &app_status);
                let _ = app_handle_init.emit("app-status-update", &app_status);

                // Phase 2: Slower — run initial dev artifact scan
                if dev_in_progress_init
                    .compare_exchange(
                        false,
                        true,
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                    )
                    .is_ok()
                {
                    let _guard = DevScanGuard(Arc::clone(&dev_in_progress_init));
                    let roots = {
                        let s = settings_init.lock().unwrap();
                        s.dev_scan_roots.clone()
                    };
                    let dev_result = dev_scanner::scan_dev_artifacts(&roots);
                    let dev_total = dev_result.total_bytes;

                    // Cache the result
                    {
                        let mut cached = dev_result_init.lock().unwrap();
                        *cached = Some(dev_result.clone());
                    }

                    // Re-compute and emit unified AppStatus with dev total
                    let failures = settings_init.lock().unwrap().consecutive_autoclean_failures;
                    let app_status = compute_app_status(&initial_cache, dev_total, true, failures);
                    let _ = update_tray(&app_handle_init, &app_status);
                    let _ = app_handle_init.emit("app-status-update", &app_status);

                    // Also emit full artifact list for DevScanPanel
                    let _ = app_handle_init.emit("dev-scan-ready", &dev_result);

                    dev_scan_epoch_init.store(
                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                }
            });

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
            let dev_result_monitor = Arc::clone(&state.dev_scan_result);

            std::thread::spawn(move || {
                // Track if we've already notified for each escalation level this session
                // Reset when state drops back to clean
                let mut warning_notified = false;
                let mut critical_notified = false;
                let mut runaway_notified = false;

                loop {
                    // Get monitoring interval (read before sleep)
                    let interval = {
                        let s = settings.lock().unwrap();
                        s.monitor_interval_secs
                    };

                    // Sleep first (so we don't immediately check on startup)
                    std::thread::sleep(std::time::Duration::from_secs(interval));

                    // Get current cache status (read debug settings FRESH after sleep)
                    let cache_status = {
                        let s = settings.lock().unwrap();
                        if s.debug_mode {
                            get_simulated_status(s.debug_simulated_size)
                        } else {
                            get_cache_status()
                        }
                    };

                    // Compute unified AppStatus and update both tray + frontend
                    let (dev_total, dev_scan_complete) = dev_result_monitor
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map(|r| (r.total_bytes, true))
                        .unwrap_or((0, false));
                    let failures = settings.lock().unwrap().consecutive_autoclean_failures;
                    let app_status = compute_app_status(&cache_status, dev_total, dev_scan_complete, failures);
                    let _ = update_tray(&app_handle, &app_status);
                    let _ = app_handle.emit("app-status-update", &app_status);

                    // Check for auto-clean conditions
                    let should_auto_clean = {
                        let s = settings.lock().unwrap();
                        let threshold_clean = s.auto_clean_on_threshold
                            && cache_status.size_bytes >= s.auto_clean_threshold;

                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();

                        let scheduled_clean = s.auto_clean_scheduled
                            && (now - s.last_clean_timestamp) >= s.auto_clean_interval_secs;

                        threshold_clean || scheduled_clean
                    };

                    if should_auto_clean {
                        let show_notifications = settings.lock().unwrap().show_notifications;

                        // Perform clean
                        match clean_cache(false) {
                            Ok(result) => {
                                // Update last clean timestamp and reset debug size
                                // (record_clean also resets consecutive_autoclean_failures)
                                if let Ok(mut s) = settings.lock() {
                                    s.record_clean(result.bytes_freed);
                                    if s.debug_mode {
                                        s.debug_simulated_size = 0;
                                        let _ = s.save();
                                        let _ = app_handle.emit("settings-updated", s.clone());
                                    }
                                }

                                // Re-compute and emit unified AppStatus after clean
                                let clean_cache = get_cache_status();
                                let failures = settings.lock().unwrap().consecutive_autoclean_failures;
                                let post_clean_status = compute_app_status(&clean_cache, dev_total, dev_scan_complete, failures);
                                let _ = update_tray(&app_handle, &post_clean_status);
                                let _ = app_handle.emit("app-status-update", &post_clean_status);

                                // Emit clean result
                                let _ = app_handle.emit("auto-clean-completed", &result);

                                if show_notifications && result.bytes_freed > 0 {
                                    send_notification(
                                        &app_handle,
                                        "SymbolSweep",
                                        &format!("Cleaned {} of cache", result.bytes_freed_display),
                                    );
                                }
                            }
                            Err(e) => {
                                // Log every failure
                                log_deletion(&format!("AUTOCLEAN FAILED: {}", e));

                                // Track consecutive failures (only increments on real attempts)
                                let failures = if let Ok(mut s) = settings.lock() {
                                    s.record_autoclean_failure();
                                    s.consecutive_autoclean_failures
                                } else {
                                    0
                                };

                                // After 3 consecutive failures, surface to the user
                                if failures >= 3 {
                                    let fail_status = compute_app_status(
                                        &cache_status, dev_total, dev_scan_complete, failures,
                                    );
                                    let _ = update_tray(&app_handle, &fail_status);
                                    let _ = app_handle.emit("app-status-update", &fail_status);

                                    if show_notifications {
                                        send_notification(
                                            &app_handle,
                                            "SymbolSweep",
                                            "Autoclean has failed repeatedly \u{2014} cache is unmanaged",
                                        );
                                    }
                                }
                            }
                        }
                    }

                    // Check for warning/critical thresholds and notify (once per escalation)
                    // Uses unified health state, not just cache state
                    let show_notifications = settings.lock().unwrap().show_notifications;
                    if show_notifications && !should_auto_clean {
                        match app_status.clean_state {
                            cache_monitor::CleanState::Moderate => {
                                if !warning_notified {
                                    send_notification(
                                        &app_handle,
                                        "SymbolSweep",
                                        &format!("{} reclaimable — consider cleaning", app_status.reclaimable_display),
                                    );
                                    warning_notified = true;
                                }
                            }
                            cache_monitor::CleanState::Heavy => {
                                if !critical_notified {
                                    send_notification(
                                        &app_handle,
                                        "SymbolSweep",
                                        &format!("{} piled up — a good time to sweep", app_status.reclaimable_display),
                                    );
                                    critical_notified = true;
                                }
                            }
                            cache_monitor::CleanState::Runaway => {
                                if !runaway_notified {
                                    send_notification(
                                        &app_handle,
                                        "SymbolSweep",
                                        &format!(
                                            "Symbolication cache has grown to {} \u{2014} clean now",
                                            app_status.cache.size_display
                                        ),
                                    );
                                    runaway_notified = true;
                                }
                            }
                            _ => {}
                        }
                    }

                    // Reset notification flags when back to clean
                    if matches!(app_status.clean_state, cache_monitor::CleanState::Clean) {
                        warning_notified = false;
                        critical_notified = false;
                        runaway_notified = false;
                    }
                }
            });

            // Refresh status on window show — cache always, dev scan if stale (>5 min)
            {
                let app_handle = app.handle().clone();
                let state = app.state::<AppState>();
                let settings_show = Arc::clone(&state.settings);
                let dev_result_show = Arc::clone(&state.dev_scan_result);
                let dev_in_progress_show = Arc::clone(&state.dev_scan_in_progress);
                let dev_scan_epoch_show = Arc::clone(&state.last_dev_scan_epoch);

                app.listen("window-shown", move |_event| {
                    // Always: refresh cache + recompute + emit (instant)
                    let cache_status = {
                        let s = settings_show.lock().unwrap();
                        if s.debug_mode {
                            get_simulated_status(s.debug_simulated_size)
                        } else {
                            get_cache_status()
                        }
                    };
                    let (dev_total, dev_scan_complete) = dev_result_show
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map(|r| (r.total_bytes, true))
                        .unwrap_or((0, false));
                    let failures = settings_show.lock().unwrap().consecutive_autoclean_failures;
                    let app_status = compute_app_status(&cache_status, dev_total, dev_scan_complete, failures);
                    let _ = update_tray(&app_handle, &app_status);
                    let _ = app_handle.emit("app-status-update", &app_status);

                    // Conditionally: re-run dev scan if stale (>5 minutes)
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let last_scan = dev_scan_epoch_show.load(std::sync::atomic::Ordering::SeqCst);
                    let stale = now.saturating_sub(last_scan) >= 300; // 5 minutes

                    if stale
                        && dev_in_progress_show
                            .compare_exchange(
                                false,
                                true,
                                std::sync::atomic::Ordering::SeqCst,
                                std::sync::atomic::Ordering::SeqCst,
                            )
                            .is_ok()
                    {
                        let app_handle_bg = app_handle.clone();
                        let settings_bg = Arc::clone(&settings_show);
                        let dev_result_bg = Arc::clone(&dev_result_show);
                        let dev_in_progress_bg = Arc::clone(&dev_in_progress_show);
                        let dev_scan_epoch_bg = Arc::clone(&dev_scan_epoch_show);

                        std::thread::spawn(move || {
                            let _guard = DevScanGuard(dev_in_progress_bg);
                            let roots = {
                                let s = settings_bg.lock().unwrap();
                                s.dev_scan_roots.clone()
                            };
                            let result = dev_scanner::scan_dev_artifacts(&roots);
                            let dev_total = result.total_bytes;

                            // Cache result + update epoch
                            {
                                let mut cached = dev_result_bg.lock().unwrap();
                                *cached = Some(result.clone());
                            }
                            dev_scan_epoch_bg.store(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                                std::sync::atomic::Ordering::SeqCst,
                            );

                            // Recompute and emit updated status
                            let cache_status = {
                                let s = settings_bg.lock().unwrap();
                                if s.debug_mode {
                                    get_simulated_status(s.debug_simulated_size)
                                } else {
                                    get_cache_status()
                                }
                            };
                            let failures = settings_bg.lock().unwrap().consecutive_autoclean_failures;
                            let app_status = compute_app_status(
                                &cache_status, dev_total, true, failures,
                            );
                            let _ = update_tray(&app_handle_bg, &app_status);
                            let _ = app_handle_bg.emit("app-status-update", &app_status);
                            let _ = app_handle_bg.emit("dev-scan-ready", &result);
                        });
                    }
                });
            }

            // Background update check (30s after launch, then every 6 hours)
            let app_handle_updater = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(30));
                loop {
                    let handle = app_handle_updater.clone();
                    tauri::async_runtime::block_on(async move {
                        use tauri_plugin_updater::UpdaterExt;
                        if let Ok(updater) = handle.updater() {
                            if let Ok(Some(update)) = updater.check().await {
                                let _ = update.download_and_install(|_, _| {}, || {}).await;
                            }
                        }
                    });
                    std::thread::sleep(std::time::Duration::from_secs(6 * 60 * 60));
                }
            });

            // Configure window for menu bar app behavior
            if let Some(window) = app.get_webview_window("main") {
                // Hide window initially (will show when tray icon clicked)
                let _ = window.hide();

                // Pre-position to top-right so the first show() never flashes at center
                {
                    use tauri::PhysicalPosition;
                    const WINDOW_WIDTH: i32 = 280;
                    const MARGIN_RIGHT: i32 = 10;
                    const MENU_BAR_HEIGHT: i32 = 30;

                    if let Ok(Some(monitor)) = window.primary_monitor() {
                        let screen_size = monitor.size();
                        let scale = monitor.scale_factor();
                        let screen_width = (screen_size.width as f64 / scale) as i32;
                        let x = screen_width - WINDOW_WIDTH - MARGIN_RIGHT;
                        let y = MENU_BAR_HEIGHT;
                        let _ = window.set_position(PhysicalPosition::new(
                            (x as f64 * scale) as i32,
                            (y as f64 * scale) as i32,
                        ));
                    }
                }

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
            get_app_status,
            get_daemon_status,
            clean,
            get_log_path,
            get_settings,
            update_settings,
            get_last_clean_time,
            get_last_clean_freed,
            quit_app,
            test_notification,
            open_notification_settings,
            open_storage_settings,
            open_trash,
            get_ss_trash_info,
            purge_ss_trash,
            scan_dev,
            get_dev_scan_result,
            delete_dev_artifacts,
            delete_dev_artifacts_manual,
            get_recent_deletions,
            check_for_update,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Handle dock icon click on macOS
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                if let Some(window) = app.get_webview_window("main") {
                    use tauri::PhysicalPosition;
                    const WINDOW_WIDTH: i32 = 280;
                    const MARGIN_RIGHT: i32 = 10;
                    const MENU_BAR_HEIGHT: i32 = 30;

                    // Hide first to prevent flash at old position
                    let _ = window.hide();

                    if let Ok(Some(monitor)) = window.primary_monitor() {
                        let screen_size = monitor.size();
                        let scale = monitor.scale_factor();
                        let screen_width = (screen_size.width as f64 / scale) as i32;
                        let x = screen_width - WINDOW_WIDTH - MARGIN_RIGHT;
                        let y = MENU_BAR_HEIGHT;
                        let _ = window.set_position(PhysicalPosition::new(
                            (x as f64 * scale) as i32,
                            (y as f64 * scale) as i32,
                        ));
                    }

                    std::thread::sleep(std::time::Duration::from_millis(10));
                    let _ = window.show();
                    let _ = window.set_focus();
                    let _ = app.emit("window-shown", ());
                }
            }
        });
}
