import { useState } from 'react';
import { useDevScan, useDeleteDevArtifacts } from '../hooks/useCacheStatus';
import type { ArtifactTier, DevArtifact } from '../types';
import './DevScanPanel.css';

interface DevScanPanelProps {
  onBack: () => void;
}

const TIER_CONFIG: Record<ArtifactTier, { label: string; desc: string; className: string }> = {
  Safe: { label: 'SAFE', desc: 'Caches \u2014 regenerate automatically', className: 'tier-safe' },
  SafeWithReinstall: { label: 'SAFE-WITH-REINSTALL', desc: 'npm install to restore', className: 'tier-reinstall' },
  Ask: { label: 'ASK', desc: 'May be shipping artifacts', className: 'tier-ask' },
};

interface ArtifactRowProps {
  artifact: DevArtifact;
  onDelete: (path: string) => void;
  deleting: boolean;
}

function ArtifactRow({ artifact, onDelete, deleting }: ArtifactRowProps) {
  const config = TIER_CONFIG[artifact.tier];
  const staleness = artifact.staleness_days != null
    ? `${artifact.staleness_days}d stale`
    : null;

  return (
    <div className={`artifact-row ${artifact.is_nested ? 'nested' : ''}`}>
      <div className="artifact-main">
        <span className={`artifact-tier-badge ${config.className}`}>
          {config.label}
        </span>
        <div className="artifact-main-right">
          <span className="artifact-size">{artifact.size_display}</span>
          {!artifact.is_nested && (
            <button
              className="artifact-delete-btn"
              onClick={() => onDelete(artifact.path)}
              disabled={deleting}
              title="Delete"
            >
              ×
            </button>
          )}
        </div>
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
      <div className="artifact-path" title={artifact.path}>
        {artifact.path.replace(/^\/Users\/[^/]+/, '~')}
      </div>
    </div>
  );
}

export function DevScanPanel({ onBack }: DevScanPanelProps) {
  const { result, scanning, error, scan } = useDevScan();
  const { deleteArtifacts, deleting } = useDeleteDevArtifacts();
  const [deleteMessage, setDeleteMessage] = useState<string | null>(null);

  const handleDeleteOne = async (path: string) => {
    try {
      const res = await deleteArtifacts([path]);
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
      .filter(a => a.tier === 'Safe' && !a.is_nested)
      .map(a => a.path);
    if (safePaths.length === 0) return;
    try {
      const res = await deleteArtifacts(safePaths);
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
              <span className="tier-value">{result.safe_display}</span>
            </div>
            <div className="tier-row tier-reinstall">
              <span className="tier-label">REINSTALL</span>
              <span className="tier-value">{result.safe_with_reinstall_display}</span>
            </div>
            <div className="tier-row tier-ask">
              <span className="tier-label">ASK</span>
              <span className="tier-value">{result.ask_display}</span>
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
              <span className="result-icon">✓</span>
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
