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
          <div className="activation-icon" aria-hidden="true" />
          <h1>Activate SymbolSweep</h1>
        </div>

        <p className="activation-description">
          Enter your license key to get started.
        </p>

        <div className="activation-action">
          <input
            type="text"
            className={`activation-input${key ? ' has-value' : ''}`}
            placeholder="Your license key"
            value={key}
            onChange={(e) => setKey(e.target.value)}
            onKeyDown={handleKeyDown}
            disabled={loading}
            autoFocus
            spellCheck={false}
            autoComplete="off"
          />

          <p className="activation-helper">Sent to your email after purchase.</p>

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
    </div>
  );
}
