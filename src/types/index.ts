// Type definitions for SymbolSweep

export type CacheState = 'Normal' | 'Warning' | 'Critical';

export interface CacheStatus {
  size_bytes: number;
  size_display: string;
  state: CacheState;
  path: string;
  exists: boolean;
  file_count: number;
  last_checked: number;
}

export interface DeletionItem {
  path: string;
  size: number;
  size_display: string;
  is_directory: boolean;
}

export interface CleanResult {
  success: boolean;
  bytes_freed: number;
  bytes_freed_display: string;
  files_removed: number;
  timestamp: number;
  message: string;
  requires_password: boolean;
  was_dry_run: boolean;
  items_found: DeletionItem[];
}

export interface Settings {
  auto_clean_on_threshold: boolean;
  auto_clean_threshold: number;
  auto_clean_scheduled: boolean;
  auto_clean_interval_secs: number;
  show_notifications: boolean;
  launch_at_login: boolean;
  last_clean_timestamp: number;
  monitor_interval_secs: number;
  debug_mode: boolean;
  debug_simulated_size: number;
  first_run_completed: boolean;
  first_clean_confirmed: boolean;
  dev_scan_roots: string[];
}

// Dev artifact scanning types
export type ArtifactTier = 'Safe' | 'SafeWithReinstall' | 'Ask';

export interface DevArtifact {
  path: string;
  size_bytes: number;
  size_display: string;
  tier: ArtifactTier;
  kind: string;
  project: string | null;
  staleness_days: number | null;
  is_nested: boolean;
}

export interface DevScanResult {
  artifacts: DevArtifact[];
  total_bytes: number;
  total_display: string;
  safe_bytes: number;
  safe_display: string;
  safe_with_reinstall_bytes: number;
  safe_with_reinstall_display: string;
  ask_bytes: number;
  ask_display: string;
  scan_duration_ms: number;
  scan_roots: string[];
}

// Debug preset sizes
export const DEBUG_SIZES = {
  EMPTY: 0,
  SMALL: 3 * 1024 * 1024 * 1024,      // 3GB - Normal
  WARNING: 7 * 1024 * 1024 * 1024,    // 7GB - Warning
  CRITICAL: 15 * 1024 * 1024 * 1024,  // 15GB - Critical
} as const;

// Threshold constants (should match Rust)
export const WARNING_THRESHOLD = 5 * 1024 * 1024 * 1024; // 5GB
export const CRITICAL_THRESHOLD = 10 * 1024 * 1024 * 1024; // 10GB
