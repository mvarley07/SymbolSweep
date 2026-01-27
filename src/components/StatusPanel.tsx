import { useState } from 'react';
import { useCacheStatus, useCleanCache, useLastCleanTime } from '../hooks/useCacheStatus';
import { useSettings } from '../hooks/useSettings';
import { CleanConfirmation } from './CleanConfirmation';
import type { CacheState, CleanResult } from '../types';
import './StatusPanel.css';

interface StatusIndicatorProps {
  state: CacheState;
  size: string;
}

function StatusIndicator({ state, size }: StatusIndicatorProps) {
  const stateConfig = {
    Normal: { label: 'Healthy' },
    Warning: { label: 'Getting Large' },
    Critical: { label: 'Clean Now' },
  };

  const config = stateConfig[state];
  const stateClass = state.toLowerCase();

  return (
    <div className="status-indicator">
      <div className={`status-size ${stateClass}`}>{size}</div>
      <div className={`status-state ${stateClass}`}>
        <span className="status-dot" />
        <span className="status-label">{config.label}</span>
      </div>
    </div>
  );
}

interface StatusPanelProps {
  onSettingsClick: () => void;
}

export function StatusPanel({ onSettingsClick }: StatusPanelProps) {
  const { status, loading, error, refresh } = useCacheStatus();
  const { clean, dryRun, cleaning, result: cleanResult } = useCleanCache();
  const { lastCleanTime, refresh: refreshLastClean } = useLastCleanTime();
  const { settings, updateSetting } = useSettings();

  const [showConfirmation, setShowConfirmation] = useState(false);
  const [dryRunResult, setDryRunResult] = useState<CleanResult | null>(null);

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
    try {
      await clean(false);
      refresh();
      refreshLastClean();
    } catch (err) {
      console.error('Clean failed:', err);
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
      <div className="status-panel">
        <div className="status-loading">Loading...</div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="status-panel">
        <div className="status-error">
          <p>Error: {error}</p>
          <button onClick={refresh}>Retry</button>
        </div>
      </div>
    );
  }

  if (!status) {
    return (
      <div className="status-panel">
        <div className="status-error">No status available</div>
      </div>
    );
  }

  const stateClass = status.state.toLowerCase();

  return (
    <div className="status-panel">
      <header className="panel-header">
        <h1>SymbolSweep</h1>
        <button className="settings-btn" onClick={onSettingsClick} title="Settings">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
        </button>
      </header>

      <div className="status-content">
        <StatusIndicator state={status.state} size={status.size_display} />

        <div className="status-details">
          <div className="detail-row">
            <span className="detail-label">Files</span>
            <span className="detail-value">{status.file_count.toLocaleString()}</span>
          </div>
          <div className="detail-row">
            <span className="detail-label">Last cleaned</span>
            <span className="detail-value">{lastCleanTime}</span>
          </div>
          <div className="detail-row">
            <span className="detail-label">Cache path</span>
            <span className="detail-value path" title={status.path}>
              {status.path.includes('Debug') ? '[Debug Mode]' : '~/Library/Caches/...'}
            </span>
          </div>
        </div>

        {cleanResult && !cleanResult.was_dry_run && (
          <div className="clean-result">
            <span className="result-icon">✓</span>
            <span>{cleanResult.message}</span>
          </div>
        )}

        <button
          className={`clean-btn ${stateClass}`}
          onClick={handleCleanClick}
          disabled={cleaning || !status.exists}
        >
          {cleaning ? 'Cleaning...' : 'Clean Now'}
        </button>

        {status.state === 'Warning' && (
          <p className="warning-text">Cache is getting large. Consider cleaning soon.</p>
        )}

        {status.state === 'Critical' && (
          <p className="critical-text">Cache is critically large! Clean immediately to prevent issues.</p>
        )}
      </div>

      <footer className="panel-footer">
        <button className="quit-btn" onClick={() => window.close()}>
          Quit SymbolSweep
        </button>
      </footer>
    </div>
  );
}
