import { useState, useRef, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getVersion } from '@tauri-apps/api/app';
import { isPermissionGranted, requestPermission, sendNotification } from '@tauri-apps/plugin-notification';
import { openUrl } from '@tauri-apps/plugin-opener';
import { useSettings } from '../hooks/useSettings';
import { useLicense, CHECKOUT_URL } from '../license';
import { DEBUG_SIZES } from '../types';
import './SettingsPanel.css';

interface SettingsPanelProps {
  onBack: () => void;
  onDeactivated?: () => void;
  /** Open the activation screen (free scan mode) */
  onEnterKey?: () => void;
}

export function SettingsPanel({ onBack, onDeactivated, onEnterKey }: SettingsPanelProps) {
  const { licensed } = useLicense();
  const { settings, loading, saving, updateSetting, refresh: refreshSettings } = useSettings();
  const [debugUnlocked, setDebugUnlocked] = useState(false);
  const [tapCount, setTapCount] = useState(0);
  const tapTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [appVersion, setAppVersion] = useState('');
  const [buildSha, setBuildSha] = useState('');
  const [updateStatus, setUpdateStatus] = useState<'idle' | 'checking' | 'up_to_date' | 'installed' | 'restarting' | 'restart_stalled' | 'error'>('idle');
  const [copied, setCopied] = useState(false);
  const copiedTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [deactivating, setDeactivating] = useState(false);
  const [deactivateConfirm, setDeactivateConfirm] = useState(false);
  const [deactivateError, setDeactivateError] = useState<string | null>(null);
  const [keyCopied, setKeyCopied] = useState(false);
  const keyCopiedTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // The licensed view keeps showing the key while it fades out after Deactivate
  const lastKeyRef = useRef<string | undefined>(undefined);
  if (settings.license_key) lastKeyRef.current = settings.license_key;
  const shownKey = settings.license_key ?? lastKeyRef.current;

  const handleDeactivate = async () => {
    setDeactivating(true);
    setDeactivateError(null);
    try {
      // "Deactivating…" stays up long enough to read, even on a fast network
      await Promise.all([invoke('deactivate_license'), new Promise(r => setTimeout(r, 600))]);
      // update_settings writes the whole object back: reload so a later toggle
      // can't restore the key this Mac just gave up
      await refreshSettings();
      onDeactivated?.();
      // Reset once the licensed view has faded out
      setTimeout(() => setDeactivateConfirm(false), 300);
    } catch (e) {
      setDeactivateError(String(e));
    } finally {
      setDeactivating(false);
    }
  };

  useEffect(() => {
    getVersion().then(setAppVersion);
    invoke<string>('get_build_sha').then(setBuildSha).catch(() => {});
  }, []);

  // app.restart() never returns when it works; still here after 3s means it didn't
  const handleRestart = () => {
    setUpdateStatus('restarting');
    invoke('restart_app').catch(() => {});
    setTimeout(() => setUpdateStatus('restart_stalled'), 3000);
  };

  const handleCheckUpdate = async () => {
    setUpdateStatus('checking');
    try {
      const result = await invoke<string>('check_for_update');
      if (result === 'up_to_date') {
        setUpdateStatus('up_to_date');
        setTimeout(() => setUpdateStatus('idle'), 3000);
      } else {
        setUpdateStatus('installed');
      }
    } catch {
      setUpdateStatus('error');
      setTimeout(() => setUpdateStatus('idle'), 5000);
    }
  };

  const handleFooterClick = useCallback(() => {
    // Copy build string to clipboard
    const buildString = `SymbolSweep ${appVersion}${buildSha ? ` (${buildSha})` : ''}`;
    navigator.clipboard.writeText(buildString).then(() => {
      if (copiedTimeoutRef.current) clearTimeout(copiedTimeoutRef.current);
      setCopied(true);
      copiedTimeoutRef.current = setTimeout(() => setCopied(false), 1500);
    }).catch(() => {});

    // Debug unlock: 5 rapid taps
    if (debugUnlocked) return;

    if (tapTimeoutRef.current) {
      clearTimeout(tapTimeoutRef.current);
    }

    const newCount = tapCount + 1;
    setTapCount(newCount);

    if (newCount >= 5) {
      setDebugUnlocked(true);
      setTapCount(0);
    } else {
      tapTimeoutRef.current = setTimeout(() => {
        setTapCount(0);
      }, 2000);
    }
  }, [appVersion, buildSha, debugUnlocked, tapCount]);

  if (loading) {
    return (
      <div className="settings-panel">
        <div className="settings-loading">Loading settings...</div>
      </div>
    );
  }

  return (
    <div className="settings-panel">
      <header className="settings-header">
        <button className="back-btn" onClick={onBack}>
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M15 18l-6-6 6-6" />
          </svg>
        </button>
        <h1>Settings</h1>
        <div className="header-spacer" />
      </header>

      <div className="settings-content">
        <section className="settings-section">
          <h2>Auto-Clean</h2>
          <p className="section-description">
            Keeps the macOS symbolication cache (coresymbolicationd) from growing
            out of control. Doesn't touch package caches or build artifacts — use Clean Now for those.
          </p>

          <div className="setting-row">
            <div className="setting-info">
              <label htmlFor="auto-threshold">Auto-clear symbolication cache</label>
              <span className="setting-description">
                {!licensed && <span className="setting-licensed-only">Included with a license. </span>}
                Clears automatically when it grows past{' '}
                <select
                  className="inline-select"
                  value={settings.auto_clean_threshold}
                  onChange={(e) => updateSetting('auto_clean_threshold', Number(e.target.value))}
                  disabled={saving || !licensed || !settings.auto_clean_on_threshold}
                >
                  <option value={1 * 1024 * 1024 * 1024}>1 GB</option>
                  <option value={2 * 1024 * 1024 * 1024}>2 GB</option>
                  <option value={3 * 1024 * 1024 * 1024}>3 GB</option>
                  <option value={5 * 1024 * 1024 * 1024}>5 GB</option>
                </select>
              </span>
            </div>
            <label className="toggle">
              <input
                type="checkbox"
                id="auto-threshold"
                checked={licensed && settings.auto_clean_on_threshold}
                onChange={(e) => updateSetting('auto_clean_on_threshold', e.target.checked)}
                disabled={saving || !licensed}
              />
              <span className="toggle-slider" />
            </label>
          </div>
        </section>

        <section className="settings-section">
          <h2>Notifications</h2>

          <div className="setting-row">
            <div className="setting-info">
              <label htmlFor="notifications">Show notifications</label>
              <span className="setting-description">
                Alert when reclaimable space builds up
              </span>
            </div>
            <label className="toggle">
              <input
                type="checkbox"
                id="notifications"
                checked={settings.show_notifications}
                onChange={async (e) => {
                  const enabled = e.target.checked;
                  updateSetting('show_notifications', enabled);

                  if (enabled) {
                    // Request permission and send test notification
                    try {
                      let permissionGranted = await isPermissionGranted();
                      if (!permissionGranted) {
                        const permission = await requestPermission();
                        permissionGranted = permission === 'granted';
                      }
                      if (permissionGranted) {
                        sendNotification({
                          title: 'SymbolSweep',
                          body: 'Notifications enabled!'
                        });
                      }
                    } catch (e) {
                      console.error('Notification setup error:', e);
                    }
                  }
                }}
                disabled={saving}
              />
              <span className="toggle-slider" />
            </label>
          </div>

          {settings.show_notifications && (
            <div className="setting-row nested notification-hint">
              <span className="hint-text">
                Enable notifications in macOS Settings
              </span>
              <button
                className="hint-btn"
                onClick={() => {
                  invoke('open_notification_settings');
                }}
              >
                Open Settings
              </button>
            </div>
          )}
        </section>

        <section className="settings-section">
          <h2>System</h2>

          <div className="setting-row">
            <div className="setting-info">
              <label htmlFor="launch-login">Launch at login</label>
              <span className="setting-description">
                Runs quietly in your menu bar after login
              </span>
            </div>
            <label className="toggle">
              <input
                type="checkbox"
                id="launch-login"
                checked={settings.launch_at_login}
                onChange={(e) => updateSetting('launch_at_login', e.target.checked)}
                disabled={saving}
              />
              <span className="toggle-slider" />
            </label>
          </div>

          <div className="setting-row">
            <div className="setting-info">
              <label htmlFor="monitor-interval">Background refresh</label>
              <span className="setting-description">
                How often to check your cache
              </span>
            </div>
            <select
              id="monitor-interval"
              value={settings.monitor_interval_secs}
              onChange={(e) => updateSetting('monitor_interval_secs', Number(e.target.value))}
              disabled={saving}
            >
              <option value={30}>30 seconds</option>
              <option value={60}>1 minute</option>
              <option value={300}>5 minutes</option>
              <option value={600}>10 minutes</option>
            </select>
          </div>

          <div className="setting-row">
            <div className="setting-info">
              <label>Updates</label>
              <span className="setting-description">
                {updateStatus === 'installed' || updateStatus === 'restarting' ? 'Restart to apply update' :
                 updateStatus === 'restart_stalled' ? 'Quit and reopen SymbolSweep to finish updating.' :
                 `v${appVersion}${buildSha ? ` (${buildSha})` : ''}`}
              </span>
            </div>
            <button
              className="update-check-btn"
              onClick={updateStatus === 'installed' || updateStatus === 'restart_stalled' ? handleRestart : handleCheckUpdate}
              disabled={updateStatus === 'checking' || updateStatus === 'restarting'}
            >
              {updateStatus === 'checking' ? 'Checking...' :
               updateStatus === 'up_to_date' ? 'Up to date' :
               updateStatus === 'installed' || updateStatus === 'restart_stalled' ? 'Restart' :
               updateStatus === 'restarting' ? 'Restarting\u2026' :
               updateStatus === 'error' ? 'Couldn\'t check' :
               'Check'}
            </button>
          </div>
        </section>

        <section className="settings-section">
          <h2>License</h2>

          {/* Both views share one grid cell: the section keeps the taller
              one's height, so Deactivate cross-fades with no layout jump */}
          <div className="license-stack">
            <div className={`license-view${licensed ? ' shown' : ''}`} inert={!licensed} aria-hidden={!licensed}>
              {shownKey && (
                <div
                  className="license-key-display"
                  onClick={() => {
                    navigator.clipboard.writeText(shownKey).then(() => {
                      if (keyCopiedTimeoutRef.current) clearTimeout(keyCopiedTimeoutRef.current);
                      setKeyCopied(true);
                      keyCopiedTimeoutRef.current = setTimeout(() => setKeyCopied(false), 1500);
                    }).catch(() => {});
                  }}
                  title="Click to copy license key"
                >
                  <span className="license-status">Active on this Mac</span>
                  <span className="license-key-value">{shownKey}</span>
                  {keyCopied && <span className="license-copied">Copied</span>}
                </div>
              )}

              <div className="license-stack">
                <div className={`license-view${!deactivateConfirm ? ' shown' : ''}`} inert={deactivateConfirm} aria-hidden={deactivateConfirm}>
                  <div className="setting-row">
                    <div className="setting-info">
                      <label>Deactivate this machine</label>
                      <span className="setting-description">
                        Frees a slot so you can activate on another Mac
                      </span>
                    </div>
                    <button
                      className="update-check-btn deactivate-btn"
                      onClick={() => setDeactivateConfirm(true)}
                    >
                      Deactivate
                    </button>
                  </div>
                </div>

                <div className={`license-view deactivate-confirm${deactivateConfirm ? ' shown' : ''}`} inert={!deactivateConfirm} aria-hidden={!deactivateConfirm}>
                  {deactivateError ? (
                    <p className="deactivate-error">{deactivateError}</p>
                  ) : (
                    <p className="deactivate-warning">
                      <strong>Deactivate this Mac?</strong> You can reactivate with your key.
                    </p>
                  )}
                  <div className="deactivate-actions">
                    <button
                      className="update-check-btn"
                      onClick={() => {
                        setDeactivateConfirm(false);
                        setDeactivateError(null);
                      }}
                      disabled={deactivating}
                    >
                      Cancel
                    </button>
                    <button
                      className="update-check-btn deactivate-btn"
                      onClick={handleDeactivate}
                      disabled={deactivating}
                    >
                      {deactivating ? 'Deactivating\u2026' : 'Deactivate'}
                    </button>
                  </div>
                </div>
              </div>
            </div>

            <div className={`license-view${!licensed ? ' shown' : ''}`} inert={licensed} aria-hidden={licensed}>
              <div className="setting-row">
                <div className="setting-info">
                  <label>Free scan mode</label>
                  <span className="setting-description">
                    Scanning is free. Cleaning and automatic cache cleaning need a license.
                  </span>
                </div>
              </div>
              <button className="license-unlock-btn" onClick={() => openUrl(CHECKOUT_URL)}>
                Unlock cleaning, $12
              </button>
              <button className="license-key-link" onClick={onEnterKey}>
                I have a key
              </button>
            </div>
          </div>
        </section>

        {debugUnlocked && (
          <section className="settings-section debug-section">
            <h2>Debug</h2>

            <div className="setting-row">
              <div className="setting-info">
                <label htmlFor="debug-mode">Debug mode</label>
                <span className="setting-description">
                  Simulate cache sizes to test UI states
                </span>
              </div>
              <label className="toggle">
                <input
                  type="checkbox"
                  id="debug-mode"
                  checked={settings.debug_mode}
                  onChange={(e) => updateSetting('debug_mode', e.target.checked)}
                  disabled={saving}
                />
                <span className="toggle-slider" />
              </label>
            </div>

            {settings.debug_mode && (
              <div className="debug-sizes">
                <p className="debug-label">Simulated cache size:</p>
                <div className="debug-buttons">
                  <button
                    className={`debug-btn ${settings.debug_simulated_size === DEBUG_SIZES.EMPTY ? 'active' : ''}`}
                    onClick={() => updateSetting('debug_simulated_size', DEBUG_SIZES.EMPTY)}
                    disabled={saving}
                  >
                    0 B
                  </button>
                  <button
                    className={`debug-btn ${settings.debug_simulated_size === DEBUG_SIZES.SMALL ? 'active' : ''}`}
                    onClick={() => updateSetting('debug_simulated_size', DEBUG_SIZES.SMALL)}
                    disabled={saving}
                  >
                    3GB
                  </button>
                  <button
                    className={`debug-btn moderate ${settings.debug_simulated_size === DEBUG_SIZES.MODERATE ? 'active' : ''}`}
                    onClick={() => updateSetting('debug_simulated_size', DEBUG_SIZES.MODERATE)}
                    disabled={saving}
                  >
                    7GB
                  </button>
                  <button
                    className={`debug-btn heavy ${settings.debug_simulated_size === DEBUG_SIZES.HEAVY ? 'active' : ''}`}
                    onClick={() => updateSetting('debug_simulated_size', DEBUG_SIZES.HEAVY)}
                    disabled={saving}
                  >
                    15GB
                  </button>
                </div>
                <button
                  className="debug-btn test-notification"
                  onClick={async () => {
                    console.log('Sending test notification via backend...');
                    try {
                      await invoke('test_notification');
                      console.log('Test notification command sent');
                    } catch (e) {
                      console.error('Notification error:', e);
                      alert('Failed to send notification. Check console for details.');
                    }
                  }}
                  disabled={saving}
                >
                  Test Notification
                </button>
              </div>
            )}
          </section>
        )}

        </div>

      <footer className="settings-footer" onClick={handleFooterClick} title="Click to copy build info">
        <div className="footer-logo">
          <div className="footer-logo-icon" aria-hidden="true" />
          <span className="footer-logo-text"><span className="logo-sym">Symbol</span>Sweep</span>
        </div>
        {copied
          ? <span className="footer-copied">Copied</span>
          : <span className="footer-build">{appVersion}</span>
        }
      </footer>
    </div>
  );
}
