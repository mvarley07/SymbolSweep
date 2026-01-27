import type { CleanResult } from '../types';
import './CleanConfirmation.css';

interface CleanConfirmationProps {
  onConfirm: () => void;
  onCancel: () => void;
  onDryRun: () => void;
  dryRunResult?: CleanResult | null;
  loading?: boolean;
}

export function CleanConfirmation({
  onConfirm,
  onCancel,
  onDryRun,
  dryRunResult,
  loading,
}: CleanConfirmationProps) {
  return (
    <div className="clean-confirmation">
      <div className="confirmation-header">
        <div className="confirmation-icon">
          <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5">
            <path d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z" />
          </svg>
        </div>
        <h2>First Clean</h2>
      </div>

      <div className="confirmation-content">
        <p className="safety-message">
          <strong>SymbolSweep only deletes macOS debug symbol cache.</strong>
        </p>
        <p className="safety-details">
          This is safe to remove and macOS will regenerate it as needed.
          Your code, projects, and files are never touched.
        </p>

        <div className="safety-info">
          <div className="info-row">
            <span className="info-icon">&#10003;</span>
            <span>Only deletes from <code>~/Library/Caches/com.apple.coresymbolicationd</code></span>
          </div>
          <div className="info-row">
            <span className="info-icon">&#10003;</span>
            <span>All deletions are logged to a local file</span>
          </div>
          <div className="info-row">
            <span className="info-icon">&#10003;</span>
            <span>No user files, projects, or code are ever touched</span>
          </div>
        </div>

        {dryRunResult && (
          <div className="dry-run-result">
            <h3>Dry Run Result</h3>
            <p>
              Would delete <strong>{dryRunResult.bytes_freed_display}</strong> ({dryRunResult.files_removed} items)
            </p>
            {dryRunResult.items_found.length > 0 && (
              <ul className="items-list">
                {dryRunResult.items_found.slice(0, 5).map((item, i) => (
                  <li key={i}>
                    {item.is_directory ? '📁' : '📄'} {item.path} ({item.size_display})
                  </li>
                ))}
                {dryRunResult.items_found.length > 5 && (
                  <li className="more-items">
                    ...and {dryRunResult.items_found.length - 5} more items
                  </li>
                )}
              </ul>
            )}
          </div>
        )}
      </div>

      <div className="confirmation-actions">
        <button
          className="btn-secondary"
          onClick={onCancel}
          disabled={loading}
        >
          Cancel
        </button>
        <button
          className="btn-secondary"
          onClick={onDryRun}
          disabled={loading}
        >
          {loading ? 'Checking...' : 'Dry Run'}
        </button>
        <button
          className="btn-primary"
          onClick={onConfirm}
          disabled={loading}
        >
          {loading ? 'Cleaning...' : 'Clean Now'}
        </button>
      </div>
    </div>
  );
}
