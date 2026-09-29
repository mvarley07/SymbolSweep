import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useAppStatus, useDevScan, useDeleteDevArtifacts, useDeleteDevArtifactsManual } from '../hooks/useCacheStatus';
import type { DevArtifact, DevDeleteResult, SkippedArtifact, SsTrashInfo, PurgeResult } from '../types';
import { ArtifactRow, SkippedRow } from './ArtifactRows';
import { fitWindowTo } from '../windowSize';
import { formatSize } from '../formatSize';
import './DevScanPanel.css';

const LEGEND_SEEN_KEY = 'symbolsweep:tier-legend-seen';
const REVIEW_MIN_BYTES = 10 * 1024 * 1024;
const TRASH_BANNER_MIN_BYTES = 1024 * 1024;

interface DevScanPanelProps {
  onBack: () => void;
}

export function DevScanPanel({ onBack }: DevScanPanelProps) {
  const { result, scanning, error, scan } = useDevScan();
  // Same AppStatus as the tray and hero, for the dev-vs-combined breakdown line
  const { status: appStatus } = useAppStatus();
  const { deleteArtifacts: bulkDeleteArtifacts, deleting: bulkDeleting } = useDeleteDevArtifacts();
  const { deleteArtifacts: manualDeleteArtifacts, deleting: manualDeleting } = useDeleteDevArtifactsManual();
  const deleting = bulkDeleting || manualDeleting;
  const [deleteMessage, setDeleteMessage] = useState<string | null>(null);
  const [deletedToTrash, setDeletedToTrash] = useState(false);
  const [trashInfo, setTrashInfo] = useState<SsTrashInfo | null>(null);
  const [purging, setPurging] = useState(false);
  const [purgeProgress, setPurgeProgress] = useState<{ current: number; total: number; bytes_freed_so_far: string } | null>(null);
  const [confirmPurge, setConfirmPurge] = useState(false);
  // Legend: expanded once on first-ever visit, collapsed by default thereafter.
  // "?" in header toggles it open/closed within a session without re-persisting.
  const [legendExpanded, setLegendExpanded] = useState(() => {
    // First-ever open: show expanded, then mark as seen
    if (localStorage.getItem(LEGEND_SEEN_KEY) !== 'true') {
      localStorage.setItem(LEGEND_SEEN_KEY, 'true');
      return true;
    }
    return false;
  });
  const [confirmRebuild, setConfirmRebuild] = useState(false);
  const [confirmReinstall, setConfirmReinstall] = useState(false);
  // Artifacts the last delete left in place, with the backend's reason
  const [skipped, setSkipped] = useState<SkippedArtifact[]>([]);
  // Artifact list as it was when the last delete ran, for labelling skipped rows
  const [skippedContext, setSkippedContext] = useState<DevArtifact[] | undefined>(undefined);

  // Size the window to the panel's content, like the main screen: a short list
  // gives a short window; a long one fills the allowed height and scrolls.
  // The observer's first callback lands after App's per-view setSize.
  const panelRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = panelRef.current;
    if (!el) return;
    const observer = new ResizeObserver(() => fitWindowTo(el));
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const refreshTrashInfo = useCallback(async () => {
    try {
      const info = await invoke<SsTrashInfo>('get_ss_trash_info');
      // Under 1 MB isn't worth a banner
      setTrashInfo(info.count > 0 && info.total_bytes >= TRASH_BANNER_MIN_BYTES ? info : null);
    } catch {
      setTrashInfo(null);
    }
  }, []);

  // Check for SymbolSweep items in Trash on mount and after deletes
  useEffect(() => { refreshTrashInfo(); }, [refreshTrashInfo]);

  const handlePurgeSsTrash = async () => {
    setConfirmPurge(false);
    setPurging(true);
    setPurgeProgress(null);
    const unlisten = await listen<{ current: number; total: number; bytes_freed_so_far: string }>(
      'trash-purge-progress',
      (event) => { setPurgeProgress(event.payload); },
    );
    try {
      const res = await invoke<PurgeResult>('purge_ss_trash');
      if (res.purged_count > 0 && res.errors.length > 0) {
        showResult(`Freed ${res.bytes_freed_display} — ${res.errors.length} item(s) failed`);
      } else if (res.purged_count > 0) {
        showResult(`Freed ${res.bytes_freed_display} from Trash (${res.purged_count} items)`);
      } else if (res.errors.length > 0) {
        showResult(`Error: ${res.errors[0]}`);
      } else {
        showResult('Trash already empty');
      }
      // Refresh instead of clearing — shows remaining items if partial failure
      refreshTrashInfo();
    } catch {
      showResult('Error emptying Trash');
    } finally {
      unlisten();
      setPurging(false);
      setPurgeProgress(null);
    }
  };

  /** Record a delete's skips; returns a " · N skipped" suffix for the result message */
  const noteSkipped = (res: DevDeleteResult) => {
    const items = res.skipped ?? [];
    setSkipped(items);
    setSkippedContext(result?.artifacts);
    return items.length > 0 ? ` \u00b7 ${items.length} skipped` : '';
  };

  const showResult = (msg: string, trashed = false) => {
    setDeleteMessage(msg);
    setDeletedToTrash(trashed);
    setTimeout(() => { setDeleteMessage(null); setDeletedToTrash(false); }, 4000);
  };

  const handleDeleteOne = async (path: string) => {
    try {
      const artifact = result?.artifacts.find(a => a.path === path);
      const trashed = artifact?.tier === 'Rebuildable' || artifact?.tier === 'SafeWithReinstall' || !!artifact?.is_nested;
      const res = await manualDeleteArtifacts([path]);
      noteSkipped(res);
      if (res.deleted_count > 0) {
        showResult(
          trashed
            ? `Moved ${res.bytes_freed_display} to Trash`
            : `Freed ${res.bytes_freed_display}`,
          trashed,
        );
        if (trashed) refreshTrashInfo();
      }
      if (res.errors.length > 0) showResult(`Error: ${res.errors[0]}`);
    } catch {
      // error state handled by hook
    }
  };

  const handleCleanSafe = async () => {
    if (!result) return;
    const paths = result.artifacts
      .filter(a => a.tier === 'Safe' && !a.active_build && !a.in_use)
      .map(a => a.path);
    if (paths.length === 0) return;
    try {
      const res = await bulkDeleteArtifacts(paths);
      const skippedNote = noteSkipped(res);
      // When nothing was deleted, the skipped list alone explains why
      if (res.deleted_count > 0) {
        showResult(`Freed ${res.bytes_freed_display} (${res.deleted_count} items)${skippedNote}`);
      }
    } catch {
      // error state handled by hook
    }
  };

  /** Top-level rows of a tier the tier button deletes now: not building, not in use */
  const tierCleanable = (tier: DevArtifact['tier']) => (result?.artifacts ?? [])
    .filter(a => a.tier === tier && !a.is_nested && !a.active_build && !a.in_use);
  const rebuildCleanableBytes = tierCleanable('Rebuildable').reduce((n, a) => n + a.size_bytes, 0);
  const reinstallCleanableBytes = tierCleanable('SafeWithReinstall').reduce((n, a) => n + a.size_bytes, 0);

  const handleCleanRebuild = async () => {
    if (!result) return;
    setConfirmRebuild(false);
    const paths = tierCleanable('Rebuildable').map(a => a.path);
    if (paths.length === 0) return;
    try {
      const res = await manualDeleteArtifacts(paths);
      noteSkipped(res);
      if (res.deleted_count > 0) {
        showResult(`Moved ${res.bytes_freed_display} to Trash (${res.deleted_count} items)`, true);
        refreshTrashInfo();
      }
    } catch {
      // error state handled by hook
    }
  };

  const handleCleanReinstall = async () => {
    if (!result) return;
    setConfirmReinstall(false);
    const paths = tierCleanable('SafeWithReinstall').map(a => a.path);
    if (paths.length === 0) return;
    try {
      const res = await manualDeleteArtifacts(paths);
      noteSkipped(res);
      if (res.deleted_count > 0) {
        showResult(`Moved ${res.bytes_freed_display} to Trash (${res.deleted_count} items)`, true);
        refreshTrashInfo();
      }
    } catch {
      // error state handled by hook
    }
  };

  // REVIEW under 10 MB isn't worth a tile or rows: it's shipped output to leave alone
  const showReview = !!result && result.ask_bytes >= REVIEW_MIN_BYTES;
  const visibleArtifacts = result
    ? result.artifacts.filter(a => showReview || a.tier !== 'Ask')
    : [];

  // Tier guide. Before a scan it sits under the header; once rows exist it
  // scrolls with them, so the pinned summary never squeezes the list shut.
  const legend = legendExpanded && (
        <div className="tier-legend">
          <div className="tier-legend-items">
            <div className="tier-legend-item">
              <span className="artifact-tier-badge tier-safe">SAFE</span>
              <span>Free to delete, regenerates automatically</span>
            </div>
            <div className="tier-legend-item">
              <span className="artifact-tier-badge tier-rebuild">REBUILD</span>
              <span>Safe, but takes time to rebuild</span>
            </div>
            <div className="tier-legend-item">
              <span className="artifact-tier-badge tier-reinstall">REINSTALL</span>
              <span>Safe, one command to restore</span>
            </div>
            <div className="tier-legend-item">
              <span className="artifact-tier-badge tier-ask">REVIEW</span>
              <span>May contain data you want; check before deleting</span>
            </div>
          </div>
        </div>
  );

  return (
    <div className="devscan-panel" ref={panelRef}>
      <header className="panel-header">
        <button className="back-btn" onClick={onBack} title="Back">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M19 12H5M12 19l-7-7 7-7" />
          </svg>
        </button>
        <span className="header-title">Dev Artifacts</span>
        <button
          className={`legend-help-btn${legendExpanded ? ' active' : ''}`}
          onClick={() => setLegendExpanded(v => !v)}
          title={legendExpanded ? 'Hide tier guide' : 'Show tier guide'}
        >
          <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
            <circle cx="8" cy="8" r="6.5" />
            <path d="M6.5 6.5a1.5 1.5 0 1 1 1.5 1.5v1" />
            <circle cx="8" cy="11.5" r="0.5" fill="currentColor" stroke="none" />
          </svg>
        </button>
        <button
          className="rescan-btn"
          onClick={() => scan()}
          disabled={scanning}
          title="Rescan"
        >
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            className={scanning ? 'spinning' : ''}
          >
            <path d="M21 12a9 9 0 1 1-3.2-6.9" />
            <path d="M21 3v6h-6" />
          </svg>
        </button>
      </header>

      {!result && legend}

      {error && (
        <div className="scan-error">
          <p>Scan failed: {error}</p>
          <button onClick={() => scan()}>Retry</button>
        </div>
      )}

      {scanning && !result && (
        <div className="scan-loading">
          <div className="scan-spinner" />
          <p>Scanning for dev artifacts...</p>
        </div>
      )}

      {result && (
        <div className="scan-results">
          {/* Pinned: total, tiles, action buttons, messages */}
          <div className="scan-summary">
          <div className="scan-total scan-total-only">
            <span className="total-value">{result.total_display}</span>
          </div>
          {appStatus?.breakdown_display && appStatus.cache.size_bytes > 0 && (
            <div className="scan-breakdown">{appStatus.breakdown_display}</div>
          )}

          <div className="tier-breakdown">
            <div className="tier-row tier-safe">
              <span className="tier-label">SAFE</span>
              <span className="tier-value">{result.safe_bytes > 0 ? result.safe_display : '0 B'}</span>
            </div>
            <div className="tier-row tier-rebuild">
              <span className="tier-label">REBUILD</span>
              <span className="tier-value">{result.rebuildable_bytes > 0 ? result.rebuildable_display : '0 B'}</span>
            </div>
            <div className="tier-row tier-reinstall">
              <span className="tier-label">REINSTALL</span>
              <span className="tier-value">{result.safe_with_reinstall_bytes > 0 ? result.safe_with_reinstall_display : '0 B'}</span>
            </div>
            {showReview && (
              <div className="tier-row tier-ask">
                <span className="tier-label">REVIEW</span>
                <span className="tier-value">{result.ask_display}</span>
              </div>
            )}
          </div>

          {result.safe_deletable_bytes > 0 && (
            <button
              className="clean-safe-btn"
              onClick={handleCleanSafe}
              disabled={deleting}
            >
              {deleting ? 'Cleaning...' : `Clean Safe (${result.safe_deletable_display})`}
            </button>
          )}

          {result.rebuildable_bytes > 0 && (
            confirmRebuild ? (
              <div className="confirm-strip tier-rebuild">
                <span className="confirm-text">Rebuilds take time (cargo build, etc.)</span>
                <button className="confirm-yes" onClick={handleCleanRebuild} disabled={deleting}>Delete</button>
                <button className="confirm-no" onClick={() => setConfirmRebuild(false)}>Cancel</button>
              </div>
            ) : (
              <button
                className="clean-tier-btn tier-rebuild"
                onClick={() => setConfirmRebuild(true)}
                disabled={deleting || rebuildCleanableBytes === 0}
              >
                {rebuildCleanableBytes > 0 ? `Clean Rebuild (${formatSize(rebuildCleanableBytes)})` : 'Nothing to clean now'}
              </button>
            )
          )}

          {result.safe_with_reinstall_bytes > 0 && (
            confirmReinstall ? (
              <div className="confirm-strip tier-reinstall">
                <span className="confirm-text">Restore with npm/yarn install</span>
                <button className="confirm-yes" onClick={handleCleanReinstall} disabled={deleting}>Delete</button>
                <button className="confirm-no" onClick={() => setConfirmReinstall(false)}>Cancel</button>
              </div>
            ) : (
              <button
                className="clean-tier-btn tier-reinstall"
                onClick={() => setConfirmReinstall(true)}
                disabled={deleting || reinstallCleanableBytes === 0}
              >
                {reinstallCleanableBytes > 0 ? `Clean Reinstall (${formatSize(reinstallCleanableBytes)})` : 'Nothing to clean now'}
              </button>
            )
          )}

          {deleteMessage && (
            <div className={`delete-message${deletedToTrash ? ' trashed' : ''}`}>
              {deletedToTrash ? (
                <svg className="result-icon trash-icon" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
                  <path d="M2.5 4.5h11M6 4.5V3a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1v1.5M4 4.5l.5 8.5a1 1 0 0 0 1 1h5a1 1 0 0 0 1-1l.5-8.5" />
                </svg>
              ) : (
                <span className="result-icon">&#10003;</span>
              )}
              <span>{deleteMessage}</span>
            </div>
          )}

          {trashInfo && !purging && (
            <div className="ss-trash-banner">
              <div className="ss-trash-text">
                <span className="ss-trash-size">{trashInfo.total_display}</span> in Trash ({trashInfo.count} {trashInfo.count === 1 ? 'item' : 'items'}) — recoverable
              </div>
              {confirmPurge ? (
                <div className="confirm-strip tier-purge">
                  <span className="confirm-text">
                    Permanently delete {trashInfo.count} {trashInfo.count === 1 ? 'item' : 'items'} ({trashInfo.total_display}) from Trash? This cannot be undone.
                  </span>
                  <button className="confirm-yes" onClick={handlePurgeSsTrash}>Empty</button>
                  <button className="confirm-no" onClick={() => setConfirmPurge(false)}>Cancel</button>
                </div>
              ) : (
                <button className="ss-trash-purge-btn" onClick={() => setConfirmPurge(true)}>
                  Empty SymbolSweep Trash
                </button>
              )}
            </div>
          )}

          {purging && (
            <div className="delete-message">
              <span>
                {purgeProgress
                  ? `Emptying Trash\u2026 ${purgeProgress.current} of ${purgeProgress.total} items${purgeProgress.bytes_freed_so_far !== '0 B' ? ` (${purgeProgress.bytes_freed_so_far} freed)` : ''}`
                  : 'Emptying Trash\u2026'}
              </span>
            </div>
          )}

          </div>

          {/* Scrolls: skipped list and every artifact row */}
          <div className="scan-list">
          {legend}
          {skipped.length > 0 && (
            <div className="skipped-list">
              <div className="skipped-header">
                <span>Skipped &mdash; left in place ({skipped.length})</span>
                <button className="skipped-dismiss" onClick={() => setSkipped([])} aria-label="Dismiss skipped list">
                  Dismiss
                </button>
              </div>
              {skipped.map(item => (
                <SkippedRow key={item.path} item={item} artifacts={skippedContext} />
              ))}
            </div>
          )}

          <div className="artifacts-list">
            {visibleArtifacts.map((artifact, i) => (
              <ArtifactRow
                key={i}
                artifact={artifact}
                onDelete={handleDeleteOne}
                deleting={deleting}
              />
            ))}
            {visibleArtifacts.length === 0 && (
              <div className="no-artifacts">No dev artifacts found</div>
            )}
          </div>
          </div>

        </div>
      )}

      {!result && !scanning && !error && (
        <div className="scan-empty">
          <p>No scan results yet</p>
          <button className="scan-btn" onClick={() => scan()}>
            Scan Now
          </button>
        </div>
      )}
    </div>
  );
}
