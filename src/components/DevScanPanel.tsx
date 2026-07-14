import { useState } from 'react';
import { useDevScan, useDeleteDevArtifacts, useDeleteDevArtifactsManual } from '../hooks/useCacheStatus';
import type { ArtifactTier, DevArtifact } from '../types';
import './DevScanPanel.css';

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

function ArtifactRow({ artifact, onDelete, deleting }: ArtifactRowProps) {
  const config = TIER_CONFIG[artifact.tier];

  // Only show staleness for genuinely unused artifacts (14+ days)
  const staleness = artifact.staleness_days != null && artifact.staleness_days >= STALE_THRESHOLD_DAYS
    ? `${artifact.staleness_days}d unused`
    : null;

  // No delete button for nested items, Ask-tier items, or active builds
  const showDelete = !artifact.is_nested && artifact.tier !== 'Ask' && !artifact.active_build;

  return (
    <div className={`artifact-row ${artifact.is_nested ? 'nested' : ''} ${artifact.active_build ? 'active-build' : ''} ${artifact.tier === 'Ask' ? 'tier-ask-row' : ''}`}>
      <div className="artifact-main">
        <span className={`artifact-tier-badge ${config.className}`}>
          {artifact.active_build ? 'BUILDING' : config.label}
        </span>
        <span className="artifact-size">{artifact.size_display}</span>
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
      <div className="artifact-path" title={artifact.path}>
        {artifact.path.replace(/^\/Users\/[^/]+/, '~')}
      </div>
      {showDelete && (
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
      )}
    </div>
  );
}

export function DevScanPanel({ onBack }: DevScanPanelProps) {
  const { result, scanning, error, scan } = useDevScan();
  const { deleteArtifacts: bulkDeleteArtifacts, deleting: bulkDeleting } = useDeleteDevArtifacts();
  const { deleteArtifacts: manualDeleteArtifacts, deleting: manualDeleting } = useDeleteDevArtifactsManual();
  const deleting = bulkDeleting || manualDeleting;
  const [deleteMessage, setDeleteMessage] = useState<string | null>(null);

  const handleDeleteOne = async (path: string) => {
    try {
      const res = await manualDeleteArtifacts([path]);
      if (res.deleted_count > 0) {
        setDeleteMessage(`Freed ${res.bytes_freed_display}`);
        setTimeout(() => setDeleteMessage(null), 3000);
      }
      if (res.errors.length > 0) {
        setDeleteMessage(`Error: ${res.errors[0]}`);
        setTimeout(() => setDeleteMessage(null), 4000);
      }
    } catch {
      // error state handled by hook
    }
  };

  const handleCleanSafe = async () => {
    if (!result) return;
    const safePaths = result.artifacts
      .filter(a => a.tier === 'Safe' && !a.is_nested && !a.active_build)
      .map(a => a.path);
    if (safePaths.length === 0) return;
    try {
      const res = await bulkDeleteArtifacts(safePaths);
      if (res.deleted_count > 0) {
        setDeleteMessage(`Freed ${res.bytes_freed_display} (${res.deleted_count} items)`);
        setTimeout(() => setDeleteMessage(null), 4000);
      }
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
              <span className="tier-value">{result.safe_bytes > 0 ? result.safe_display : 'None found'}</span>
            </div>
            <div className="tier-row tier-rebuild">
              <span className="tier-label">REBUILD</span>
              <span className="tier-value">{result.rebuildable_bytes > 0 ? result.rebuildable_display : 'None found'}</span>
            </div>
            <div className="tier-row tier-reinstall">
              <span className="tier-label">REINSTALL</span>
              <span className="tier-value">{result.safe_with_reinstall_bytes > 0 ? result.safe_with_reinstall_display : 'None found'}</span>
            </div>
            <div className="tier-row tier-ask">
              <span className="tier-label">REVIEW</span>
              <span className="tier-value">{result.ask_bytes > 0 ? result.ask_display : 'None found'}</span>
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
