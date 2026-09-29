import { useState } from 'react';
import type { ArtifactTier, DevArtifact, SkippedArtifact } from '../types';
import './DevScanPanel.css';

// Artifact row markup shared by the Dev Artifacts panel and the main screen's
// "Safe to clean" list, so both render identical rows from the same tokens.

export const TIER_CONFIG: Record<ArtifactTier, { label: string; desc: string; className: string }> = {
  Safe: { label: 'SAFE', desc: 'Caches \u2014 regenerate automatically', className: 'tier-safe' },
  Rebuildable: { label: 'REBUILD', desc: 'Build artifacts \u2014 slow to rebuild', className: 'tier-rebuild' },
  SafeWithReinstall: { label: 'REINSTALL', desc: 'npm install to restore', className: 'tier-reinstall' },
  Ask: { label: 'REVIEW', desc: 'May contain shipped output', className: 'tier-ask' },
};

export interface ArtifactRowProps {
  artifact: DevArtifact;
  onDelete: (path: string) => void;
  deleting: boolean;
}

const STALE_THRESHOLD_DAYS = 14;

const TRASH_ICON = (
  <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
    <path d="M2.5 4.5h11M6 4.5V3a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1v1.5M4 4.5l.5 8.5a1 1 0 0 0 1 1h5a1 1 0 0 0 1-1l.5-8.5" />
  </svg>
);

/** Generate the removal command/instruction for REVIEW-tier artifacts */
function getRemovalInfo(artifact: DevArtifact): { command: string; note?: string } | { instruction: string } | null {
  const shortPath = artifact.path.replace(/^\/Users\/[^/]+/, '~');
  switch (artifact.kind) {
    case 'Docker':
      return { command: 'docker system prune -a --volumes', note: 'Deletes volumes \u2014 may include databases' };
    case 'Xcode Archives':
      return { instruction: 'In Xcode: Window \u2192 Organizer \u2192 delete archives you no longer need' };
    case 'iOS Simulators':
      return { instruction: 'In Xcode: Settings \u2192 Platforms \u2192 delete unused simulators' };
    case 'Android emulator images':
      return { instruction: 'In Android Studio: Device Manager \u2192 delete unused AVDs' };
    default:
      if (artifact.kind.endsWith(' output')) {
        return { command: `rm -rf ${shortPath}` };
      }
      return null;
  }
}

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // clipboard API may fail in some contexts
    }
  };

  return (
    <button className="copy-btn" onClick={handleCopy} data-tip="Copy just this item" aria-label="Copy just this item">
      {copied ? (
        <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
          <path d="M3 8.5l3 3 7-7" />
        </svg>
      ) : (
        <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
          <rect x="5" y="5" width="8" height="8" rx="1" />
          <path d="M3 11V3a1 1 0 0 1 1-1h8" />
        </svg>
      )}
    </button>
  );
}

/** Kind without its parenthetical: "Rust target (build artifacts)" -> "Rust target" */
function shortKind(kind: string): string {
  return kind.replace(/\s*\([^)]*\)\s*$/, '');
}

/** Row title, project first: "padfinder · node_modules". The tier badge carries the type. */
function RowTitle({ kind, project, title }: { kind: string; project: string | null; title: string }) {
  return (
    <span className="artifact-title" title={title}>
      {project && <><span className="artifact-title-project">{project}</span> &middot; </>}
      {shortKind(kind).split('/').map((part, i) => (
        <span key={i}>{i > 0 && <>/<wbr /></>}{part}</span>
      ))}
    </span>
  );
}

/** Row copy for the scan's in-use reason: "modified 6m ago" -> "In use · changed 6m ago" */
function inUseLabel(reason: string): string {
  if (reason.startsWith('modified ')) return `In use \u00b7 changed ${reason.slice('modified '.length)}`;
  return reason.charAt(0).toUpperCase() + reason.slice(1);
}

/** Short label for a skipped path: the artifact kind if known, else ~-relative path */
function skippedLabel(item: SkippedArtifact, artifacts: DevArtifact[] | undefined) {
  const artifact = artifacts?.find(a => a.path === item.path);
  return {
    kind: artifact?.kind ?? item.path.replace(/^\/Users\/[^/]+/, '~'),
    project: artifact?.project ?? null,
  };
}

