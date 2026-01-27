import { useState, useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { StatusPanel } from './components/StatusPanel';
import { SettingsPanel } from './components/SettingsPanel';
import { WelcomeScreen } from './components/WelcomeScreen';
import { useSettings } from './hooks/useSettings';
import './App.css';

type View = 'welcome' | 'status' | 'settings';

function App() {
  const { settings, loading, updateSettings } = useSettings();
  const [view, setView] = useState<View>('status');

  // Determine initial view based on first_run_completed
  useEffect(() => {
    if (!loading && !settings.first_run_completed) {
      setView('welcome');
    }
  }, [loading, settings.first_run_completed]);

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
