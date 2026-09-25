import { getCurrentWindow, LogicalSize } from '@tauri-apps/api/window';

/** tauri.conf.json width (min = max, not resizable) */
export const WINDOW_WIDTH = 280;

/** tauri.conf.json minHeight / maxHeight — Tauri clamps setSize to these anyway */
const CONFIG_MIN_HEIGHT = 200;
const CONFIG_MAX_HEIGHT = 580;

/** The window's top edge sits this far below the screen top (tray.rs MENU_BAR_HEIGHT) */
const WINDOW_TOP = 30;

/** Gap kept between the window's bottom edge and the Dock / screen edge */
const BOTTOM_MARGIN = 8;

/**
 * Tallest the popup may be: never past the screen's usable area (below the
 * menu bar, above the Dock), and never past tauri.conf's maxHeight.
 */
export function maxWindowHeight(): number {
  const s = window.screen as Screen & { availTop?: number };
  const usableBottom = (s.availTop ?? 0) + s.availHeight;
  const fits = usableBottom - WINDOW_TOP - BOTTOM_MARGIN;
  return Math.max(CONFIG_MIN_HEIGHT, Math.min(CONFIG_MAX_HEIGHT, fits));
}

/**
 * Publish the cap to CSS (--window-max-h) so panels can bound themselves and
 * scroll their lists instead of growing past the window.
 */
export function publishWindowMaxHeight(): number {
  const max = maxWindowHeight();
  document.documentElement.style.setProperty('--window-max-h', `${max}px`);
  return max;
}

/** Resize the popup, clamped to maxWindowHeight(). Returns the height applied. */
export function setWindowHeight(height: number): number {
  const clamped = Math.min(Math.ceil(height), publishWindowMaxHeight());
  getCurrentWindow().setSize(new LogicalSize(WINDOW_WIDTH, clamped));
  return clamped;
}
