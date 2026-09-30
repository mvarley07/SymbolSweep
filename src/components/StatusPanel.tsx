import { useState, useEffect, useRef, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppStatus, useCleanCache, useLastCleanTime, useDevScan, useDeleteDevArtifacts, useDeleteDevArtifactsManual } from '../hooks/useCacheStatus';
import { useSettings } from '../hooks/useSettings';
import { CleanConfirmation } from './CleanConfirmation';
import { ArtifactRow } from './ArtifactRows';
import { fitWindowTo } from '../windowSize';
import { useLicense } from '../license';
import { formatSize } from '../formatSize';
import type { CleanState, CleanResult, DevScanResult } from '../types';
import './StatusPanel.css';

/** The Safe rows Clean deletes: exactly the ones shown as deletable (not
 *  building, not held back). The button, the ready line and the delete all
 *  use this one list, so the amount shown is the amount removed. */
function cleanableRows(devResult: DevScanResult | null) {
  return devResult ? devResult.artifacts.filter(a => a.tier === 'Safe' && !a.active_build && !a.in_use) : [];
}

const sumBytes = (rows: { size_bytes: number }[]) => rows.reduce((n, a) => n + a.size_bytes, 0);

interface StatusIndicatorProps {
  state: CleanState;
  value: string | null;
  label: string;
  /** What Clean Now removes right now: "225 MB ready to clean now" */
  ready: string | null;
  /** How the headline splits into dev artifacts and system cache */
  breakdown: string;
  /** Opens Dev Artifacts; the hero is the way in when there are artifacts */
  onOpen?: () => void;
}

