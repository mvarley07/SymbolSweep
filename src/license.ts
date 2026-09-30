import { createContext, useContext } from 'react';

/** Lemon Squeezy checkout for a SymbolSweep license */
export const CHECKOUT_URL = 'https://vdevtools.lemonsqueezy.com/checkout/buy/a925dc06-6f9e-4554-8936-e90b062edc76';

/** The backend's refusal when a clean or delete runs without a license */
export const LICENSE_REQUIRED = 'license_required';

interface LicenseState {
  /** A key is activated on this Mac. Without one the app runs in free scan
   *  mode: it scans and shows real numbers, but can't clean or delete. */
  licensed: boolean;
  /** Show the unlock sheet (checkout, or "I have a key"). `bytes` is what the
   *  triggering clean or delete would free; the buy button names it. */
  requestUnlock: (bytes?: number) => void;
}

export const LicenseContext = createContext<LicenseState>({ licensed: true, requestUnlock: () => {} });

export const useLicense = () => useContext(LicenseContext);
