import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { Settings } from '../types';

const defaultSettings: Settings = {
  auto_clean_on_threshold: false,
  auto_clean_threshold: 5 * 1024 * 1024 * 1024, // 5GB
  auto_clean_scheduled: false,
  auto_clean_interval_secs: 6 * 60 * 60, // 6 hours
  show_notifications: true,
  launch_at_login: false,
  last_clean_timestamp: 0,
  monitor_interval_secs: 60,
  debug_mode: false,
  debug_simulated_size: 0,
  first_run_completed: false,
  first_clean_confirmed: false,
};

export function useSettings() {
  const [settings, setSettings] = useState<Settings>(defaultSettings);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchSettings = useCallback(async () => {
    try {
      const result = await invoke<Settings>('get_settings');
      setSettings(result);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  const updateSettings = useCallback(async (newSettings: Settings) => {
    setSaving(true);
    setError(null);

    try {
      await invoke('update_settings', { settings: newSettings });
      setSettings(newSettings);
    } catch (err) {
      const errorMsg = err instanceof Error ? err.message : String(err);
      setError(errorMsg);
      throw err;
    } finally {
      setSaving(false);
    }
  }, []);

  const updateSetting = useCallback(
    async <K extends keyof Settings>(key: K, value: Settings[K]) => {
      const newSettings = { ...settings, [key]: value };
      await updateSettings(newSettings);
    },
    [settings, updateSettings]
  );

  useEffect(() => {
    fetchSettings();

    // Listen for settings updates from backend (e.g., after clean resets debug size)
    const unlisten = listen<Settings>('settings-updated', (event) => {
      setSettings(event.payload);
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, [fetchSettings]);

  return {
    settings,
    loading,
    saving,
    error,
    updateSettings,
    updateSetting,
    refresh: fetchSettings,
  };
}
