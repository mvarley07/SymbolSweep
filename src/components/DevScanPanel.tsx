import { useState } from 'react';
import { useDevScan, useDeleteDevArtifacts, useDeleteDevArtifactsManual } from '../hooks/useCacheStatus';
import type { ArtifactTier, DevArtifact } from '../types';
import './DevScanPanel.css';

const LEGEND_SEEN_KEY = 'symbolsweep:tier-legend-seen';

interface DevScanPanelProps {
  onBack: () => void;
}

const TIER_CONFIG: Record<ArtifactTier, { label: string; desc: string; className: string }> = {
  Safe: { label: 'SAFE', desc: 'Caches \u2014 regenerate automatically', className: 'tier-safe' },
  Rebuildable: { label: 'REBUILD', desc: 'Build artifacts \u2014 slow to rebuild', className: 'tier-rebuild' },
  SafeWithReinstall: { label: 'REINSTALL', desc: 'npm install to restore', className: 'tier-reinstall' },
  Ask: { label: 'REVIEW', desc: 'May contain shipped output', className: 'tier-ask' },
};

interface ArtifactRowProps {
  artifact: DevArtifact;
  onDelete: (path: string) => void;
  deleting: boolean;
}

const STALE_THRESHOLD_DAYS = 14;

/** Generate the removal command/instruction for REVIEW-tier artifacts */
function getRemovalInfo(artifact: DevArtifact): { command: string; note?: string } | { instruction: string } | null {
  const shortPath = artifact.path.replace(/^\/Users\/[^/]+/, '~');
  switch (artifact.kind) {
    case 'Docker':
      return { command: 'docker system prune -a --volumes', note: 'Deletes volumes \u2014 may include databases' };
    case 'Xcode Archives':
      return { instruction: 'In Xcode: Window \u2192 Organizer \u2192 delete archives you no longer need' };
    case 'iOS Simulators':
      return { instruction: 'In Xcode: Settings \u2192 Platforms \u2192 delete unused simulators' };
    case 'Android emulator images':
      return { instruction: 'In Android Studio: Device Manager \u2192 delete unused AVDs' };
    default:
      if (artifact.kind.endsWith(' output')) {
        return { command: `rm -rf ${shortPath}` };
      }
      return null;
  }
}

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // clipboard API may fail in some contexts
    }
  };

  return (
    <button className="copy-btn" onClick={handleCopy} title="Copy command">
      {copied ? (
        <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
          <path d="M3 8.5l3 3 7-7" />
        </svg>
      ) : (
        <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
          <rect x="5" y="5" width="8" height="8" rx="1" />
          <path d="M3 11V3a1 1 0 0 1 1-1h8" />
        </svg>
      )}
    </button>
  );
}

function ArtifactRow({ artifact, onDelete, deleting }: ArtifactRowProps) {
  const config = TIER_CONFIG[artifact.tier];

  // Only show staleness for genuinely unused artifacts (14+ days)
  const staleness = artifact.staleness_days != null && artifact.staleness_days >= STALE_THRESHOLD_DAYS
    ? `${artifact.staleness_days}d unused`
    : null;

  // Delete button for SAFE/REBUILD/REINSTALL, not nested, not active builds
  const showDelete = !artifact.is_nested && artifact.tier !== 'Ask' && !artifact.active_build;

  // REVIEW-tier: show removal command/instruction instead of delete
  const isReview = artifact.tier === 'Ask';
  const removalInfo = isReview ? getRemovalInfo(artifact) : null;

  return (
    <div className={`artifact-row ${artifact.is_nested ? 'nested' : ''} ${artifact.active_build ? 'active-build' : ''} ${isReview ? 'tier-ask-row' : ''}`}>
      <div className="artifact-body">
        <div className="artifact-text">
          <div className="artifact-main">
            <span className={`artifact-tier-badge ${config.className}`}>
              {artifact.active_build ? 'BUILDING' : config.label}
            </span>
          </div>
          <div className="artifact-details">
            <span className="artifact-kind">{artifact.kind}</span>
            {artifact.project && (
              <span className="artifact-project">{artifact.project}</span>
            )}
            {staleness && (
              <span className="artifact-staleness">{staleness}</span>
            )}
          </div>
          {artifact.hint && (
            <div className="artifact-hint">{artifact.hint}</div>
          )}
          {removalInfo && 'command' in removalInfo && (
            <div className="removal-command">
              <code>{removalInfo.command}</code>
              <CopyButton text={removalInfo.command} />
              {removalInfo.note && <span className="removal-note">{removalInfo.note}</span>}
            </div>
          )}
          {removalInfo && 'instruction' in removalInfo && (
            <div className="removal-instruction">{removalInfo.instruction}</div>
          )}
          <div className="artifact-path" title={artifact.path}>
            {artifact.path.replace(/^\/Users\/[^/]+/, '~')}
          </div>
        </div>
        <div className="artifact-actions">
          <span className="artifact-size">{artifact.size_display}</span>
          {showDelete ? (
            <button
              className="artifact-delete-btn"
              onClick={() => onDelete(artifact.path)}
              disabled={deleting}
              title="Move to Trash"
            >
              <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
                <path d="M2.5 4.5h11M6 4.5V3a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1v1.5M4 4.5l.5 8.5a1 1 0 0 0 1 1h5a1 1 0 0 0 1-1l.5-8.5" />
              </svg>
            </button>
          ) : (
            <span className="artifact-delete-spacer" />
          )}
        </div>
      </div>
    </div>
  );
}

