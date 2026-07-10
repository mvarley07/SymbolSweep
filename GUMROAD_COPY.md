# SymbolSweep - Gumroad Listing Copy

## Title
**SymbolSweep - Reclaim Your Mac's Hidden Storage**

## Tagline
Stop the silent disk hog that's eating your Mac's storage.

---

## Description

### The Problem You Didn't Know You Had

If you use Xcode, debug apps, or analyze crash logs on your Mac, there's a hidden cache quietly growing in the background: **CoreSymbolication**.

It can silently balloon to **50GB, 100GB, or more** — and macOS never cleans it up. You only notice when you're out of disk space and wondering where it all went.

### The Fix

**SymbolSweep** is a lightweight menu bar app that:

- **Monitors** your CoreSymbolication cache in real-time
- **Auto-cleans** when it hits your threshold (default: 5GB)
- **Notifies** you before storage becomes a problem
- **Lives quietly** in your menu bar — set it and forget it

### Features

- **One-click clean** — Free up gigabytes instantly
- **Dev artifact scanner** — Find and clean node_modules, build caches, Xcode DerivedData, and more
- **Auto-clean** — Set a threshold, never think about it again
- **Smart notifications** — Only alerts when action is needed
- **Tiny footprint** — 14MB app, minimal resource usage
- **Launch at login** — Always watching, never intrusive
- **Auto-updates** — Built-in updater keeps you current

### Who It's For

- macOS developers using Xcode
- Anyone who debugs or symbolizes crash logs
- Mac users who've mysteriously lost disk space

### What You Get

- SymbolSweep.app (macOS, Apple Silicon + Intel)
- Lightweight DMG installer — drag to Applications and go
- Built-in auto-updater — always stay current
- Free updates for life

### Download

After purchase, download the latest DMG from:
[GitHub Releases](https://github.com/mvarley07/SymbolSweep/releases/latest)

---

## Pricing

- **$12** (Pay What You Want, $12 minimum)
- Lifetime license — no subscriptions, no recurring fees
- Free updates forever via built-in auto-updater

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
Yes. SymbolSweep only deletes symbol cache files that macOS regenerates as needed. Your apps, data, and system files are never touched.

**Will this break Xcode?**
No. The cache rebuilds automatically when needed. You may see slightly longer symbolication times immediately after cleaning, but that's it.

**Does it support Intel Macs?**
Yes! SymbolSweep ships as a universal binary that runs natively on both Apple Silicon (M1/M2/M3/M4) and Intel Macs.

**How do updates work?**
SymbolSweep checks for updates automatically in the background. When an update is available, it downloads and installs silently. Just restart the app to apply. You can also check manually from Settings.

**Do I need to keep the app running?**
For auto-clean and monitoring, yes — it sits quietly in your menu bar. For manual cleaning, just open it when needed.