export function SkippedRow({ item, artifacts }: { item: SkippedArtifact; artifacts: DevArtifact[] | undefined }) {
  const { kind, project } = skippedLabel(item, artifacts);
  const shortPath = item.path.replace(/^\/Users\/[^/]+/, '~');
  return (
    <div className="artifact-row skipped-row">
      <div className="artifact-body">
        <div className="artifact-text">
          <div className="artifact-main">
            <span className="artifact-tier-badge tier-skipped">SKIPPED</span>
            <RowTitle kind={kind} project={project} title={shortPath} />
          </div>
          <div className="artifact-hint skipped-reason">skipped: {item.reason}</div>
        </div>
        <div className="artifact-actions">
          <span className="artifact-size">{item.size_display}</span>
          <span className="artifact-delete-spacer" />
        </div>
      </div>
    </div>
  );
}

export function ArtifactRow({ artifact, onDelete, deleting }: ArtifactRowProps) {
  const config = TIER_CONFIG[artifact.tier];

  // Only show staleness for genuinely unused artifacts (14+ days)
  const staleness = artifact.staleness_days != null && artifact.staleness_days >= STALE_THRESHOLD_DAYS
    ? `${artifact.staleness_days}d unused`
    : null;

  // Delete button for SAFE/REBUILD/REINSTALL, not active builds. Nested rows
  // (node_modules/.cache) are deletable on their own and go to Trash.
  const showDelete = artifact.tier !== 'Ask' && !artifact.active_build && !artifact.in_use;

  // REVIEW-tier: show removal command/instruction instead of delete
  const isReview = artifact.tier === 'Ask';
  const removalInfo = isReview ? getRemovalInfo(artifact) : null;

  const shortPath = artifact.path.replace(/^\/Users\/[^/]+/, '~');

  return (
    <div className={`artifact-row ${artifact.is_nested ? 'nested' : ''} ${artifact.active_build ? 'active-build' : ''} ${isReview ? 'tier-ask-row' : ''} ${artifact.in_use ? 'in-use-row' : ''}`}>
      <div className="artifact-body">
        <div className="artifact-text">
          <div className="artifact-main">
            <span className={`artifact-tier-badge ${config.className}`}>
              {artifact.active_build ? 'BUILDING' : config.label}
            </span>
            <RowTitle kind={artifact.kind} project={artifact.project} title={shortPath} />
          </div>
          {artifact.active_build ? (
            <div className="artifact-hint in-use-reason">Building now &middot; will retry</div>
          ) : artifact.in_use ? (
            <div className="artifact-hint in-use-reason">{inUseLabel(artifact.in_use)}</div>
          ) : (artifact.hint || staleness) && (
            <div className="artifact-hint" title={artifact.hint ?? undefined}>
              {staleness && (
                <span className="artifact-staleness" aria-label={`Unused for ${artifact.staleness_days} days`}>{staleness}</span>
              )}
              {staleness && artifact.hint && ' \u00b7 '}
              {artifact.hint}
            </div>
          )}
          {removalInfo && 'command' in removalInfo && (
            <div className="removal-command">
              <code>{removalInfo.command}</code>
              <CopyButton text={removalInfo.command} />
              {removalInfo.note && <span className="removal-note">{removalInfo.note}</span>}
            </div>
          )}
          {removalInfo && 'instruction' in removalInfo && (
            <div className="removal-instruction">{removalInfo.instruction}</div>
          )}
        </div>
        <div className="artifact-actions">
          <span className="artifact-size">{artifact.size_display}</span>
          {showDelete ? (
            <button
              className="artifact-delete-btn"
              onClick={() => onDelete(artifact.path)}
              disabled={deleting}
              data-tip="Remove from rack"
              aria-label="Remove from rack"
            >
              {TRASH_ICON}
            </button>
          ) : artifact.in_use ? (
            <button
              className="artifact-delete-btn"
              disabled
              title="In use, will retry"
              aria-label="In use, will retry"
            >
              {TRASH_ICON}
            </button>
          ) : (
            <span className="artifact-delete-spacer" />
          )}
        </div>
      </div>
    </div>
  );
}
