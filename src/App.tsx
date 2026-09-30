import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { publishWindowMaxHeight, setWindowHeight } from './windowSize';
import { StatusPanel } from './components/StatusPanel';
import { SettingsPanel } from './components/SettingsPanel';
import { WelcomeScreen } from './components/WelcomeScreen';
import { DevScanPanel } from './components/DevScanPanel';
import { ActivationScreen } from './components/ActivationScreen';
import { UnlockSheet } from './components/UnlockSheet';
import { LicenseContext } from './license';
import { useSettings } from './hooks/useSettings';
import type { LicenseStatus } from './types';
import './App.css';

type View = 'activate' | 'welcome' | 'status' | 'settings' | 'devscan';

// Fixed heights per view, each clamped by setWindowHeight() to the screen's
// usable height. Status uses dynamic measurement (see StatusPanel).
const VIEW_HEIGHTS: Record<View, number> = {
  activate: 360,
  welcome: 320,
  status: 300,  // initial; StatusPanel self-sizes via ResizeObserver
  settings: 480,
  devscan: 300, // initial; DevScanPanel self-sizes to content via ResizeObserver
};

// Publish the height cap to CSS before any panel measures itself
publishWindowMaxHeight();

function App() {
  const { settings, loading, updateSettings } = useSettings();
  const [view, setView] = useState<View>('status');
  const [licenseChecked, setLicenseChecked] = useState(false);
  // Without a license the app runs in free scan mode: real scans and numbers,
  // but every clean or delete opens the unlock sheet instead
  const [licensed, setLicensed] = useState(false);
  const [unlockOpen, setUnlockOpen] = useState(false);
  // What the clean or delete that opened the sheet would free
  const [unlockBytes, setUnlockBytes] = useState<number | undefined>();

  // Check license status on mount
  useEffect(() => {
    invoke<LicenseStatus>('check_license')
      .then((status) => {
        setLicensed(status.status !== 'NotActivated' && status.status !== 'Rejected');
        setLicenseChecked(true);
      })
      .catch(() => {
        // Command failure: free scan mode (the backend refuses cleaning too)
        setLicensed(false);
        setLicenseChecked(true);
      });
  }, []);

  // Determine initial view based on first_run_completed
  useEffect(() => {
    if (!loading && licenseChecked && view !== 'activate' && !settings.first_run_completed) {
      setView('welcome');
    }
  }, [loading, licenseChecked, settings.first_run_completed]);

  // Set window size immediately on view change — fixed heights, no observer
  useEffect(() => {
    setWindowHeight(VIEW_HEIGHTS[view]);
  }, [view]);

  // Handle Escape key and click outside to close window
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        getCurrentWindow().hide();
      }
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, []);

  // Focus-loss hiding is now handled in Rust (lib.rs) for reliability

  const handleWelcomeComplete = async (launchAtLogin: boolean) => {
    await updateSettings({
      ...settings,
      first_run_completed: true,
      launch_at_login: launchAtLogin,
    });
    setView('status');
  };

  if (loading || !licenseChecked) {
    return (
      <div className="app-container">
        <div className="app-loading"><div className="loading-logo" aria-hidden="true" /></div>
      </div>
    );
  }

  return (
    <LicenseContext.Provider value={{ licensed, requestUnlock: (bytes) => { setUnlockBytes(bytes); setUnlockOpen(true); } }}>
    <div className="app-container">
      {view === 'activate' && (
        <ActivationScreen
          onActivated={() => {
            setLicensed(true);
            if (!settings.first_run_completed) {
              setView('welcome');
            } else {
              setView('status');
            }
          }}
          onCancel={() => setView(settings.first_run_completed ? 'status' : 'welcome')}
        />
      )}
      {view === 'welcome' && (
        <WelcomeScreen onComplete={handleWelcomeComplete} />
      )}
      {view === 'status' && (
        <StatusPanel
          onSettingsClick={() => setView('settings')}
          onDevScanClick={() => setView('devscan')}
        />
      )}
      {view === 'settings' && (
        <SettingsPanel
          onBack={() => setView('status')}
          onDeactivated={() => setLicensed(false)}
          onEnterKey={() => setView('activate')}
        />
      )}
      {view === 'devscan' && (
        <DevScanPanel onBack={() => setView('status')} />
      )}
      {unlockOpen && (
        <UnlockSheet
          bytes={unlockBytes}
          onClose={() => setUnlockOpen(false)}
          onHaveKey={() => { setUnlockOpen(false); setView('activate'); }}
        />
      )}
    </div>
    </LicenseContext.Provider>
  );
}

export default App;
