import { useState, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { isPermissionGranted, requestPermission, sendNotification } from '@tauri-apps/plugin-notification';
import { useSettings } from '../hooks/useSettings';
import { WARNING_THRESHOLD, CRITICAL_THRESHOLD, DEBUG_SIZES } from '../types';
import './SettingsPanel.css';

interface SettingsPanelProps {
  onBack: () => void;
}

function formatBytes(bytes: number): string {
  const gb = bytes / (1024 * 1024 * 1024);
  return `${gb.toFixed(0)}GB`;
}

function formatInterval(secs: number): string {
  const hours = secs / 3600;
  if (hours < 1) {
    return `${Math.round(secs / 60)} minutes`;
  }
  return `${hours} hours`;
}

export function SettingsPanel({ onBack }: SettingsPanelProps) {
  const { settings, loading, saving, updateSetting } = useSettings();
  const [debugUnlocked, setDebugUnlocked] = useState(false);
  const [tapCount, setTapCount] = useState(0);
  const tapTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const handleVersionTap = () => {
    if (debugUnlocked) return;

    // Clear existing timeout
    if (tapTimeoutRef.current) {
      clearTimeout(tapTimeoutRef.current);
    }

    const newCount = tapCount + 1;
    setTapCount(newCount);

    if (newCount >= 5) {
      setDebugUnlocked(true);
      setTapCount(0);
    } else {
      // Reset after 2 seconds of no taps
      tapTimeoutRef.current = setTimeout(() => {
        setTapCount(0);
      }, 2000);
    }
  };

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

          <div className="setting-row">
            <div className="setting-info">
              <label htmlFor="auto-threshold">Clean when cache exceeds threshold</label>
              <span className="setting-description">
                Automatically clean when cache reaches {formatBytes(settings.auto_clean_threshold)}
              </span>
            </div>
            <label className="toggle">
              <input
                type="checkbox"
                id="auto-threshold"
                checked={settings.auto_clean_on_threshold}
                onChange={(e) => updateSetting('auto_clean_on_threshold', e.target.checked)}
                disabled={saving}
              />
              <span className="toggle-slider" />
            </label>
          </div>

          {settings.auto_clean_on_threshold && (
            <div className="setting-row nested">
              <label htmlFor="threshold-select">Threshold</label>
              <select
                id="threshold-select"
                value={settings.auto_clean_threshold}
                onChange={(e) => updateSetting('auto_clean_threshold', Number(e.target.value))}
                disabled={saving}
              >
                <option value={2 * 1024 * 1024 * 1024}>2GB</option>
                <option value={WARNING_THRESHOLD}>5GB (Warning)</option>
                <option value={7 * 1024 * 1024 * 1024}>7GB</option>
                <option value={CRITICAL_THRESHOLD}>10GB (Critical)</option>
              </select>
            </div>
          )}

          <div className="setting-row">
            <div className="setting-info">
              <label htmlFor="auto-scheduled">Scheduled cleaning</label>
              <span className="setting-description">
                Clean every {formatInterval(settings.auto_clean_interval_secs)}
              </span>
            </div>
            <label className="toggle">
              <input
                type="checkbox"
                id="auto-scheduled"
                checked={settings.auto_clean_scheduled}
                onChange={(e) => updateSetting('auto_clean_scheduled', e.target.checked)}
                disabled={saving}
              />
              <span className="toggle-slider" />
            </label>
          </div>

          {settings.auto_clean_scheduled && (
            <div className="setting-row nested">
              <label htmlFor="interval-select">Interval</label>
              <select
                id="interval-select"
                value={settings.auto_clean_interval_secs}
                onChange={(e) => updateSetting('auto_clean_interval_secs', Number(e.target.value))}
                disabled={saving}
              >
                {settings.debug_mode && (
                  <option value={2 * 60}>2 minutes (debug)</option>
                )}
                <option value={1 * 60 * 60}>1 hour</option>
                <option value={3 * 60 * 60}>3 hours</option>
                <option value={6 * 60 * 60}>6 hours</option>
                <option value={12 * 60 * 60}>12 hours</option>
                <option value={24 * 60 * 60}>24 hours</option>
              </select>
            </div>
          )}
        </section>

        <section className="settings-section">
          <h2>Notifications</h2>

          <div className="setting-row">
            <div className="setting-info">
              <label htmlFor="notifications">Show notifications</label>
              <span className="setting-description">
                Alert when cache reaches warning or critical levels
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
                Not seeing notifications? Enable them in macOS Settings.
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
                Start SymbolSweep when you log in
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
                    className={`debug-btn warning ${settings.debug_simulated_size === DEBUG_SIZES.WARNING ? 'active' : ''}`}
                    onClick={() => updateSetting('debug_simulated_size', DEBUG_SIZES.WARNING)}
                    disabled={saving}
                  >
                    7GB
                  </button>
                  <button
                    className={`debug-btn critical ${settings.debug_simulated_size === DEBUG_SIZES.CRITICAL ? 'active' : ''}`}
                    onClick={() => updateSetting('debug_simulated_size', DEBUG_SIZES.CRITICAL)}
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

      <footer className="settings-footer" onClick={handleVersionTap}>
        <div className="footer-logo">
          <div className="footer-logo-icon" aria-hidden="true" />
          <span className="footer-logo-text">SymbolSweep</span>
        </div>
      </footer>
    </div>
  );
}
