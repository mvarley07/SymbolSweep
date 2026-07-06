import { useDevScan } from '../hooks/useCacheStatus';
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

function ArtifactRow({ artifact }: { artifact: DevArtifact }) {
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
      <div className="artifact-path" title={artifact.path}>
        {artifact.path.replace(/^\/Users\/[^/]+/, '~')}
      </div>
    </div>
  );
}

export function DevScanPanel({ onBack }: DevScanPanelProps) {
  const { result, scanning, error, scan } = useDevScan();

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

          <div className="artifacts-list">
            {result.artifacts.map((artifact, i) => (
              <ArtifactRow key={i} artifact={artifact} />
            ))}
            {result.artifacts.length === 0 && (
              <div className="no-artifacts">No dev artifacts found</div>
            )}
          </div>

          <div className="scan-meta">
            Scanned in {result.scan_duration_ms}ms
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
