import { useState, useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { StatusPanel } from './components/StatusPanel';
import { SettingsPanel } from './components/SettingsPanel';
import { WelcomeScreen } from './components/WelcomeScreen';
import { useSettings } from './hooks/useSettings';
import './App.css';

type View = 'welcome' | 'status' | 'settings';

// Window heights for different views
const VIEW_HEIGHTS = {
  welcome: 320,
  status: 275,
  settings: 400,
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

  // Resize window when view changes
  useEffect(() => {
    const appWindow = getCurrentWindow();
    const height = VIEW_HEIGHTS[view];
    appWindow.setSize({ width: 280, height, type: 'Logical' });
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

  // Listen for clean request from tray menu
  useEffect(() => {
    const unlisten = listen('clean-requested', () => {
      // The clean will be triggered, we just need to be visible
      // The StatusPanel will handle the actual clean
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

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
        <div className="app-loading">Loading...</div>
      </div>
    );
  }

  return (
    <div className="app-container">
      {view === 'welcome' && (
        <WelcomeScreen onComplete={handleWelcomeComplete} />
      )}
      {view === 'status' && (
        <StatusPanel onSettingsClick={() => setView('settings')} />
      )}
      {view === 'settings' && (
        <SettingsPanel onBack={() => setView('status')} />
      )}
    </div>
  );
}

export default App;
