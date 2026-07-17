import { useState, useEffect, useRef, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow, LogicalSize } from '@tauri-apps/api/window';
import { useAppStatus, useCleanCache, useLastCleanTime, useDevScan, useDeleteDevArtifacts } from '../hooks/useCacheStatus';
import { useSettings } from '../hooks/useSettings';
import { CleanConfirmation } from './CleanConfirmation';
import type { CleanState, CleanResult } from '../types';
import './StatusPanel.css';

/** Format bytes to human-readable string (matches Rust format_size) */
function formatSize(bytes: number): string {
  const KB = 1024;
  const MB = KB * 1024;
  const GB = MB * 1024;
  const GB_THRESHOLD = 1000 * MB;

  if (bytes >= GB_THRESHOLD) {
    const value = bytes / GB;
    const rounded = Math.round(value * 10) / 10;
    if (Math.abs(rounded - Math.floor(rounded)) < 0.01) {
      return `${Math.floor(rounded)} GB`;
    }
    return `${rounded.toFixed(1)} GB`;
  } else if (bytes >= MB) {
    return `${Math.floor(bytes / MB)} MB`;
  } else if (bytes >= KB) {
    return `${Math.floor(bytes / KB)} KB`;
  } else if (bytes > 0) {
    return `${bytes} B`;
  }
  return '0 B';
}

interface StatusIndicatorProps {
  state: CleanState;
  value: string | null;
  label: string;
}

function StatusIndicator({ state, value, label }: StatusIndicatorProps) {
  const stateConfig = {
    Clean: { label: 'All clean' },
    Moderate: { label: 'Moderate' },
    Heavy: { label: 'Heavy' },
  };

  const config = stateConfig[state];
  const stateClass = state.toLowerCase();

  return (
    <div className="status-indicator">
      <div className={`status-size ${stateClass}`}>
        {value ? (
          <>
            <span className="hero-value">{value}</span>
            <span className="hero-label">{label}</span>
          </>
        ) : (
          label
        )}
      </div>
      {state !== 'Clean' && (
        <div className={`status-state ${stateClass}`}>
          <span className="status-dot" />
          <span className="status-label">{config.label}</span>
        </div>
      )}
    </div>
  );
}

interface StatusPanelProps {
  onSettingsClick: () => void;
  onDevScanClick: () => void;
}

