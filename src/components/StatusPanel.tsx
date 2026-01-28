import { useState, useEffect, useMemo } from 'react';
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
  const [bannerFading, setBannerFading] = useState(false);
  const [showBanner, setShowBanner] = useState(false);
  const [isDarkMode, setIsDarkMode] = useState(() =>
    window.matchMedia('(prefers-color-scheme: dark)').matches
  );

  // Listen for system theme changes
  useEffect(() => {
    const mediaQuery = window.matchMedia('(prefers-color-scheme: dark)');
    const handler = (e: MediaQueryListEvent) => setIsDarkMode(e.matches);
    mediaQuery.addEventListener('change', handler);
    return () => mediaQuery.removeEventListener('change', handler);
  }, []);

  const logoSrc = isDarkMode ? '/logo-dark.png' : '/logo-light.png';

  // Auto-dismiss success banner after 5 seconds (only if something was cleaned)
  useEffect(() => {
    if (cleanResult && !cleanResult.was_dry_run && cleanResult.files_cleaned > 0) {
      setShowBanner(true);
      setBannerFading(false);

      const fadeTimer = setTimeout(() => {
        setBannerFading(true);
      }, 4700);

      const removeTimer = setTimeout(() => {
        setShowBanner(false);
        setBannerFading(false);
      }, 5000);

      return () => {
        clearTimeout(fadeTimer);
        clearTimeout(removeTimer);
      };
    }
  }, [cleanResult]);

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
      // Add minimum delay so spinner is visible even for fast operations
      await Promise.all([
        clean(false),
        new Promise(resolve => setTimeout(resolve, 500))
      ]);
      refresh();
      await refreshLastClean();
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
        <div className="header-logo">
          <img src={logoSrc} alt="SymbolSweep" className="logo-icon" />
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
        <StatusIndicator state={status.state} size={status.size_display} />

        <div className="status-details">
          <div className="detail-row">
            <span className="detail-label">Files</span>
            <span className="detail-value">{status.file_count.toLocaleString()}</span>
          </div>
          <div className="detail-row">
            <span className="detail-label">Last cleaned</span>
            <span className="detail-value">
              {cleaning ? (
                <span className="cleaning-indicator">
                  <svg className="mini-spinner" viewBox="0 0 24 24" fill="none">
                    <circle cx="12" cy="12" r="10" stroke="currentColor" strokeOpacity="0.3" strokeWidth="3"/>
                    <path d="M12 2a10 10 0 0 1 10 10" stroke="currentColor" strokeWidth="3" strokeLinecap="round"/>
                  </svg>
                  Cleaning...
                </span>
              ) : (
                lastCleanTime
              )}
            </span>
          </div>
        </div>

        {showBanner && cleanResult && !cleanResult.was_dry_run && cleanResult.files_cleaned > 0 && (
          <div className={`clean-result${bannerFading ? ' fading-out' : ''}`}>
            <span className="result-icon">✓</span>
            <span>{cleanResult.message}</span>
          </div>
        )}

        <button
          className={`clean-btn ${stateClass}`}
          onClick={handleCleanClick}
          disabled={cleaning || !status.exists}
        >
          {cleaning ? (
            <>
              <svg className="btn-spinner" viewBox="0 0 24 24" fill="none">
                <circle cx="12" cy="12" r="10" stroke="currentColor" strokeOpacity="0.3" strokeWidth="3"/>
                <path d="M12 2a10 10 0 0 1 10 10" stroke="currentColor" strokeWidth="3" strokeLinecap="round"/>
              </svg>
              Cleaning...
            </>
          ) : (
            'Clean Now'
          )}
        </button>

        {status.state === 'Warning' && (
          <p className="warning-text">Cache is getting large. Consider cleaning soon.</p>
        )}

        {status.state === 'Critical' && (
          <p className="critical-text">Cache is critically large! Clean immediately to prevent issues.</p>
        )}
      </div>
    </div>
  );
}
