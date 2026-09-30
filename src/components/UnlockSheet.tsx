import { openUrl } from '@tauri-apps/plugin-opener';
import { CHECKOUT_URL } from '../license';
import './UnlockSheet.css';

interface UnlockSheetProps {
  onClose: () => void;
  onHaveKey: () => void;
}

/** Shown by any clean or delete action in free scan mode */
export function UnlockSheet({ onClose, onHaveKey }: UnlockSheetProps) {
  return (
    <div className="unlock-backdrop" onClick={onClose}>
      <div className="unlock-sheet" role="dialog" aria-labelledby="unlock-title" onClick={e => e.stopPropagation()}>
        <h2 id="unlock-title" className="unlock-title">Unlock cleaning</h2>
        <p className="unlock-text">
          Scanning is free. Cleaning what SymbolSweep finds, and keeping the symbol cache
          in check automatically, come with a license: one payment, one Mac.
        </p>
        <button className="unlock-buy" onClick={() => { openUrl(CHECKOUT_URL); onClose(); }}>
          Unlock cleaning, $12
        </button>
        <div className="unlock-links">
          <button className="unlock-link" onClick={onHaveKey}>I have a key</button>
          <button className="unlock-link" onClick={onClose}>Not now</button>
        </div>
      </div>
    </div>
  );
}
