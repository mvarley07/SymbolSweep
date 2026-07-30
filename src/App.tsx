import { useState, useEffect } from 'react';
import { getCurrentWindow, LogicalSize } from '@tauri-apps/api/window';
import { StatusPanel } from './components/StatusPanel';
import { SettingsPanel } from './components/SettingsPanel';
import { WelcomeScreen } from './components/WelcomeScreen';
import { DevScanPanel } from './components/DevScanPanel';
import { useSettings } from './hooks/useSettings';
import './App.css';

type View = 'welcome' | 'status' | 'settings' | 'devscan';

const WINDOW_WIDTH = 280;

// Fixed heights per view. Status uses dynamic measurement (see StatusPanel).
const VIEW_HEIGHTS: Record<View, number> = {
  welcome: 320,
  status: 300,  // initial; StatusPanel self-sizes via ResizeObserver
  settings: 420,
  devscan: 520,
};

function App() {
  const { settings, loading, updateSettings } = useSettings();
  const [view, setView] = useState<View>('status');

  // Determine initial view based on first_run_completed
  useEffect(() => {
    if (!loading && !settings.first_run_completed) {
      setView('welcome');
    }
  }, [loading, settings.first_run_completed]);

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

  if (loading) {
    return (
      <div className="app-container">
        <div className="app-loading"><div className="loading-logo" aria-hidden="true" /></div>
      </div>
    );
  }

  return (
    <div className="app-container">
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
        <SettingsPanel onBack={() => setView('status')} />
      )}
      {view === 'devscan' && (
        <DevScanPanel onBack={() => setView('status')} />
      )}
    </div>
  );
}

export default App;
