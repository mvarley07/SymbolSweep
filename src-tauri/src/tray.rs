use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Runtime,
};
use tauri_plugin_positioner::{Position, WindowExt};

use crate::cache_monitor::{CacheState, CacheStatus};

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
    let icon = create_minimal_icon()?;

    // Build tray - text only, minimal icon
    // IMPORTANT: The returned TrayIcon MUST be stored somewhere to prevent it from being dropped
    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .icon_as_template(true)
        .menu(&menu)
        .title("0 B") // Initial title - will be updated
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

                    position_window_near_tray(&window);
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

/// Create a minimal transparent icon (macOS requires an icon, but we use title for display)
fn create_minimal_icon() -> Result<Image<'static>, Box<dyn std::error::Error>> {
    // 16x16 transparent image using raw RGBA data
    // Each pixel is 4 bytes: R, G, B, A (all zeros = fully transparent)
    let width = 16u32;
    let height = 16u32;
    let rgba_data: Vec<u8> = vec![0u8; (width * height * 4) as usize];

    Ok(Image::new_owned(rgba_data, width, height))
}

/// Format the tray title with status indicator
/// Uses flat colored circles - renders cleanly on macOS
fn format_tray_title(status: &CacheStatus) -> String {
    let indicator = match status.state {
        CacheState::Normal => "🟢",   // Green - healthy
        CacheState::Warning => "🟠",  // Orange - attention needed
        CacheState::Critical => "🔴", // Red - urgent
    };

    format!("{} {}", indicator, status.size_display)
}

/// Update tray with current cache status
pub fn update_tray_icon<R: Runtime>(
    app: &AppHandle<R>,
    status: &CacheStatus,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        // Update title with size and status indicator
        let title = format_tray_title(status);
        tray.set_title(Some(&title))?;

        // Update tooltip with more details
        let tooltip = format!(
            "SymbolSweep\n{} - {} files\nStatus: {}",
            status.size_display,
            status.file_count,
            match status.state {
                CacheState::Normal => "Normal",
                CacheState::Warning => "Warning (5GB+)",
                CacheState::Critical => "Critical (10GB+)",
            }
        );
        tray.set_tooltip(Some(&tooltip))?;
    }

    Ok(())
}

/// Position window in top-right corner (near menu bar)
fn position_window_near_tray<R: Runtime>(window: &tauri::WebviewWindow<R>) {
    // Use TopRight position which is reliable and near the menu bar area
    // TrayBottomCenter can panic if tray position is not available
    let _ = window.move_window(Position::TopRight);
}

/// Send a macOS notification with sound
pub fn send_notification_with_sound(title: &str, body: &str, sound: &str) {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            r#"display notification "{}" with title "{}" sound name "{}""#,
            body.replace('"', r#"\""#),
            title.replace('"', r#"\""#),
            sound
        );

        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output();
    }
}
