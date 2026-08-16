import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow, LogicalSize } from '@tauri-apps/api/window';
import { StatusPanel } from './components/StatusPanel';
import { SettingsPanel } from './components/SettingsPanel';
import { WelcomeScreen } from './components/WelcomeScreen';
import { DevScanPanel } from './components/DevScanPanel';
import { ActivationScreen } from './components/ActivationScreen';
import { useSettings } from './hooks/useSettings';
import type { LicenseStatus } from './types';
import './App.css';

type View = 'activate' | 'welcome' | 'status' | 'settings' | 'devscan';

const WINDOW_WIDTH = 280;

// Fixed heights per view. Status uses dynamic measurement (see StatusPanel).
const VIEW_HEIGHTS: Record<View, number> = {
  activate: 360,
  welcome: 320,
  status: 300,  // initial; StatusPanel self-sizes via ResizeObserver
  settings: 480,
  devscan: 520,
};

function App() {
  const { settings, loading, updateSettings } = useSettings();
  const [view, setView] = useState<View>('status');
  const [licenseChecked, setLicenseChecked] = useState(false);

  // Check license status on mount
  useEffect(() => {
    invoke<LicenseStatus>('check_license')
      .then((status) => {
        if (status.status === 'NotActivated' || status.status === 'Rejected') {
          setView('activate');
        }
        setLicenseChecked(true);
      })
      .catch(() => {
        // Command failure with no stored activation → block
        setView('activate');
        setLicenseChecked(true);
      });
  }, []);

  // Determine initial view based on first_run_completed
  // (only runs after license is confirmed valid)
  useEffect(() => {
    if (!loading && licenseChecked && view !== 'activate' && !settings.first_run_completed) {
      setView('welcome');
    }
  }, [loading, licenseChecked, settings.first_run_completed]);

  // Set window size immediately on view change — fixed heights, no observer
  useEffect(() => {
    getCurrentWindow().setSize(
      new LogicalSize(WINDOW_WIDTH, VIEW_HEIGHTS[view]),
    );
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
    <div className="app-container">
      {view === 'activate' && (
        <ActivationScreen onActivated={() => {
          if (!settings.first_run_completed) {
            setView('welcome');
          } else {
            setView('status');
          }
        }} />
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
          onDeactivated={() => setView('activate')}
        />
      )}
      {view === 'devscan' && (
        <DevScanPanel onBack={() => setView('status')} />
      )}
    </div>
  );
}

export default App;
