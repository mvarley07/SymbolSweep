use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Runtime,
};
use tauri_plugin_positioner::{Position, WindowExt};

use crate::cache_monitor::{AppStatus, CleanState};

/// Activate the macOS app so it receives first-click events
#[cfg(target_os = "macos")]
fn activate_app() {
    use cocoa::appkit::{NSApp, NSApplication, NSApplicationActivationPolicy};
    use cocoa::base::nil;
    unsafe {
        let app = NSApp();
        // Activate ignoring other apps to bring to front
        app.activateIgnoringOtherApps_(true);
    }
}

#[cfg(not(target_os = "macos"))]
fn activate_app() {
    // No-op on other platforms
}

/// Tray icon identifier
pub const TRAY_ID: &str = "symbolsweep-tray";

/// Track last show time to prevent rapid toggle (debounce)
static LAST_SHOW_TIME: AtomicU64 = AtomicU64::new(0);

/// Get current time in milliseconds
fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Create the system tray with minimal text-only display
/// Returns the TrayIcon which MUST be stored to prevent it from being dropped
pub fn create_tray<R: Runtime>(app: &AppHandle<R>) -> Result<TrayIcon<R>, Box<dyn std::error::Error>> {
    // CRITICAL: Check if tray already exists and return it to prevent duplicates
    // This handles hot reload in dev mode and prevents multiple tray icons
    if let Some(existing) = app.tray_by_id(TRAY_ID) {
        // Tray already exists, make sure it's visible and return it
        let _ = existing.set_visible(true);
        return Ok(existing);
    }

    // Create menu items
    let show_item = MenuItem::with_id(app, "show", "Show SymbolSweep", true, None::<&str>)?;
    let clean_item = MenuItem::with_id(app, "clean", "Clean Cache Now", true, None::<&str>)?;
    let separator = MenuItem::with_id(app, "sep", "---", false, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    // Create menu
    let menu = Menu::with_items(app, &[&show_item, &clean_item, &separator, &quit_item])?;

    // Create a minimal 1x1 transparent icon (required by Tauri, but we'll hide it with title)
    let icon = create_tray_icon()?;

    // Build tray - text only, minimal icon
    // IMPORTANT: The returned TrayIcon MUST be stored somewhere to prevent it from being dropped
    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .icon_as_template(true)
        .menu(&menu)
        .title("") // Empty until first status update — prevents "0 B" flash
        .tooltip("SymbolSweep - Cache Monitor")
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if let Some(window) = tray.app_handle().get_webview_window("main") {
                    // ALWAYS show the window on tray click - never hide
                    // User can press Escape or click outside to dismiss
                    // This avoids the macOS tray click visibility bug entirely
                    LAST_SHOW_TIME.store(current_time_ms(), Ordering::SeqCst);

                    // Activate the app first so it receives first-click events
                    activate_app();

                    // Position window (this hides it first to prevent flash)
                    position_window_near_tray(&window);

                    // Small delay to let macOS process the position change
                    // before showing - prevents the flash/drift issue
                    std::thread::sleep(std::time::Duration::from_millis(10));

                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        })
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    LAST_SHOW_TIME.store(current_time_ms(), Ordering::SeqCst);
                    activate_app();
                    position_window_near_tray(&window);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "clean" => {
                // Emit event to trigger clean from frontend
                let _ = app.emit("clean-requested", ());
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;

    Ok(tray)
}

/// Load the broom template icon for the tray.
/// macOS treats images with filenames ending in "Template" as template images,
/// automatically adapting to light/dark menu bar.
fn create_tray_icon() -> Result<Image<'static>, Box<dyn std::error::Error>> {
    let icon_bytes = include_bytes!("../icons/tray-iconTemplate@2x.png");
    let img = Image::from_bytes(icon_bytes)?.to_owned();
    Ok(img)
}

