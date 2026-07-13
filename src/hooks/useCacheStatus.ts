import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { AppStatus, CacheStatus, CleanResult, DevDeleteResult, DevScanResult } from '../types';

export function useAppStatus() {
  const [status, setStatus] = useState<AppStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchStatus = useCallback(async () => {
    try {
      const result = await invoke<AppStatus>('get_app_status');
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

    // Listen for unified status updates from backend
    const unlisten = listen<AppStatus>('app-status-update', (event) => {
      setStatus(event.payload);
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, [fetchStatus]);

  return { status, loading, error, refresh: fetchStatus };
}

/** @deprecated Use useAppStatus instead — kept for backward compatibility */
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
    fetchStatus();
    const unlisten = listen<AppStatus>('app-status-update', (event) => {
      setStatus(event.payload.cache);
    });
    return () => { unlisten.then((fn) => fn()); };
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

export function useDevScan() {
  const [result, setResult] = useState<DevScanResult | null>(null);
  const [scanning, setScanning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Load cached result on mount
  useEffect(() => {
    invoke<DevScanResult | null>('get_dev_scan_result').then((cached) => {
      if (cached) setResult(cached);
    });

    // Listen for background scan completion
    const unlisten = listen<DevScanResult>('dev-scan-ready', (event) => {
      setResult(event.payload);
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const scan = useCallback(async () => {
    setScanning(true);
    setError(null);
    try {
      const res = await invoke<DevScanResult>('scan_dev');
      setResult(res);
      return res;
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      throw err;
    } finally {
      setScanning(false);
    }
  }, []);

  return { result, scanning, error, scan };
}

export function useDeleteDevArtifacts() {
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const deleteArtifacts = useCallback(async (paths: string[]) => {
    setDeleting(true);
    setError(null);
    try {
      const result = await invoke<DevDeleteResult>('delete_dev_artifacts', { paths });
      return result;
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      throw err;
    } finally {
      setDeleting(false);
    }
  }, []);

  return { deleteArtifacts, deleting, error };
}

/** Manual deletion — allows Rebuildable/SafeWithReinstall, blocks Ask tier */
export function useDeleteDevArtifactsManual() {
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const deleteArtifacts = useCallback(async (paths: string[]) => {
    setDeleting(true);
    setError(null);
    try {
      const result = await invoke<DevDeleteResult>('delete_dev_artifacts_manual', { paths });
      return result;
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      throw err;
    } finally {
      setDeleting(false);
    }
  }, []);

  return { deleteArtifacts, deleting, error };
}

export function useLastCleanTime() {
  const [lastCleanTime, setLastCleanTime] = useState<string>('Loading...');
  const [refreshTrigger, setRefreshTrigger] = useState(0);
  const timeoutRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  const fetchLastCleanTime = useCallback(async () => {
    try {
      const result = await invoke<string>('get_last_clean_time');
      setLastCleanTime(result);
      return result;
    } catch {
      setLastCleanTime('Unknown');
      return 'Unknown';
    }
  }, []);

  // Call this after cleaning to reset the timer
  const refresh = useCallback(async () => {
    // Clear existing timer
    if (timeoutRef.current) {
      clearTimeout(timeoutRef.current);
    }
    // Fetch immediately
    await fetchLastCleanTime();
    // Trigger effect to restart timer from now
    setRefreshTrigger(prev => prev + 1);
  }, [fetchLastCleanTime]);

  useEffect(() => {
    const scheduleNextFetch = async () => {
      const result = await fetchLastCleanTime();
      // Update every 10 seconds when showing seconds, every 60 seconds otherwise
      const interval = result.includes('second') ? 10000 : 60000;
      timeoutRef.current = setTimeout(scheduleNextFetch, interval);
    };

    scheduleNextFetch();
    return () => {
      if (timeoutRef.current) {
        clearTimeout(timeoutRef.current);
      }
    };
  }, [fetchLastCleanTime, refreshTrigger]);

  return { lastCleanTime, refresh };
}
