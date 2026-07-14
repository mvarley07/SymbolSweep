import { useState, useEffect, useRef, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow, LogicalSize } from '@tauri-apps/api/window';
import { StatusPanel } from './components/StatusPanel';
import { SettingsPanel } from './components/SettingsPanel';
import { WelcomeScreen } from './components/WelcomeScreen';
import { DevScanPanel } from './components/DevScanPanel';
import { useSettings } from './hooks/useSettings';
import './App.css';

type View = 'welcome' | 'status' | 'settings' | 'devscan';

const WINDOW_WIDTH = 280;

// Max heights — caps for views with scrollable content
const MAX_HEIGHTS: Record<View, number> = {
  welcome: 320,
  status: 400,
  settings: 420,
  devscan: 520,
};

function App() {
  const { settings, loading, updateSettings } = useSettings();
  const [view, setView] = useState<View>('status');
  const containerRef = useRef<HTMLDivElement>(null);

  // Determine initial view based on first_run_completed
  useEffect(() => {
    if (!loading && !settings.first_run_completed) {
      setView('welcome');
    }
  }, [loading, settings.first_run_completed]);

  // Resize window to match actual content height
  const syncWindowHeight = useCallback(() => {
    const el = containerRef.current;
    if (!el) return;
    // scrollHeight = full content height, ignoring any overflow clipping
    const contentHeight = el.scrollHeight;
    const maxHeight = MAX_HEIGHTS[view];
    const height = Math.min(contentHeight, maxHeight);
    getCurrentWindow().setSize(new LogicalSize(WINDOW_WIDTH, height));
  }, [view]);

  // Observe content size changes and resize window accordingly
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;

    // Initial sync after a frame (let layout settle)
    const raf = requestAnimationFrame(syncWindowHeight);

    const observer = new ResizeObserver(syncWindowHeight);
    observer.observe(el);

    return () => {
      cancelAnimationFrame(raf);
      observer.disconnect();
    };
  }, [syncWindowHeight]);

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
    <div className="app-container" ref={containerRef}>
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
