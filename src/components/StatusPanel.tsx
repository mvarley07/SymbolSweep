import { useState, useEffect } from 'react';
import { useCacheStatus, useCleanCache, useLastCleanTime, useDevScan, useDeleteDevArtifacts } from '../hooks/useCacheStatus';
import { useSettings } from '../hooks/useSettings';
import { CleanConfirmation } from './CleanConfirmation';
import type { CacheState, CleanResult } from '../types';
import { WARNING_THRESHOLD, CRITICAL_THRESHOLD } from '../types';
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

/** Determine cache state from combined byte total */
function stateFromSize(bytes: number): CacheState {
  if (bytes >= CRITICAL_THRESHOLD) return 'Critical';
  if (bytes >= WARNING_THRESHOLD) return 'Warning';
  return 'Normal';
}

interface StatusIndicatorProps {
  state: CacheState;
  size: string;
}

function StatusIndicator({ state, size }: StatusIndicatorProps) {
  const stateConfig = {
    Normal: { label: 'Healthy' },
    Warning: { label: 'Warning' },
    Critical: { label: 'Critical' },
  };

  const config = stateConfig[state];
  const stateClass = state.toLowerCase();

  return (
    <div className="status-indicator">
      <div className={`status-size ${stateClass}`}>
        {size}
      </div>
      <div className={`status-state ${stateClass}`}>
        <span className="status-dot" />
        <span className="status-label">{config.label}</span>
      </div>
    </div>
  );
}

interface StatusPanelProps {
  onSettingsClick: () => void;
  onDevScanClick: () => void;
}

export function StatusPanel({ onSettingsClick, onDevScanClick }: StatusPanelProps) {
  const { status, loading, error, refresh } = useCacheStatus();
  const { clean, dryRun, cleaning } = useCleanCache();
  const { lastCleanTime, refresh: refreshLastClean } = useLastCleanTime();
  const { settings, updateSetting } = useSettings();
  const { result: devResult } = useDevScan();
  const { deleteArtifacts } = useDeleteDevArtifacts();

  const [showConfirmation, setShowConfirmation] = useState(false);
  const [dryRunResult, setDryRunResult] = useState<CleanResult | null>(null);
  const [bannerFading, setBannerFading] = useState(false);
  const [showBanner, setShowBanner] = useState(false);
  const [isLoading, setIsLoading] = useState(false);
  const [bannerMessage, setBannerMessage] = useState<string | null>(null);

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
    try {
      // Clean system cache
      const sysResult = await clean(false);
      let totalFreed = sysResult.bytes_freed;

      // Also clean all dev artifacts
      if (devResult) {
        const cleanablePaths = devResult.artifacts
          .filter(a => !a.is_nested)
          .map(a => a.path);
        if (cleanablePaths.length > 0) {
          const devDeleteResult = await deleteArtifacts(cleanablePaths);
          totalFreed += devDeleteResult.bytes_freed;
        }
      }

      refresh();
      await refreshLastClean();

      // Show combined result banner
      if (totalFreed > 0) {
        showCleanBanner(`Freed ${formatSize(totalFreed)}`);
      } else {
        showCleanBanner('Already clean');
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

  const devBytes = devResult?.total_bytes ?? 0;
  const combinedBytes = status.size_bytes + devBytes;
  const combinedState = stateFromSize(combinedBytes);
  const combinedDisplay = formatSize(combinedBytes);
  const stateClass = combinedState.toLowerCase();

  return (
    <div className="status-panel">
      <header className="panel-header">
        <div className="header-logo">
          <div className="logo-icon" aria-hidden="true" />
          <span className="logo-text">SymbolSweep</span>
        </div>
        <button className="settings-btn" onClick={onSettingsClick} title="Settings">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
        </button>
      </header>

      <div className="status-content">
        <StatusIndicator state={combinedState} size={combinedDisplay} />

        <div className="status-details">
          <div className="detail-row">
            <span className="detail-label">Items</span>
            <span className="detail-value">{(status.file_count + (devResult?.artifacts.filter(a => !a.is_nested).length ?? 0)).toLocaleString()}</span>
          </div>
          <div className="detail-row">
            <span className="detail-label">Last cleaned</span>
            <span className="detail-value">{lastCleanTime}</span>
          </div>
        </div>

        {showBanner && bannerMessage && (
          <div className={`clean-result${bannerFading ? ' fading-out' : ''}`}>
            <span className="result-icon">✓</span>
            <span>{bannerMessage}</span>
          </div>
        )}

        <button
          className={`clean-btn ${stateClass}${isLoading ? ' loading' : ''}`}
          onClick={handleCleanClick}
          disabled={isLoading || cleaning || !status.exists}
        >
          {isLoading ? (
            <span className="loading-text">
              Cleaning<span className="loading-dots"><span>.</span><span>.</span><span>.</span></span>
            </span>
          ) : (
            'Clean Now'
          )}
        </button>

        {combinedState === 'Warning' && (
          <p className="warning-text">Cache getting large -- consider cleaning</p>
        )}

        {combinedState === 'Critical' && (
          <p className="critical-text">Cache critically large -- clean now!</p>
        )}

        {devResult && devResult.total_bytes > 0 && (
          <button className="dev-scan-link" onClick={onDevScanClick}>
            <span className="dev-scan-total">{devResult.total_display} dev artifacts</span>
            <span className="dev-scan-arrow">&rsaquo;</span>
          </button>
        )}
      </div>
    </div>
  );
}
