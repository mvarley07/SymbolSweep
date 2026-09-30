import { openUrl } from '@tauri-apps/plugin-opener';
import { CHECKOUT_URL } from '../license';
import { formatSize } from '../formatSize';
import './UnlockSheet.css';

interface UnlockSheetProps {
  /** What the triggering action would free; the buy button names it */
  bytes?: number;
  onClose: () => void;
  onHaveKey: () => void;
}

/** Shown by any clean or delete action in free scan mode */
export function UnlockSheet({ bytes, onClose, onHaveKey }: UnlockSheetProps) {
  return (
    <div className="unlock-backdrop" onClick={onClose}>
      <div className="unlock-sheet" role="dialog" aria-labelledby="unlock-title" onClick={e => e.stopPropagation()}>
        <h2 id="unlock-title" className="unlock-title">Unlock cleaning</h2>
        <p className="unlock-text">
          Scanning is free. A license cleans everything SymbolSweep finds and keeps the
          symbol cache in check automatically. $12 once, for one Mac.
        </p>
        <button className="unlock-buy" onClick={() => { openUrl(CHECKOUT_URL); onClose(); }}>
          {bytes && bytes > 0 ? `Free up ${formatSize(bytes)}, $12` : 'Unlock cleaning, $12'}
        </button>
        <div className="unlock-links">
          <button className="unlock-link" onClick={onHaveKey}>I have a key</button>
          <button className="unlock-link" onClick={onClose}>Not now</button>
        </div>
      </div>
    </div>
  );
}