function StatusIndicator({ state, value, label, ready, breakdown, onOpen }: StatusIndicatorProps) {
  // Moderate/Heavy grade everything found (cache + all dev tiers); the hero
  // label above already says FOUND. Runaway is the system cache alone.
  const stateConfig = {
    Clean: { label: 'All clean' },
    Moderate: { label: 'Moderate' },
    Heavy: { label: 'Heavy' },
    Runaway: { label: 'Cache: runaway' },
  };

  const config = stateConfig[state];
  const stateClass = state.toLowerCase();

  return (
    <div className="status-indicator">
      {value && onOpen ? (
        <button className={`status-size hero-link ${stateClass}`} onClick={onOpen} title="Open Dev Artifacts" data-open-devscan>
          <span className="hero-value">{value}</span>
          <span className="hero-label">{label}<span className="hero-arrow">&rsaquo;</span></span>
        </button>
      ) : (
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
      )}
      {value && ready && (
        <div className="hero-ready">{ready}</div>
      )}
      {value && breakdown && (
        <div className="hero-breakdown">{breakdown}</div>
      )}
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
  const { licensed, requestUnlock } = useLicense();
  const { result: devResult } = useDevScan();
  const { deleteArtifacts } = useDeleteDevArtifacts();
  const { deleteArtifacts: deleteOneArtifact, deleting: deletingRow } = useDeleteDevArtifactsManual();

  const [showConfirmation, setShowConfirmation] = useState(false);
  const [dryRunResult, setDryRunResult] = useState<CleanResult | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [freshCleanFreed, setFreshCleanFreed] = useState<string | null>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  // Auto-resize window to fit panel content (capped; the SAFE list scrolls past it)
  const resizeWindow = useCallback(() => {
    if (panelRef.current) fitWindowTo(panelRef.current);
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

  // Auto-clear the transient "Freed X" display after 5 seconds
  useEffect(() => {
    if (!freshCleanFreed) return;
    const timer = setTimeout(() => setFreshCleanFreed(null), 5000);
    return () => clearTimeout(timer);
  }, [freshCleanFreed]);

  const handleCleanClick = () => {
    if (!licensed) return requestUnlock(cleanBytes);
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
      // The rows the button counted, taken before anything changes. Rows that
      // were held back when the popup rendered are never added; one that became
      // held back since is skipped by the backend guard and shown after the rescan.
      const paths = cleanableRows(devResult).map(a => a.path);

      // Clean system cache
      const sysResult = await clean(false);
      let totalFreed = sysResult.bytes_freed;

      if (paths.length > 0) {
        const devDeleteResult = await deleteArtifacts(paths);
        totalFreed += devDeleteResult.bytes_freed;
      }

      refresh();
      await refreshLastClean();

      // Report what was removed, not the change in free disk space (which
      // other apps move too), so it matches the button
      setFreshCleanFreed(totalFreed > 0 ? `Cache cleaned ${formatSize(totalFreed)}` : null);
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

  // Pre-scan: show skeleton until scan completes.
  // If scan finishes within SCAN_LOADER_DELAY ms, skip the skeleton entirely.
  // DEBUG: set SCAN_LOADER_DELAY=0 to show immediately.
  //        Add `|| true` to the `scanPending` condition in the useEffect to hold open.
  if (!appStatus.dev_scan_complete) {
    return (
      <div className="status-panel" ref={panelRef}>
        {showScanning && (
          <>
            <header className="panel-header">
              <div className="header-logo">
                <div className="logo-icon" aria-hidden="true" />
                <span className="logo-text"><span className="logo-sym">Symbol</span>Sweep</span>
              </div>
              <div className="skel skel-settings" />
            </header>
            <div className="status-indicator">
              <div className="skel skel-hero" />
              <div className="skel skel-hero-label" />
            </div>
            <div className="skel skel-disk" />
            <div className="status-content">
              <div className="skel skel-row" />
            </div>
            <div className="status-footer">
              <div className="skel skel-btn" />
            </div>
          </>
        )}
      </div>
    );
  }

  const stateClass = appStatus.clean_state.toLowerCase();

  // Button scope: cache + the deletable Safe rows, the same list Clean deletes
  const cleanRows = cleanableRows(devResult);
  const cleanBytes = appStatus.cache.size_bytes + sumBytes(cleanRows);
  const safeCleanableDisplay = formatSize(cleanBytes);
  // Threshold: below 1 MB, the safe-clean button is effectively empty
  const safeCleanMeaningful = cleanBytes >= 1024 * 1024;
  // Whether non-safe dev artifacts hold meaningful space (>= 10 MB)
  const hasNonSafeArtifacts = appStatus.dev_review_bytes >= 10 * 1024 * 1024;

  // Every SAFE row, on the main screen: delete per row, or the reason it's skipped
  const safeRows = devResult ? devResult.artifacts.filter(a => a.tier === 'Safe') : [];
  // Safe rows held back right now (building or in use)
  const heldBackBytes = sumBytes(safeRows.filter(a => a.active_build || a.in_use));
  const handleDeleteSafeRow = async (path: string) => {
    if (!licensed) return requestUnlock(safeRows.find(a => a.path === path)?.size_bytes);
    try {
      // Backend rescans and emits; rows, hero and tray refresh from that rescan
      await deleteOneArtifact([path]);
    } catch (err) {
      console.error('Delete failed:', err);
    }
  };

  // Hero sub-line: what Clean Now removes right now, or why that's nothing
  const heroReady = cleanBytes > 0
    ? `${safeCleanableDisplay} ready to clean now`
    : heldBackBytes > 0
      ? `0 B ready \u00b7 ${formatSize(heldBackBytes)} held back`
      : null;

  // Build the resting summary suffix: "freed 1.2 GB"
  const lastCleanSummary = lastCleanFreed
    ? `freed ${lastCleanFreed}`
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

      {/* Hero -- reclaimable total (or cache-specific for Runaway) */}
      <StatusIndicator
        state={appStatus.clean_state}
        value={appStatus.headline_bytes > 0 ? appStatus.headline_display : null}
        label={appStatus.headline_label}
        ready={heroReady}
        breakdown={appStatus.cache.size_bytes > 0 ? appStatus.breakdown_display : ''}
        onOpen={appStatus.dev_scan_available && appStatus.dev_total_bytes > 0 ? onDevScanClick : undefined}
      />

      {/* Secondary disk context line */}
      {appStatus.disk_health === 'Unknown' ? (
        <span className="disk-context">Disk: unavailable</span>
      ) : (
        <span className={`disk-context${appStatus.disk_health !== 'Normal' ? ' disk-low' : ''}`}>
          {appStatus.disk_free_display} free of {appStatus.disk_total_display}
        </span>
      )}

      {safeRows.length > 0 && (
        <div className="safe-to-clean">
          <div className="scan-total">
            {/* Label only: the ready line and the button already give the amount */}
            <span className="total-label">Safe to clean</span>
          </div>
          <div className="artifacts-list">
            {safeRows.map(artifact => (
              <ArtifactRow
                key={artifact.path}
                artifact={artifact}
                onDelete={handleDeleteSafeRow}
                deleting={deletingRow || isLoading}
              />
            ))}
          </div>
        </div>
      )}

      <div className="status-content">
        {/* Last clean summary — single line, no expand */}
        {lastCleanTime !== 'Never' && lastCleanTime !== 'Loading...' && (
          <div className={`last-clean-summary${freshCleanFreed ? ' fresh' : ''}`}>
            {freshCleanFreed ? (
              <>
                <span className="last-clean-freed-highlight">{freshCleanFreed}</span>
                <span className="last-clean-sep">&middot;</span>
                <span className="last-clean-time">just now</span>
              </>
            ) : (
              <>
                <span className="last-clean-label">Cache cleaned</span>
                <span className="last-clean-time">{lastCleanTime}</span>
                {lastCleanSummary && (
                  <>
                    <span className="last-clean-sep">&middot;</span>
                    <span className="last-clean-freed">{lastCleanSummary}</span>
                  </>
                )}
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

        {appStatus.autoclean_failing && (
          <div className="autoclean-fail-banner">
            <span className="fail-icon">&#9888;</span>
            <span>Autoclean is failing repeatedly. Try a manual clean or check disk permissions.</span>
          </div>
        )}

      </div>

      <div className="status-footer">
        {/* One CTA: clean what's safe now, else review the rest */}
        {safeCleanMeaningful ? (
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
              `Clean ${safeCleanableDisplay} safely`
            )}
          </button>
        ) : hasNonSafeArtifacts && (
          <button
            className="clean-btn review-artifacts"
            onClick={onDevScanClick}
          >
            {`Review ${appStatus.dev_review_display} in dev artifacts`}
            <span className="review-arrow">&rsaquo;</span>
          </button>
        )}
      </div>
    </div>
  );
}
