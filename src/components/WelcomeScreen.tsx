import { useState } from 'react';
import './WelcomeScreen.css';

interface WelcomeScreenProps {
  onComplete: (launchAtLogin: boolean) => void;
}

export function WelcomeScreen({ onComplete }: WelcomeScreenProps) {
  const [launchAtLogin, setLaunchAtLogin] = useState(true);

  return (
    <div className="welcome-screen">
      <div className="welcome-arrow">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
          <path d="M12 19V5M5 12l7-7 7 7" />
        </svg>
      </div>

      <div className="welcome-content">
        <div className="welcome-header">
          <div className="welcome-icon">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
              <polyline points="20 6 9 17 4 12" />
            </svg>
          </div>
          <div className="welcome-titles">
            <h1>SymbolSweep is running</h1>
            <p className="welcome-subtitle">in your menu bar</p>
          </div>
        </div>

        <p className="welcome-description">
          Monitors your macOS debug symbol cache and cleans it before it causes filesystem issues.
        </p>

        <div className="welcome-features">
          <div className="feature-item">
            <span className="feature-icon">✓</span>
            <span>Automatic monitoring in the background</span>
          </div>
          <div className="feature-item">
            <span className="feature-icon">✓</span>
            <span>One-click cleaning when needed</span>
          </div>
          <div className="feature-item">
            <span className="feature-icon">✓</span>
            <span>Safe — only touches Apple's cache folder</span>
          </div>
        </div>

        <label className="welcome-toggle">
          <input
            type="checkbox"
            checked={launchAtLogin}
            onChange={(e) => setLaunchAtLogin(e.target.checked)}
          />
          <span className="toggle-slider" />
          <span className="toggle-label">Launch at login</span>
        </label>

        <button
          className="welcome-button"
          onClick={() => onComplete(launchAtLogin)}
        >
          Get Started
        </button>
      </div>
    </div>
  );
}
