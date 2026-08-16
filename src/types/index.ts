// Type definitions for SymbolSweep

export type CacheState = 'Normal' | 'Warning' | 'Critical';
export type CleanState = 'Clean' | 'Moderate' | 'Heavy' | 'Runaway';

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

// Unified app status — single source of truth for tray and popup
export type DiskHealth = 'Normal' | 'Warning' | 'Critical' | 'Unknown';

export interface AppStatus {
  disk_free_bytes: number;
  disk_free_display: string;
  disk_total_bytes: number;
  disk_total_display: string;
  disk_health: DiskHealth;
  cache: CacheStatus;
  dev_total_bytes: number;
  dev_total_display: string;
  dev_scan_available: boolean;
  reclaimable_bytes: number;
  reclaimable_display: string;
  clean_state: CleanState;
  show_gap_banner: boolean;
  snapshot_count: number;
  dev_scan_complete: boolean;
  autoclean_failing: boolean;
}

// Dev artifact scanning types
export type ArtifactTier = 'Safe' | 'Rebuildable' | 'SafeWithReinstall' | 'Ask';

export interface DevArtifact {
  path: string;
  size_bytes: number;
  size_display: string;
  tier: ArtifactTier;
  kind: string;
  project: string | null;
  staleness_days: number | null;
  is_nested: boolean;
  hint: string | null;
  active_build: boolean;
}

export interface DevScanResult {
  artifacts: DevArtifact[];
  total_bytes: number;
  total_display: string;
  safe_bytes: number;
  safe_display: string;
  rebuildable_bytes: number;
  rebuildable_display: string;
  safe_with_reinstall_bytes: number;
  safe_with_reinstall_display: string;
  ask_bytes: number;
  ask_display: string;
  scan_duration_ms: number;
  scan_roots: string[];
}

export interface DevDeleteResult {
  deleted_count: number;
  bytes_freed: number;
  bytes_freed_display: string;
  errors: string[];
}

export interface SsTrashInfo {
  count: number;
  total_bytes: number;
  total_display: string;
}

export interface PurgeResult {
  purged_count: number;
  bytes_freed: number;
  bytes_freed_display: string;
  errors: string[];
}

// License types
export interface LicenseStatus {
  status: 'NotActivated' | 'Valid' | 'Revalidated' | 'Rejected' | 'FailOpen';
  message?: string;
}

export interface ActivationResult {
  success: boolean;
  error: string | null;
  instance_id: string | null;
  is_limit_reached: boolean;
  activation_limit: number | null;
  activation_usage: number | null;
}

// Debug preset sizes (simulate reclaimable for CleanState testing)
export const DEBUG_SIZES = {
  EMPTY: 0,
  SMALL: 1 * 1024 * 1024 * 1024,      // 1GB
  MODERATE: 3 * 1024 * 1024 * 1024,   // 3GB
  HEAVY: 7 * 1024 * 1024 * 1024,      // 7GB
} as const;
