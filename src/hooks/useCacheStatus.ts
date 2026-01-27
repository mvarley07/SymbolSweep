import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { CacheStatus, CleanResult } from '../types';

export function useCacheStatus() {
  const [status, setStatus] = useState<CacheStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchStatus = useCallback(async () => {
    try {
      const result = await invoke<CacheStatus>('get_status');
      setStatus(result);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    // Initial fetch
    fetchStatus();

    // Listen for status updates from backend
    const unlisten = listen<CacheStatus>('cache-status-update', (event) => {
      setStatus(event.payload);
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, [fetchStatus]);

  return { status, loading, error, refresh: fetchStatus };
}

export function useCleanCache() {
  const [cleaning, setCleaning] = useState(false);
  const [result, setResult] = useState<CleanResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  const clean = useCallback(async (dryRun: boolean = false) => {
    setCleaning(true);
    setError(null);
    setResult(null);

    try {
      const result = await invoke<CleanResult>('clean', { dryRun });
      setResult(result);
      return result;
    } catch (err) {
      const errorMsg = err instanceof Error ? err.message : String(err);
      setError(errorMsg);
      throw err;
    } finally {
      setCleaning(false);
    }
  }, []);

  const dryRun = useCallback(async () => {
    return clean(true);
  }, [clean]);

  return { clean, dryRun, cleaning, result, error };
}

export function useLastCleanTime() {
  const [lastCleanTime, setLastCleanTime] = useState<string>('Loading...');

  const fetchLastCleanTime = useCallback(async () => {
    try {
      const result = await invoke<string>('get_last_clean_time');
      setLastCleanTime(result);
    } catch {
      setLastCleanTime('Unknown');
    }
  }, []);

  useEffect(() => {
    fetchLastCleanTime();

    // Refresh every minute
    const interval = setInterval(fetchLastCleanTime, 60000);
    return () => clearInterval(interval);
  }, [fetchLastCleanTime]);

  return { lastCleanTime, refresh: fetchLastCleanTime };
}