/// Update tray from the unified AppStatus — single source of truth.
/// Both tray and popup read from the same struct, emitted at the same moment.
pub fn update_tray<R: Runtime>(
    app: &AppHandle<R>,
    status: &AppStatus,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        if !status.dev_scan_complete {
            // Pre-scan: don't assert any clean state
            tray.set_title(Some(""))?;
            tray.set_tooltip(Some("SymbolSweep \u{2014} Scanning\u{2026}"))?;
        } else {
            match status.clean_state {
                CleanState::Clean => {
                    // Icon only — nothing meaningful to clean
                    tray.set_title(Some(""))?;
                }
                CleanState::Runaway => {
                    // Distinct label naming the actual problem
                    tray.set_title(Some(&format!("Cache runaway \u{2014} {}", status.cache.size_display)))?;
                }
                CleanState::Moderate | CleanState::Heavy => {
                    // Show reclaimable total (same value as popup hero)
                    tray.set_title(Some(&format!("{} to clean", status.reclaimable_display)))?;
                }
            }

            // Tooltip always shows full breakdown
            let tooltip = format!(
                "SymbolSweep\nReclaimable: {}\nDisk: {} free of {}\nStatus: {}",
                status.reclaimable_display,
                status.disk_free_display,
                status.disk_total_display,
                match status.clean_state {
                    CleanState::Clean => "All clean",
                    CleanState::Moderate => "Moderate",
                    CleanState::Heavy => "Heavy",
                    CleanState::Runaway => "Runaway cache",
                }
            );
            tray.set_tooltip(Some(&tooltip))?;
        }
    }

    Ok(())
}

/// Position window in top-right corner (near menu bar)
/// Uses fixed window dimensions to avoid measurement inconsistencies
fn position_window_near_tray<R: Runtime>(window: &tauri::WebviewWindow<R>) {
    use tauri::PhysicalPosition;

    // FIRST: Ensure window is hidden to prevent flash
    let _ = window.hide();

    // Use FIXED window dimensions (from tauri.conf.json) to avoid measurement issues
    // when window is hidden - outer_size() can return wrong values
    const WINDOW_WIDTH: i32 = 280;
    const MARGIN_RIGHT: i32 = 10;
    const MENU_BAR_HEIGHT: i32 = 30;

    // Calculate position based on primary monitor
    if let Ok(Some(monitor)) = window.primary_monitor() {
        let screen_size = monitor.size();
        let scale = monitor.scale_factor();

        // Account for scale factor on Retina displays
        let screen_width = (screen_size.width as f64 / scale) as i32;

        let x = screen_width - WINDOW_WIDTH - MARGIN_RIGHT;
        let y = MENU_BAR_HEIGHT;

        // Set position (physical pixels on macOS)
        let _ = window.set_position(PhysicalPosition::new(
            (x as f64 * scale) as i32,
            (y as f64 * scale) as i32,
        ));
    } else {
        // Fallback to positioner plugin
        let _ = window.move_window(Position::TopRight);
    }
}

/// Send a macOS notification with multiple fallback methods
/// 1. Tauri plugin (works in signed production builds)
/// 2. terminal-notifier (works if installed via Homebrew)
/// 3. osascript (may require Script Editor permissions)
pub fn send_notification<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;

    // Try Tauri notification plugin first
    let result = app.notification()
        .builder()
        .title(title)
        .body(body)
        .show();

    match &result {
        Ok(_) => {
            // In release builds, trust that Tauri notification worked
            #[cfg(not(debug_assertions))]
            return;
            // In debug builds, fall through to terminal-notifier since Tauri silently fails for unsigned apps
        }
        Err(_) => {}
    }

    #[cfg(target_os = "macos")]
    {
        // Fallback to terminal-notifier (works if installed)
        let result = std::process::Command::new("terminal-notifier")
            .arg("-title")
            .arg(title)
            .arg("-message")
            .arg(body)
            .arg("-sound")
            .arg("default")
            .output();

        if matches!(&result, Ok(output) if output.status.success()) {
            return;
        }

        // Last resort: osascript
        let script = format!(
            r#"display notification "{}" with title "{}" sound name "Glass""#,
            body.replace('"', r#"\""#),
            title.replace('"', r#"\""#),
        );
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output();
    }
}