export function StatusPanel({ onSettingsClick, onDevScanClick }: StatusPanelProps) {
  const { status: appStatus, loading, error, refresh } = useAppStatus();
  const { clean, dryRun, cleaning } = useCleanCache();
  const { lastCleanTime, lastCleanFreed, refresh: refreshLastClean } = useLastCleanTime();
  const { settings, updateSetting } = useSettings();
  const { result: devResult } = useDevScan();
  const { deleteArtifacts } = useDeleteDevArtifacts();

  const [showConfirmation, setShowConfirmation] = useState(false);
  const [dryRunResult, setDryRunResult] = useState<CleanResult | null>(null);
  const [bannerFading, setBannerFading] = useState(false);
  const [showBanner, setShowBanner] = useState(false);
  const [isLoading, setIsLoading] = useState(false);
  const [bannerMessage, setBannerMessage] = useState<string | null>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  // Auto-resize window to fit panel content
  const resizeWindow = useCallback(() => {
    if (!panelRef.current) return;
    const height = Math.ceil(panelRef.current.scrollHeight);
    if (height > 0) {
      getCurrentWindow().setSize(new LogicalSize(280, height));
    }
  }, []);

  useEffect(() => {
    const el = panelRef.current;
    if (!el) return;
    const observer = new ResizeObserver(() => resizeWindow());
    observer.observe(el);
    resizeWindow();
    return () => observer.disconnect();
  }, [resizeWindow]);

  // Delayed scanning indicator: only show after 350ms of !dev_scan_complete.
  // If the scan finishes faster, the user sees nothing — straight to results.
  // DEBUG: set SCAN_LOADER_DELAY to e.g. 0 and add `|| true` to the condition to force-show.
  const SCAN_LOADER_DELAY = 350;
  const [showScanning, setShowScanning] = useState(false);
  const scanTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    const scanPending = appStatus && !appStatus.dev_scan_complete;
    if (scanPending) {
      scanTimerRef.current = setTimeout(() => setShowScanning(true), SCAN_LOADER_DELAY);
    } else {
      setShowScanning(false);
      if (scanTimerRef.current) {
        clearTimeout(scanTimerRef.current);
        scanTimerRef.current = null;
      }
    }
    return () => {
      if (scanTimerRef.current) clearTimeout(scanTimerRef.current);
    };
  }, [appStatus?.dev_scan_complete]);

  const showCleanBanner = (message: string) => {
    setBannerMessage(message);
    setShowBanner(true);
    setBannerFading(false);
  };

  // Auto-dismiss success banner after 5 seconds
  useEffect(() => {
    if (!showBanner) return;

    const fadeTimer = setTimeout(() => {
      setBannerFading(true);
    }, 4700);

    const removeTimer = setTimeout(() => {
      setShowBanner(false);
      setBannerFading(false);
      setBannerMessage(null);
    }, 5000);

    return () => {
      clearTimeout(fadeTimer);
      clearTimeout(removeTimer);
    };
  }, [showBanner]);

  const handleCleanClick = () => {
    if (!settings.first_clean_confirmed) {
      setShowConfirmation(true);
      setDryRunResult(null);
    } else {
      performClean();
    }
  };

  const handleDryRun = async () => {
    try {
      const result = await dryRun();
      setDryRunResult(result);
    } catch (err) {
      console.error('Dry run failed:', err);
    }
  };

  const handleConfirmClean = async () => {
    await updateSetting('first_clean_confirmed', true);
    setShowConfirmation(false);
    performClean();
  };

  const performClean = async () => {
    setIsLoading(true);
    // Yield to browser so "Cleaning..." state paints before heavy I/O
    await new Promise(resolve => requestAnimationFrame(resolve));
    try {
      // Snapshot disk free space BEFORE cleaning
      const beforeFree = appStatus?.disk_free_bytes ?? 0;

      // Clean system cache
      const sysResult = await clean(false);
      let totalFreed = sysResult.bytes_freed;

      // Clean ONLY Safe-tier dev artifacts
      if (devResult) {
        const safePaths = devResult.artifacts
          .filter(a => !a.is_nested && a.tier === 'Safe' && !a.active_build)
          .map(a => a.path);
        if (safePaths.length > 0) {
          const devDeleteResult = await deleteArtifacts(safePaths);
          totalFreed += devDeleteResult.bytes_freed;
        }
      }

      refresh();
      await refreshLastClean();

      // Snapshot disk free space AFTER cleaning (re-fetch from backend)
      let diskDelta = totalFreed;
      try {
        const afterStatus = await invoke<{ disk_free_bytes: number }>('get_app_status');
        const afterFree = afterStatus.disk_free_bytes;
        if (afterFree > beforeFree) {
          diskDelta = afterFree - beforeFree;
        }
      } catch {
        // fall back to reported bytes freed
      }

      // Show disk-free delta, not just bytes unlinked
      if (diskDelta > 0) {
        showCleanBanner(`Freed ${formatSize(diskDelta)} disk space`);
      } else if (totalFreed > 0) {
        showCleanBanner(`Freed ${formatSize(totalFreed)}`);
      } else {
        const devRemaining = appStatus?.dev_total_bytes ?? 0;
        showCleanBanner(devRemaining > 0 ? 'Caches already clean' : 'Already clean');
      }
    } catch (err) {
      console.error('Clean failed:', err);
    } finally {
      setIsLoading(false);
    }
  };

  if (showConfirmation) {
    return (
      <CleanConfirmation
        onConfirm={handleConfirmClean}
        onCancel={() => setShowConfirmation(false)}
        onDryRun={handleDryRun}
        dryRunResult={dryRunResult}
        loading={cleaning}
      />
    );
  }

  if (loading) {
    return (
      <div className="status-panel" ref={panelRef}>
        <div className="status-loading">Loading...</div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="status-panel" ref={panelRef}>
        <div className="status-error">
          <p>Error: {error}</p>
          <button onClick={refresh}>Retry</button>
        </div>
      </div>
    );
  }

  if (!appStatus) {
    return (
      <div className="status-panel" ref={panelRef}>
        <div className="status-error">No status available</div>
      </div>
    );
  }

  // Pre-scan: suppress "All clean" until scan completes.
  // If scan finishes within SCAN_LOADER_DELAY ms, skip the loader entirely.
  // DEBUG: set SCAN_LOADER_DELAY=0 to show immediately.
  //        Add `|| true` to the `scanPending` condition in the useEffect to hold open.
  if (!appStatus.dev_scan_complete) {
    return (
      <div className="status-panel scan-shell" ref={panelRef}>
        {showScanning && (
          <div className="scan-loader">
            <div className="loading-logo" aria-hidden="true" />
          </div>
        )}
      </div>
    );
  }

  const stateClass = appStatus.clean_state.toLowerCase();

  // Build the last-clean summary: "Last clean freed 1.2 GB" or "No recent cleans"
  const lastCleanSummary = lastCleanFreed
    ? `Last clean freed ${lastCleanFreed}`
    : null;

  return (
    <div className="status-panel" ref={panelRef}>
      <header className="panel-header">
        <div className="header-logo">
          <div className="logo-icon" aria-hidden="true" />
          <span className="logo-text"><span className="logo-sym">Symbol</span>Sweep</span>
        </div>
        <button className="settings-btn" onClick={onSettingsClick} title="Settings">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
        </button>
      </header>

      {/* Hero -- reclaimable total */}
      <StatusIndicator
        state={appStatus.clean_state}
        value={appStatus.clean_state === 'Clean' ? null : appStatus.reclaimable_display}
        label={
          appStatus.clean_state !== 'Clean'
            ? 'to clean'
            : appStatus.dev_total_bytes > 0
              ? 'Caches clean'
              : appStatus.show_gap_banner
                ? 'Nothing to clean'
                : 'All clean'
        }
      />

      {/* Secondary disk context line */}
      {appStatus.disk_health === 'Unknown' ? (
        <span className="disk-context">Disk: unavailable</span>
      ) : (
        <span className={`disk-context${appStatus.disk_health !== 'Normal' ? ' disk-low' : ''}`}>
          {appStatus.disk_free_display} free of {appStatus.disk_total_display}
        </span>
      )}

      <div className="status-content">
        {/* Last clean summary — single line, no expand */}
        {lastCleanTime !== 'Never' && lastCleanTime !== 'Loading...' && (
          <div className="last-clean-summary">
            <span className="last-clean-label">Last cleaned</span>
            <span className="last-clean-time">{lastCleanTime}</span>
            {lastCleanSummary && (
              <>
                <span className="last-clean-sep">&middot;</span>
                <span className="last-clean-freed">{lastCleanSummary}</span>
              </>
            )}
          </div>
        )}

        {appStatus.show_gap_banner && (
          <div className="gap-banner">
            <span>Your disk is still low — most usage is outside SymbolSweep's reach.</span>
            {appStatus.snapshot_count > 0 && (
              <span className="gap-snapshots"> {appStatus.snapshot_count} snapshot{appStatus.snapshot_count !== 1 ? 's' : ''} detected.</span>
            )}
            {' '}
            <a className="gap-link" onClick={() => invoke('open_storage_settings')}>Manage Storage &rsaquo;</a>
          </div>
        )}

      </div>

      {showBanner && bannerMessage && (
        <div className={`clean-result${bannerFading ? ' fading-out' : ''}`}>
          <span className="result-icon">&#10003;</span>
          <span>{bannerMessage}</span>
        </div>
      )}

      <div className="status-footer">
        {appStatus.clean_state !== 'Clean' && (
          <button
            className={`clean-btn ${stateClass}${isLoading ? ' loading' : ''}`}
            onClick={handleCleanClick}
            disabled={isLoading || cleaning}
          >
            {isLoading ? (
              <span className="loading-text">
                Cleaning<span className="loading-dots"><span>.</span><span>.</span><span>.</span></span>
              </span>
            ) : (
              'Clean Now'
            )}
          </button>
        )}

        {appStatus.dev_scan_available && (
          <button className="dev-scan-link" onClick={onDevScanClick}>
            <span className="dev-scan-total">
              {appStatus.dev_total_bytes > 0 ? `${appStatus.dev_total_display} dev artifacts` : 'Dev artifacts'}
            </span>
            <span className="dev-scan-arrow">&rsaquo;</span>
          </button>
        )}
      </div>
    </div>
  );
}
