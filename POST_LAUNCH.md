# Post-launch

Things to review after the 2.1.8 launch. No release is planned for these yet.

## Hero sub-line wording

The line under the big number (`heroReady` in `src/components/StatusPanel.tsx`):

- Something to clean: "1.2 GB ready to clean now"
- Nothing ready, some held back: "0 B ready · 284 MB held back"
- Nothing ready, nothing held back: hidden

To review: whether "held back" reads clearly to someone who hasn't seen the rows yet, and whether the line should hint at when the held-back amount frees up.

## Badge contrast check

Check text contrast against the panel (`#24272C`), in both badge sizes:

- State badge (`.status-state`, `StatusPanel.css`, 10px, weight 600): "Found: heavy" `#f87171` on `rgba(248,113,113,0.08)`, "Found: moderate" `#eab308` on `rgba(234,179,8,0.10)`, runaway `#f87171` on `rgba(248,113,113,0.10)`
- Tier badges (`.artifact-tier-badge`, `DevScanPanel.css`, 8px, weight 700): SAFE, REBUILD, REINSTALL, REVIEW, BUILDING, SKIPPED
- Dimmed held-back rows (`.in-use-row`): badge and title are faded further

Small text needs 4.5:1 for WCAG AA. Check the dimmed rows especially.

## Held-back section UX

Today (2.1.8):

- Safe rows that can't be deleted right now stay in "Safe to clean", dimmed, with a disabled trash icon and one label saying why and when (`heldBackLabel` in `ArtifactRows.tsx`): "Changed 2h ago · ready in 22h", "In use by node · ready when it stops", "Building now · ready when the build ends"
- The "Safe to clean" header is a label only; Clean and the ready line count only the deletable rows
- Disabled Rebuild/Reinstall buttons say why: "Held back · building"
- Opening the popup rechecks only the held-back rows, so they clear as soon as the reason ends

Open questions:

- Should held-back rows sit below the deletable ones, or collapse into one "Held back (2)" line?
- Should a held-back row be cleaned automatically once it clears, or wait for the next Clean?
- Is "held back" the right name everywhere, or should rows and totals use one word?

## Dev Artifacts total row

Today the total (`.scan-total-only` in `DevScanPanel.tsx`) sits alone, right-aligned to the row sizes (`padding-right: var(--size-edge)` in `DevScanPanel.css`). It stops about 50px short of the tiles' and buttons' right edge, so it looks like it's floating.

Change:

- Add a "FOUND" label (`.total-label`) flush left
- Put the total flush right
- Line both up with the tile grid's edges: drop the `--size-edge` padding for `.scan-total-only`

## Customer feedback

Append new entries at the bottom: date, where it came from, what they said, what we did.

<!-- - 2026-10-01 · email · "…" · → action -->