export function DevScanPanel({ onBack }: DevScanPanelProps) {
  const { result, scanning, error, scan } = useDevScan();
  const { deleteArtifacts: bulkDeleteArtifacts, deleting: bulkDeleting } = useDeleteDevArtifacts();
  const { deleteArtifacts: manualDeleteArtifacts, deleting: manualDeleting } = useDeleteDevArtifactsManual();
  const deleting = bulkDeleting || manualDeleting;
  const [deleteMessage, setDeleteMessage] = useState<string | null>(null);
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

  const showResult = (msg: string) => {
    setDeleteMessage(msg);
    setTimeout(() => setDeleteMessage(null), 4000);
  };

  const handleDeleteOne = async (path: string) => {
    try {
      const res = await manualDeleteArtifacts([path]);
      if (res.deleted_count > 0) showResult(`Freed ${res.bytes_freed_display}`);
      if (res.errors.length > 0) showResult(`Error: ${res.errors[0]}`);
    } catch {
      // error state handled by hook
    }
  };

  const handleCleanSafe = async () => {
    if (!result) return;
    const paths = result.artifacts
      .filter(a => a.tier === 'Safe' && !a.is_nested && !a.active_build)
      .map(a => a.path);
    if (paths.length === 0) return;
    try {
      const res = await bulkDeleteArtifacts(paths);
      if (res.deleted_count > 0) showResult(`Freed ${res.bytes_freed_display} (${res.deleted_count} items)`);
    } catch {
      // error state handled by hook
    }
  };

  const handleCleanRebuild = async () => {
    if (!result) return;
    setConfirmRebuild(false);
    const paths = result.artifacts
      .filter(a => a.tier === 'Rebuildable' && !a.is_nested && !a.active_build)
      .map(a => a.path);
    if (paths.length === 0) return;
    try {
      const res = await manualDeleteArtifacts(paths);
      if (res.deleted_count > 0) showResult(`Freed ${res.bytes_freed_display} (${res.deleted_count} items)`);
    } catch {
      // error state handled by hook
    }
  };

  const handleCleanReinstall = async () => {
    if (!result) return;
    setConfirmReinstall(false);
    const paths = result.artifacts
      .filter(a => a.tier === 'SafeWithReinstall' && !a.is_nested && !a.active_build)
      .map(a => a.path);
    if (paths.length === 0) return;
    try {
      const res = await manualDeleteArtifacts(paths);
      if (res.deleted_count > 0) showResult(`Freed ${res.bytes_freed_display} (${res.deleted_count} items)`);
    } catch {
      // error state handled by hook
    }
  };

  return (
    <div className="devscan-panel">
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

      {legendExpanded && (
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
      )}

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
          <div className="scan-total">
            <span className="total-label">Total reclaimable</span>
            <span className="total-value">{result.total_display}</span>
          </div>

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
            <div className="tier-row tier-ask">
              <span className="tier-label">REVIEW</span>
              <span className="tier-value">{result.ask_bytes > 0 ? result.ask_display : '0 B'}</span>
            </div>
          </div>

          {result.safe_bytes > 0 && (
            <button
              className="clean-safe-btn"
              onClick={handleCleanSafe}
              disabled={deleting}
            >
              {deleting ? 'Cleaning...' : `Clean Safe (${result.safe_display})`}
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
                disabled={deleting}
              >
                Clean Rebuild ({result.rebuildable_display})
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
                disabled={deleting}
              >
                Clean Reinstall ({result.safe_with_reinstall_display})
              </button>
            )
          )}

          {deleteMessage && (
            <div className="delete-message">
              <span className="result-icon">&#10003;</span>
              <span>{deleteMessage}</span>
            </div>
          )}

          <div className="artifacts-list">
            {result.artifacts.map((artifact, i) => (
              <ArtifactRow
                key={i}
                artifact={artifact}
                onDelete={handleDeleteOne}
                deleting={deleting}
              />
            ))}
            {result.artifacts.length === 0 && (
              <div className="no-artifacts">No dev artifacts found</div>
            )}
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
