# SymbolSweep - Gumroad Listing Copy

## Title
**SymbolSweep — The macOS Symbolication Cache Cleaner**

## Tagline
Your Mac's `coresymbolicationd` cache is silently eating 50–100GB. This fixes that.

---

## Description

### A macOS-specific problem needs a macOS-specific tool

If you develop on a Mac, there's a system daemon called `coresymbolicationd` that caches debug symbols every time you build in Xcode, attach a debugger, or symbolicate a crash log. Apple never expires this cache. It just grows — 50GB, 100GB, sometimes more — until you notice your disk is full and go hunting.

This isn't a cross-platform problem. Windows and Linux don't have this daemon. Only macOS accumulates this cache, and only macOS developers hit the wall.

**SymbolSweep** is a menu bar utility built for exactly this. It monitors the cache, auto-cleans at your threshold, and stays out of your way.

### What it does

- **Watches the cache** — shows the current size in your menu bar
- **One-click clean** — reclaim gigabytes instantly, with a dry-run option first
- **Auto-clean** — set a threshold and forget about it
- **Dev artifact scanner** — also finds `node_modules`, `DerivedData`, build caches, and other dev cruft across your projects
- **Safe by design** — hardcoded to one path (`~/Library/Caches/com.apple.coresymbolicationd`), all deletions logged, your code is never touched

### Built for Mac developers

- Universal binary — native on Apple Silicon and Intel
- 14MB footprint, lives in the menu bar, hidden from the Dock
- Launch at login for always-on monitoring
- Built-in auto-updater, no subscription
- macOS native notifications

### Who actually needs this

- You use Xcode regularly
- You debug or profile apps on macOS
- You've been symbolicating crash logs and noticed your disk shrinking
- You checked `~/Library/Caches` and found a folder measured in tens of gigabytes

If none of that applies to you, you don't have this problem — and you don't need this tool. That's the point.

### What you get

- SymbolSweep.app (macOS, Apple Silicon + Intel)
- DMG installer — drag to Applications
- Lifetime license, free updates forever

---

## Pricing

- **$12** (Pay What You Want, $12 minimum)
- One-time purchase — no subscription, no recurring fees

---

## Screenshots Needed

1. Menu bar showing cache size
2. Main window with "Clean Now" button
3. Settings panel
4. Dev artifact scanner
5. Before/after storage comparison (optional)

---

## Installation Note (Include in description)

> **macOS Installation:**
> Since SymbolSweep isn't from the App Store, macOS may ask for confirmation:
> 1. Move SymbolSweep to your Applications folder
> 2. Right-click the app and select "Open"
> 3. Click "Open" on the security prompt
>
> This only happens once. After that, SymbolSweep runs normally.

---

## FAQ

**Is this safe?**
Yes. SymbolSweep deletes symbol cache files that macOS regenerates on demand. Your apps, projects, and system files are never touched. Every deletion is logged to `~/Library/Logs/SymbolSweep/deletions.log`.

**Will this break Xcode?**
No. The cache rebuilds automatically. You may see slightly longer symbolication times immediately after a clean — that's it.

**Why macOS only?**
Because `coresymbolicationd` is a macOS system daemon. Windows and Linux don't have it, so they don't accumulate this cache. There's no cross-platform version of this problem to solve.

**Does it support Intel Macs?**
Yes. SymbolSweep ships as a universal binary — native on both Apple Silicon (M1–M4) and Intel.

**How do updates work?**
SymbolSweep checks for updates in the background and downloads them automatically. Restart the app to apply, or check manually from Settings.

**Do I need to keep the app running?**
For auto-clean and monitoring, yes — it sits quietly in your menu bar. For manual cleaning, just open it when needed.
