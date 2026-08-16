import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { ActivationResult } from '../types';
import './ActivationScreen.css';

interface ActivationScreenProps {
  onActivated: () => void;
}

export function ActivationScreen({ onActivated }: ActivationScreenProps) {
  const [key, setKey] = useState('');
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleActivate = async () => {
    const trimmed = key.trim();
    if (!trimmed) return;

    setLoading(true);
    setError(null);

    try {
      const result = await invoke<ActivationResult>('activate_license', { key: trimmed });

      if (result.success) {
        onActivated();
      } else if (result.is_limit_reached) {
        const usage = result.activation_usage ?? '?';
        const limit = result.activation_limit ?? '?';
        setError(
          `This key is already active on ${usage} of ${limit} machines. ` +
          `To free a slot, open SymbolSweep on one of those machines and ` +
          `deactivate it from Settings.`
        );
      } else {
        setError(result.error ?? 'Activation failed');
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !loading && key.trim()) {
      handleActivate();
    }
  };

  return (
    <div className="activation-screen">
      <div className="activation-content">
        <div className="activation-header">
          <div className="activation-icon">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5">
              <rect x="3" y="11" width="18" height="11" rx="2" ry="2" />
              <path d="M7 11V7a5 5 0 0 1 10 0v4" />
            </svg>
          </div>
          <h1>Activate SymbolSweep</h1>
        </div>

        <p className="activation-description">
          Enter your license key to get started.
        </p>

        <input
          type="text"
          className="activation-input"
          placeholder="Your license key"
          value={key}
          onChange={(e) => setKey(e.target.value)}
          onKeyDown={handleKeyDown}
          disabled={loading}
          autoFocus
          spellCheck={false}
          autoComplete="off"
        />

        {error && <p className="activation-error">{error}</p>}

        <button
          className="activation-button"
          onClick={handleActivate}
          disabled={loading || !key.trim()}
        >
          {loading ? 'Activating\u2026' : 'Activate'}
        </button>
      </div>
    </div>
  );
}
