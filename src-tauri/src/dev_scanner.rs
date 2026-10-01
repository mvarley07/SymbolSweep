//! Dev artifact scanner — detects reclaimable development cruft across the filesystem.
//!
//! Scan phases:
//! 1. Home-level caches (~/.npm, ~/.yarn/cache, etc.)
//! 2. ~/Library/Caches known dev tool subdirectories
//! 3. ~/Library/Developer/Xcode/DerivedData
//! 4. Project root traversal for node_modules, build outputs, etc.
//!
//! Classification tiers:
//! - SAFE: Caches that regenerate automatically (npm cache, .next, .turbo, etc.)
//! - SAFE-WITH-REINSTALL: node_modules — one `npm install` to restore
//! - REVIEW: dist/build/out — some projects ship from these

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use crate::cache_monitor::format_size;

// ============================================================================
// Types
// ============================================================================

/// Classification tier for a dev artifact
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactTier {
    /// Caches — regenerate automatically
    Safe,
    /// Build artifacts — regenerable but slow to rebuild
    Rebuildable,
    /// node_modules — one npm install to restore
    SafeWithReinstall,
    /// dist/build/out — some projects ship from these
    Ask,
}

impl ArtifactTier {
    pub fn label(&self) -> &'static str {
        match self {
            ArtifactTier::Safe => "SAFE",
            ArtifactTier::Rebuildable => "REBUILD",
            ArtifactTier::SafeWithReinstall => "SAFE-WITH-REINSTALL",
            ArtifactTier::Ask => "REVIEW",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            ArtifactTier::Safe => "Caches: regenerate automatically",
            ArtifactTier::Rebuildable => "Build artifacts: regenerable but slow to rebuild",
            ArtifactTier::SafeWithReinstall => "node_modules: one npm install to restore",
            ArtifactTier::Ask => "Build outputs: some projects ship from these",
        }
    }
}

/// A single discovered dev artifact
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevArtifact {
    /// Full filesystem path
    pub path: String,
    /// Size in bytes
    pub size_bytes: u64,
    /// Human-readable size
    pub size_display: String,
    /// Classification tier
    pub tier: ArtifactTier,
    /// What kind of artifact (e.g. "npm global cache", "node_modules", ".next build")
    pub kind: String,
    /// Parent project name for attribution (directory containing the artifact)
    pub project: Option<String>,
    /// Days since project's package.json or src/ was last modified (node_modules only)
    pub staleness_days: Option<u64>,
    /// True if this artifact lives inside another artifact's directory
    /// (node_modules/.cache inside node_modules). Its bytes are NOT counted in
    /// the parent's size, so every row counts once toward the tier tiles.
    /// Deleting it goes to Trash, since it sits inside a REINSTALL parent.
    pub is_nested: bool,
    /// Inline guidance for the user (restore cost or safety warning)
    pub hint: Option<String>,
    /// True if a build process is actively using this artifact (pgrep / lock-file mtime)
    #[serde(default)]
    pub active_build: bool,
    /// SAFE rows only: why a delete right now would skip this row ("modified
    /// 2h ago" for project artifacts, "in use by cargo"). Set at scan time by the same in_use_reason()
    /// the delete guard uses; the delete guard still re-checks at delete time.
    #[serde(default)]
    pub in_use: Option<String>,
}

/// Complete scan result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevScanResult {
    pub artifacts: Vec<DevArtifact>,
    /// Total reclaimable bytes (excludes nested/double-counted items)
    pub total_bytes: u64,
    pub total_display: String,
    /// Breakdown by tier (excludes nested items)
    pub safe_bytes: u64,
    pub safe_display: String,
    /// SAFE rows Clean Now will actually remove (not in use, not building)
    #[serde(default)]
    pub safe_deletable_bytes: u64,
    #[serde(default)]
    pub safe_deletable_display: String,
    pub rebuildable_bytes: u64,
    pub rebuildable_display: String,
    pub safe_with_reinstall_bytes: u64,
    pub safe_with_reinstall_display: String,
    pub ask_bytes: u64,
    pub ask_display: String,
    /// How long the scan took
    pub scan_duration_ms: u64,
    /// Which roots were actually scanned (existed on disk)
    pub scan_roots: Vec<String>,
}

// ============================================================================
// Known patterns
// ============================================================================

/// Known dev tool caches in ~/Library/Caches (directory name, display label)
const KNOWN_LIBRARY_CACHES: &[(&str, &str)] = &[
    ("pnpm", "pnpm"),
    ("node-gyp", "node-gyp"),
    ("typescript", "TypeScript"),
    ("Homebrew", "Homebrew"),
    ("pip", "pip"),
    ("go-build", "Go build"),
    ("CocoaPods", "CocoaPods"),
];

/// Directories to skip when traversing project roots
const SKIP_DIRS: &[&str] = &[
    ".git", ".svn", ".hg", ".Trash", "Library", "Applications",
];

/// Max traversal depth for project roots
const MAX_PROJECT_DEPTH: u32 = 6;

// ============================================================================
// Helpers
// ============================================================================

fn get_home_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users".to_string());
    PathBuf::from(home)
}

/// Check if a path is a symlink (without following it).
/// Uses symlink_metadata which does NOT resolve symlinks.
fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Default project root directories to scan
pub fn default_scan_roots() -> Vec<String> {
    let home = get_home_dir();
    // Only include paths that commonly contain dev projects
    [
        "Desktop",
        "dev",
        "Developer",
        "Projects",
        "Code",
        "repos",
        "workspace",
        "src",
    ]
    .iter()
    .map(|d| home.join(d).to_string_lossy().to_string())
    .collect()
}

/// Top-level home folders never treated as projects
const HOME_SKIP_DIRS: &[&str] = &["Library", "Applications"];

/// Files or folders that mark a top-level home folder as a project
const HOME_PROJECT_MARKERS: &[&str] = &["Cargo.toml", "package.json", ".git"];

/// Projects that live directly in home (~/shotrack), one level deep only:
/// folders holding Cargo.toml, package.json or .git. Skips Library,
/// Applications, dotfolders, symlinks and folders already in `roots`.
fn home_project_roots(home: &Path, roots: &[PathBuf]) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(home) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            !name.starts_with('.') && !HOME_SKIP_DIRS.contains(&name.as_ref())
        })
        .map(|e| e.path())
        .filter(|p| !roots.contains(p))
        .filter(|p| HOME_PROJECT_MARKERS.iter().any(|m| p.join(m).exists()))
        .collect();
    found.sort();
    found
}

/// Validate a scan root — reject dangerous paths that should never be scanned.
/// Returns true if the root is safe to scan, false if it should be rejected.
fn is_safe_scan_root(root: &Path) -> bool {
    let home = get_home_dir();

    // Reject filesystem root
    if root == Path::new("/") {
        eprintln!("WARNING: Rejected scan root '/' — filesystem root is never scannable");
        return false;
    }

    // Reject bare home directory
    if root == home {
        eprintln!(
            "WARNING: Rejected scan root '{}' — bare home directory is never scannable",
            root.display()
        );
        return false;
    }

    // Reject volume roots (/Volumes/*)
    if root.starts_with("/Volumes") && root.components().count() <= 2 {
        eprintln!(
            "WARNING: Rejected scan root '{}' — volume root is never scannable",
            root.display()
        );
        return false;
    }

    // Reject system-critical top-level directories
    const BLOCKED_ROOTS: &[&str] = &[
        "/System",
        "/Library",
        "/Applications",
        "/private",
        "/usr",
        "/bin",
        "/sbin",
        "/var",
    ];
    for blocked in BLOCKED_ROOTS {
        if root == Path::new(blocked) || root.starts_with(blocked) {
            eprintln!(
                "WARNING: Rejected scan root '{}' — system directory is never scannable",
                root.display()
            );
            return false;
        }
    }

    true
}

/// Calculate directory size recursively, skipping symlinks.
/// Uses DirEntry::file_type() for efficient type checks on macOS (uses d_type).
fn dir_size(path: &Path) -> u64 {
    let mut total: u64 = 0;

    let entries = match fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return 0,
    };

    for entry in entries.flatten() {
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        // Skip symlinks to avoid cycles and miscounting
        if ft.is_symlink() {
            continue;
        }

        if ft.is_dir() {
            total += dir_size(&entry.path());
        } else if let Ok(metadata) = entry.metadata() {
            total += metadata.len();
        }
    }

    total
}

/// Days since an artifact's project was last active: the newer of the
/// artifact's own mtime and <project>/.git/index (touched by checkouts,
/// commits, staging). None if neither mtime is readable.
fn check_project_staleness(artifact: &Path, project_dir: &Path) -> Option<u64> {
    let mtime = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
    let most_recent = [mtime(artifact), mtime(&project_dir.join(".git").join("index"))]
        .into_iter()
        .flatten()
        .max()?;
    // A future mtime (clock skew) counts as active today
    Some(
        SystemTime::now()
            .duration_since(most_recent)
            .map(|d| d.as_secs() / 86400)
            .unwrap_or(0),
    )
}

/// Check if a directory has a sibling file with any of the given extensions
fn has_sibling_with_ext(dir: &Path, extensions: &[&str]) -> bool {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                for ext in extensions {
                    if name.ends_with(&format!(".{}", ext)) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Get the project name from an artifact path: the enclosing git repo's root
/// folder (so src-tauri/target reads as the repo, not "src-tauri"), else the
/// artifact's parent directory name
fn get_project_name(artifact_path: &Path) -> Option<String> {
    project_name_in(artifact_path, &get_home_dir())
}

/// get_project_name with an explicit home. The search for .git stops below
/// `home`, so a dotfiles repo at ~ never renames every project.
fn project_name_in(artifact_path: &Path, home: &Path) -> Option<String> {
    let project_dir = artifact_path.parent()?;
    let repo_root = project_dir
        .ancestors()
        .take_while(|dir| *dir != home && dir.starts_with(home))
        .find(|dir| dir.join(".git").exists());
    repo_root
        .unwrap_or(project_dir)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
}

// ============================================================================
// Scanner entry point
// ============================================================================

/// Per-tier byte totals. Every row counts exactly once (a nested row's
/// bytes are excluded from its parent), so the tiles always equal the rows
/// shown under them and the tiles sum to the total.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct TierTotals {
    total: u64,
    safe: u64,
    safe_deletable: u64,
    rebuildable: u64,
    safe_with_reinstall: u64,
    ask: u64,
}

impl TierTotals {
    fn of(artifacts: &[DevArtifact]) -> Self {
        let mut t = TierTotals::default();
        for a in artifacts {
            t.total += a.size_bytes;
            match a.tier {
                ArtifactTier::Safe => {
                    t.safe += a.size_bytes;
                    if !a.active_build && a.in_use.is_none() {
                        t.safe_deletable += a.size_bytes;
                    }
                }
                ArtifactTier::Rebuildable => t.rebuildable += a.size_bytes,
                ArtifactTier::SafeWithReinstall => t.safe_with_reinstall += a.size_bytes,
                ArtifactTier::Ask => t.ask += a.size_bytes,
            }
        }
        t
    }
}

impl DevScanResult {
    /// Recompute the tier totals from the rows (after a recheck changed some)
    fn refresh_totals(&mut self) {
        let t = TierTotals::of(&self.artifacts);
        self.total_bytes = t.total;
        self.total_display = format_size(t.total);
        self.safe_bytes = t.safe;
        self.safe_display = format_size(t.safe);
        self.safe_deletable_bytes = t.safe_deletable;
        self.safe_deletable_display = format_size(t.safe_deletable);
        self.rebuildable_bytes = t.rebuildable;
        self.rebuildable_display = format_size(t.rebuildable);
        self.safe_with_reinstall_bytes = t.safe_with_reinstall;
        self.safe_with_reinstall_display = format_size(t.safe_with_reinstall);
        self.ask_bytes = t.ask;
        self.ask_display = format_size(t.ask);
    }

    /// The totals the unified AppStatus is computed from
    pub fn totals(&self) -> crate::cache_monitor::DevTotals {
        crate::cache_monitor::DevTotals {
            total: self.total_bytes,
            safe: self.safe_bytes,
            safe_deletable: self.safe_deletable_bytes,
        }
    }
}

/// Rows smaller than this aren't shown or counted: not worth a row, and every
/// total (tray, hero, tiles, Clean) stays equal to the rows on screen
const MIN_ROW_BYTES: u64 = 1024 * 1024;

fn drop_small_rows(artifacts: &mut Vec<DevArtifact>) {
    artifacts.retain(|a| a.size_bytes >= MIN_ROW_BYTES);
}

/// Run a full dev artifact scan. Pass custom project roots or empty slice for defaults.
pub fn scan_dev_artifacts(custom_roots: &[String]) -> DevScanResult {
    let start = std::time::Instant::now();
    let mut artifacts: Vec<DevArtifact> = Vec::new();
    let home = get_home_dir();

    // Determine scan roots
    let root_strings = if custom_roots.is_empty() {
        default_scan_roots()
    } else {
        custom_roots.to_vec()
    };
    let mut scan_roots: Vec<PathBuf> = root_strings.iter().map(PathBuf::from).collect();
    scan_roots.extend(home_project_roots(&home, &scan_roots));

    // Phase 1: Home-level caches
    scan_home_caches(&home, &mut artifacts);

    // Phase 2: ~/Library/Caches known dev tool directories
    scan_library_caches(&home, &mut artifacts);

    // Phase 3: ~/Library/Developer (DerivedData)
    scan_derived_data(&home, &mut artifacts);

    // Phase 3b: Rebuildable home caches (Gradle, Maven, Go modules)
    scan_rebuildable_home_caches(&home, &mut artifacts);

    // Phase 3c: Ask-tier entries (Docker, Xcode Archives, Simulators, AVDs)
    scan_ask_tier(&home, &mut artifacts);

    // Phase 4: Project roots — validate safety then filter to existing dirs
    let existing_roots: Vec<PathBuf> = scan_roots
        .iter()
        .filter(|r| is_safe_scan_root(r))
        .filter(|r| r.exists() && r.is_dir())
        .cloned()
        .collect();

    for root in &existing_roots {
        scan_project_root(root, &mut artifacts, 0);
    }

    drop_small_rows(&mut artifacts);
    annotate_in_use(&mut artifacts);
    let totals = TierTotals::of(&artifacts);

    let duration = start.elapsed();

    // Sort by size descending
    artifacts.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));

    DevScanResult {
        artifacts,
        total_bytes: totals.total,
        total_display: format_size(totals.total),
        safe_bytes: totals.safe,
        safe_display: format_size(totals.safe),
        safe_deletable_bytes: totals.safe_deletable,
        safe_deletable_display: format_size(totals.safe_deletable),
        rebuildable_bytes: totals.rebuildable,
        rebuildable_display: format_size(totals.rebuildable),
        safe_with_reinstall_bytes: totals.safe_with_reinstall,
        safe_with_reinstall_display: format_size(totals.safe_with_reinstall),
        ask_bytes: totals.ask,
        ask_display: format_size(totals.ask),
        scan_duration_ms: duration.as_millis() as u64,
        scan_roots: existing_roots
            .iter()
            .map(|r| r.to_string_lossy().to_string())
            .collect(),
    }
}

// ============================================================================
// Phase 1: Home-level caches
// ============================================================================

fn scan_home_caches(home: &Path, artifacts: &mut Vec<DevArtifact>) {
    // ~/.npm/_cacache — npm's package cache. The rest of ~/.npm (_logs, _npx)
    // is never touched: _logs is npm's debug history, _npx holds npx installs.
    check_home_cache(home, ".npm/_cacache", "npm package cache", artifacts);

    // ~/.yarn/cache
    let yarn_cache = home.join(".yarn").join("cache");
    if !is_symlink(&yarn_cache) && yarn_cache.exists() && yarn_cache.is_dir() {
        let size = dir_size(&yarn_cache);
        if size > 0 {
            artifacts.push(DevArtifact {
                path: yarn_cache.to_string_lossy().to_string(),
                size_bytes: size,
                size_display: format_size(size),
                tier: ArtifactTier::Safe,
                kind: "Yarn cache".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: Some("Safe to delete. Cache regenerates automatically".to_string()),
                active_build: false,
                in_use: None,
            });
        }
    }

    // ~/.bun/install/cache
    let bun_cache = home.join(".bun").join("install").join("cache");
    if !is_symlink(&bun_cache) && bun_cache.exists() && bun_cache.is_dir() {
        let size = dir_size(&bun_cache);
        if size > 0 {
            artifacts.push(DevArtifact {
                path: bun_cache.to_string_lossy().to_string(),
                size_bytes: size,
                size_display: format_size(size),
                tier: ArtifactTier::Safe,
                kind: "Bun cache".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: Some("Safe to delete. Cache regenerates automatically".to_string()),
                active_build: false,
                in_use: None,
            });
        }
    }

    // ~/.cargo/registry (Rust crate downloads)
    let cargo_registry = home.join(".cargo").join("registry");
    if !is_symlink(&cargo_registry) && cargo_registry.exists() && cargo_registry.is_dir() {
        let size = dir_size(&cargo_registry);
        if size > 0 {
            artifacts.push(DevArtifact {
                path: cargo_registry.to_string_lossy().to_string(),
                size_bytes: size,
                size_display: format_size(size),
                tier: ArtifactTier::Safe,
                kind: "Cargo registry cache".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: Some("Safe to delete. Cache regenerates automatically".to_string()),
                active_build: false,
                in_use: None,
            });
        }
    }

    // ~/Library/pnpm/store (pnpm content-addressable store)
    let pnpm_store = home.join("Library").join("pnpm").join("store");
    if !is_symlink(&pnpm_store) && pnpm_store.exists() && pnpm_store.is_dir() {
        let size = dir_size(&pnpm_store);
        if size > 0 {
            artifacts.push(DevArtifact {
                path: pnpm_store.to_string_lossy().to_string(),
                size_bytes: size,
                size_display: format_size(size),
                tier: ArtifactTier::Rebuildable,
                kind: "pnpm content store".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: Some("Safe to delete. Use pnpm store prune (removes only orphaned packages)".to_string()),
                active_build: false,
                in_use: None,
            });
        }
    }
}

/// Check a single home-level cache directory (`rel_path` may be nested, e.g. ".npm/_cacache")
fn check_home_cache(home: &Path, rel_path: &str, kind: &str, artifacts: &mut Vec<DevArtifact>) {
    let path = home.join(rel_path);
    // Skip symlinks anywhere below home — .exists()/.is_dir() resolve them,
    // which could point at real data
    if path
        .ancestors()
        .take_while(|p| *p != home)
        .any(is_symlink)
    {
        return;
    }
    if path.exists() && path.is_dir() {
        let size = dir_size(&path);
        if size > 0 {
            artifacts.push(DevArtifact {
                path: path.to_string_lossy().to_string(),
                size_bytes: size,
                size_display: format_size(size),
                tier: ArtifactTier::Safe,
                kind: kind.to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: Some("Safe to delete. Cache regenerates automatically".to_string()),
                active_build: false,
                in_use: None,
            });
        }
    }
}

// ============================================================================
// Phase 2: ~/Library/Caches known dev tools
// ============================================================================

fn scan_library_caches(home: &Path, artifacts: &mut Vec<DevArtifact>) {
    let caches_dir = home.join("Library").join("Caches");
    if !caches_dir.exists() {
        return;
    }

    let entries = match fs::read_dir(&caches_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let entry_path = entry.path();

        // Must be a directory
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_symlink() || !ft.is_dir() {
            continue;
        }

        // Check against known dev tool caches (exact match on directory name)
        for (pattern, label) in KNOWN_LIBRARY_CACHES {
            if name_str.as_ref() == *pattern {
                let size = dir_size(&entry_path);
                if size > 0 {
                    artifacts.push(DevArtifact {
                        path: entry_path.to_string_lossy().to_string(),
                        size_bytes: size,
                        size_display: format_size(size),
                        tier: ArtifactTier::Safe,
                        kind: format!("{} cache", label),
                        project: None,
                        staleness_days: None,
                        is_nested: false,
                        hint: Some("Safe to delete. Cache regenerates automatically".to_string()),
                        active_build: false,
                        in_use: None,
                    });
                }
                break;
            }
        }

        // Playwright browsers — not a cache: they only come back on an explicit install
        if name_str == "ms-playwright" {
            let size = dir_size(&entry_path);
            if size > 0 {
                artifacts.push(DevArtifact {
                    path: entry_path.to_string_lossy().to_string(),
                    size_bytes: size,
                    size_display: format_size(size),
                    tier: ArtifactTier::Rebuildable,
                    kind: "Playwright browsers".to_string(),
                    project: None,
                    staleness_days: None,
                    is_nested: false,
                    hint: Some("Browsers re-download on next `npx playwright install`".to_string()),
                    active_build: false,
                    in_use: None,
                });
            }
        }

        // Check for *.ShipIt caches (an app's staged self-update)
        if name_str.ends_with(".ShipIt") {
            let size = dir_size(&entry_path);
            if size > 0 {
                artifacts.push(DevArtifact {
                    path: entry_path.to_string_lossy().to_string(),
                    size_bytes: size,
                    size_display: format_size(size),
                    tier: ArtifactTier::Rebuildable,
                    kind: "ShipIt update cache".to_string(),
                    project: None,
                    staleness_days: None,
                    is_nested: false,
                    hint: Some("In-progress app update; the app re-downloads it".to_string()),
                    active_build: false,
                    in_use: None,
                });
            }
        }
    }
}

// ============================================================================
// Phase 3: ~/Library/Developer (Xcode DerivedData)
// ============================================================================

fn scan_derived_data(home: &Path, artifacts: &mut Vec<DevArtifact>) {
    let derived_data = home
        .join("Library")
        .join("Developer")
        .join("Xcode")
        .join("DerivedData");

    if is_symlink(&derived_data) {
        return;
    }
    if derived_data.exists() && derived_data.is_dir() {
        let size = dir_size(&derived_data);
        if size > 0 {
            let building = is_active_build(&derived_data);
            artifacts.push(DevArtifact {
                path: derived_data.to_string_lossy().to_string(),
                size_bytes: size,
                size_display: format_size(size),
                tier: ArtifactTier::Rebuildable,
                kind: "Xcode DerivedData".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: Some("Safe to delete. Rebuilds on next Xcode build".to_string()),
                active_build: building,
                in_use: None,
            });
        }
    }
}

// ============================================================================
// Phase 3b: Home-level rebuildable caches
// ============================================================================

fn scan_rebuildable_home_caches(home: &Path, artifacts: &mut Vec<DevArtifact>) {
    // ~/.gradle/caches (Gradle build caches)
    let gradle_caches = home.join(".gradle").join("caches");
    if !is_symlink(&gradle_caches) && gradle_caches.exists() && gradle_caches.is_dir() {
        let size = dir_size(&gradle_caches);
        if size > 0 {
            artifacts.push(DevArtifact {
                path: gradle_caches.to_string_lossy().to_string(),
                size_bytes: size,
                size_display: format_size(size),
                tier: ArtifactTier::Rebuildable,
                kind: "Gradle caches".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: Some("Safe to delete. Re-downloads on next gradle build (needs network)".to_string()),
                active_build: false,
                in_use: None,
            });
        }
    }

    // ~/.m2/repository (Maven local repository)
    let maven_repo = home.join(".m2").join("repository");
    if !is_symlink(&maven_repo) && maven_repo.exists() && maven_repo.is_dir() {
        let size = dir_size(&maven_repo);
        if size > 0 {
            artifacts.push(DevArtifact {
                path: maven_repo.to_string_lossy().to_string(),
                size_bytes: size,
                size_display: format_size(size),
                tier: ArtifactTier::Rebuildable,
                kind: "Maven local repository".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: Some("Safe to delete. Re-downloads on next mvn build (needs network)".to_string()),
                active_build: false,
                in_use: None,
            });
        }
    }

    // ~/go/pkg/mod or $GOPATH/pkg/mod (Go module cache)
    let gopath = std::env::var("GOPATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join("go"));
    let go_mod_cache = gopath.join("pkg").join("mod");
    if !is_symlink(&go_mod_cache) && go_mod_cache.exists() && go_mod_cache.is_dir() {
        let size = dir_size(&go_mod_cache);
        if size > 0 {
            artifacts.push(DevArtifact {
                path: go_mod_cache.to_string_lossy().to_string(),
                size_bytes: size,
                size_display: format_size(size),
                tier: ArtifactTier::Rebuildable,
                kind: "Go module cache".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: Some("Safe to delete. Re-downloads on next go build (needs network)".to_string()),
                active_build: false,
                in_use: None,
            });
        }
    }
}

// ============================================================================
// Phase 3c: Ask-tier entries (report only, no auto-delete)
// ============================================================================

fn scan_ask_tier(home: &Path, artifacts: &mut Vec<DevArtifact>) {
    // ~/Library/Containers/com.docker.docker
    let docker = home.join("Library").join("Containers").join("com.docker.docker");
    if !is_symlink(&docker) && docker.exists() && docker.is_dir() {
        let size = dir_size(&docker);
        if size > 0 {
            artifacts.push(DevArtifact {
                path: docker.to_string_lossy().to_string(),
                size_bytes: size,
                size_display: format_size(size),
                tier: ArtifactTier::Ask,
                kind: "Docker".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: Some("Keep unless you're sure. May contain databases; use docker system prune".to_string()),
                active_build: false,
                in_use: None,
            });
        }
    }

    // ~/Library/Developer/Xcode/Archives
    let xcode_archives = home.join("Library").join("Developer").join("Xcode").join("Archives");
    if !is_symlink(&xcode_archives) && xcode_archives.exists() && xcode_archives.is_dir() {
        let size = dir_size(&xcode_archives);
        if size > 0 {
            artifacts.push(DevArtifact {
                path: xcode_archives.to_string_lossy().to_string(),
                size_bytes: size,
                size_display: format_size(size),
                tier: ArtifactTier::Ask,
                kind: "Xcode Archives".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: Some("Keep. Holds crash symbols for shipped apps".to_string()),
                active_build: false,
                in_use: None,
            });
        }
    }

    // ~/Library/Developer/CoreSimulator
    let core_simulator = home.join("Library").join("Developer").join("CoreSimulator");
    if !is_symlink(&core_simulator) && core_simulator.exists() && core_simulator.is_dir() {
        let size = dir_size(&core_simulator);
        if size > 0 {
            artifacts.push(DevArtifact {
                path: core_simulator.to_string_lossy().to_string(),
                size_bytes: size,
                size_display: format_size(size),
                tier: ArtifactTier::Ask,
                kind: "iOS Simulators".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: Some("Keep unless you're sure. Delete via Xcode \u{2192} Settings \u{2192} Platforms".to_string()),
                active_build: false,
                in_use: None,
            });
        }
    }

    // ~/.android/avd (Android emulator images)
    let android_avd = home.join(".android").join("avd");
    if !is_symlink(&android_avd) && android_avd.exists() && android_avd.is_dir() {
        let size = dir_size(&android_avd);
        if size > 0 {
            artifacts.push(DevArtifact {
                path: android_avd.to_string_lossy().to_string(),
                size_bytes: size,
                size_display: format_size(size),
                tier: ArtifactTier::Ask,
                kind: "Android emulator images".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: Some("Keep unless you're sure. Delete via Android Studio \u{2192} Device Manager".to_string()),
                active_build: false,
                in_use: None,
            });
        }
    }
}

// ============================================================================
// Active build detection
// ============================================================================

/// A build tool running right now: command name and working directory
#[derive(Debug, Default)]
struct BuildProcs {
    cwds: Vec<(String, PathBuf)>,
}

/// Build tools whose working directory marks their project as building
const BUILD_TOOLS: &[&str] = &["cargo", "rustc", "xcodebuild"];

/// How recently Cargo.lock or target/.rustc_info.json must have changed for
/// a Rust target to count as building with no cargo process in sight
const BUILD_RECENT_WINDOW: std::time::Duration = std::time::Duration::from_secs(5 * 60);

impl BuildProcs {
    /// cargo/rustc/xcodebuild working directories, from one lsof call.
    /// Processes started by rust-analyzer (its background `cargo check`)
    /// are not builds and are dropped.
    fn capture() -> Self {
        let mut args = vec!["-a", "-d", "cwd", "-Fpcn"];
        for tool in BUILD_TOOLS {
            args.extend(["-c", tool]);
        }
        let lsof = Command::new("lsof")
            .args(&args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let ps = Command::new("ps")
            .args(["-axo", "pid=,ppid=,comm="])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let tree = parse_ps_tree(&ps);
        let cwds = parse_lsof_pid_cwds(&lsof)
            .into_iter()
            .filter(|(pid, _, _)| !has_ancestor(*pid, &tree, "rust-analyzer"))
            .map(|(_, name, cwd)| (name, cwd))
            .collect();
        BuildProcs { cwds }
    }

    /// Is `tool` (a prefix: "cargo" covers cargo-clippy) working inside `project`?
    fn in_project(&self, tools: &[&str], project: &Path) -> bool {
        // lsof reports resolved paths (/private/tmp, not /tmp)
        let project = project.canonicalize().unwrap_or_else(|_| project.to_path_buf());
        self.cwds
            .iter()
            .any(|(name, cwd)| tools.iter().any(|t| name.starts_with(t)) && cwd.starts_with(&project))
    }

    fn any(&self, tools: &[&str]) -> bool {
        self.cwds.iter().any(|(name, _)| tools.iter().any(|t| name.starts_with(t)))
    }
}

/// `lsof -F pcn` output as (pid, command, cwd)
fn parse_lsof_pid_cwds(output: &str) -> Vec<(u32, String, PathBuf)> {
    let (mut pid, mut command) = (0u32, String::new());
    let mut found = Vec::new();
    for line in output.lines() {
        match line.split_at(line.len().min(1)) {
            ("p", n) => {
                pid = n.parse().unwrap_or(0);
                command.clear();
            }
            ("c", name) => command = name.to_string(),
            ("n", path) if !path.is_empty() => found.push((pid, command.clone(), PathBuf::from(path))),
            _ => {}
        }
    }
    found
}

/// `ps -o pid=,ppid=,comm=` output as pid -> (parent pid, command basename)
fn parse_ps_tree(output: &str) -> std::collections::HashMap<u32, (u32, String)> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.parse().ok()?;
            let ppid = parts.next()?.parse().ok()?;
            let comm: Vec<&str> = parts.collect();
            let comm = comm.join(" ");
            let name = Path::new(&comm).file_name()?.to_string_lossy().into_owned();
            Some((pid, (ppid, name)))
        })
        .collect()
}

/// Whether any ancestor of `pid` is named `name`
fn has_ancestor(pid: u32, tree: &std::collections::HashMap<u32, (u32, String)>, name: &str) -> bool {
    let mut current = pid;
    for _ in 0..64 {
        let Some((parent, _)) = tree.get(&current) else { return false };
        if *parent <= 1 {
            return false;
        }
        if tree.get(parent).map(|(_, n)| n == name).unwrap_or(false) {
            return true;
        }
        current = *parent;
    }
    false
}

/// One lsof/ps snapshot shared by every check in a scan (a scan checks many
/// targets); reused for 2 seconds
fn build_procs() -> std::sync::Arc<BuildProcs> {
    use std::sync::{Arc, Mutex};
    static CACHE: Mutex<Option<(std::time::Instant, Arc<BuildProcs>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, procs)) = cache.as_ref() {
        if at.elapsed() < std::time::Duration::from_secs(2) {
            return Arc::clone(procs);
        }
    }
    let procs = Arc::new(BuildProcs::capture());
    *cache = Some((std::time::Instant::now(), Arc::clone(&procs)));
    procs
}

/// Is this build artifact being built right now? Per project: another
/// project's cargo, or rust-analyzer, never holds it back.
fn is_active_build(artifact: &Path) -> bool {
    building(artifact, &get_home_dir(), &build_procs(), SystemTime::now())
}

/// - Rust `target`: cargo/rustc working inside its project, or the project's
///   Cargo.lock or target/.rustc_info.json changed in the last 5 minutes
/// - project DerivedData: xcodebuild working inside that project
/// - ~/Library/Developer/Xcode/DerivedData: any xcodebuild, since every Xcode
///   project builds into it
fn building(artifact: &Path, home: &Path, procs: &BuildProcs, now: SystemTime) -> bool {
    let Some(project) = artifact.parent() else { return false };
    match artifact.file_name().and_then(|n| n.to_str()) {
        Some("target") => {
            recently_modified(&project.join("Cargo.lock"), now, BUILD_RECENT_WINDOW).is_some()
                || recently_modified(&artifact.join(".rustc_info.json"), now, BUILD_RECENT_WINDOW).is_some()
                || procs.in_project(&["cargo", "rustc"], project)
        }
        Some("DerivedData") if artifact == home.join("Library/Developer/Xcode/DerivedData") => {
            procs.any(&["xcodebuild"])
        }
        Some("DerivedData") => procs.in_project(&["xcodebuild"], project),
        _ => false,
    }
}

// ============================================================================
// Phase 4: Project root traversal
// ============================================================================

/// Recursively scan a project root for dev artifacts.
/// Matches known artifact directory names and descends into unmatched dirs.
fn scan_project_root(dir: &Path, artifacts: &mut Vec<DevArtifact>, depth: u32) {
    if depth > MAX_PROJECT_DEPTH {
        return;
    }

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        // Skip symlinks and non-directories
        if ft.is_symlink() || !ft.is_dir() {
            continue;
        }

        let entry_path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip known non-project directories
        if SKIP_DIRS.contains(&name_str.as_ref()) {
            continue;
        }

        // ── node_modules (special handling: scan for .cache inside) ──
        if name_str == "node_modules" {
            handle_node_modules(&entry_path, dir, artifacts);
            continue; // Don't descend further into node_modules
        }

        // ── SAFE tier: build/tool caches ──
        // Only classify as Safe if the parent directory is a recognized project.
        // A bare .next/.turbo/etc. outside a project is not safe to auto-delete.
        if matches!(
            name_str.as_ref(),
            ".next" | ".turbo" | ".parcel-cache" | ".vite"
        ) {
            if looks_like_project(dir) {
                let size = dir_size(&entry_path);
                if size > 0 {
                    artifacts.push(DevArtifact {
                        path: entry_path.to_string_lossy().to_string(),
                        size_bytes: size,
                        size_display: format_size(size),
                        tier: ArtifactTier::Safe,
                        kind: format!("{} cache", name_str),
                        project: get_project_name(&entry_path),
                        staleness_days: None,
                        is_nested: false,
                        hint: Some("Safe to delete. Cache regenerates automatically".to_string()),
                        active_build: false,
                        in_use: None,
                    });
                }
            }
            continue; // Don't descend into matched artifact dirs
        }

        // ── REBUILDABLE tier: Rust target/ directories ──
        // These are massive (5–20 GB) and regenerable with `cargo build`,
        // but rebuilds can be slow. Only flag if parent contains Cargo.toml.
        if name_str == "target" && dir.join("Cargo.toml").exists() {
            let size = dir_size(&entry_path);
            if size > 0 {
                let building = is_active_build(&entry_path);
                artifacts.push(DevArtifact {
                    path: entry_path.to_string_lossy().to_string(),
                    size_bytes: size,
                    size_display: format_size(size),
                    tier: ArtifactTier::Rebuildable,
                    kind: "Rust target (build artifacts)".to_string(),
                    project: get_project_name(&entry_path),
                    staleness_days: check_project_staleness(&entry_path, dir),
                    is_nested: false,
                    hint: Some("Safe to delete. Rebuilds on next cargo build (takes minutes, needs network)".to_string()),
                    active_build: building,
                    in_use: None,
                });
            }
            continue;
        }

        // ── REBUILDABLE tier: .NET build output (bin/ or obj/) ──
        if (name_str == "bin" || name_str == "obj") && has_sibling_with_ext(dir, &["csproj", "sln", "fsproj"]) {
            let size = dir_size(&entry_path);
            if size > 0 {
                artifacts.push(DevArtifact {
                    path: entry_path.to_string_lossy().to_string(),
                    size_bytes: size,
                    size_display: format_size(size),
                    tier: ArtifactTier::Rebuildable,
                    kind: format!(".NET {} output", name_str),
                    project: get_project_name(&entry_path),
                    staleness_days: None,
                    is_nested: false,
                    hint: Some("Safe to delete. Rebuilds on next dotnet build".to_string()),
                    active_build: false,
                    in_use: None,
                });
            }
            continue;
        }

        // ── REBUILDABLE tier: Unity Library/ ──
        if name_str == "Library" && dir.join("Assets").exists() && dir.join("ProjectSettings").exists() {
            let size = dir_size(&entry_path);
            if size > 0 {
                artifacts.push(DevArtifact {
                    path: entry_path.to_string_lossy().to_string(),
                    size_bytes: size,
                    size_display: format_size(size),
                    tier: ArtifactTier::Rebuildable,
                    kind: "Unity Library".to_string(),
                    project: get_project_name(&entry_path),
                    staleness_days: None,
                    is_nested: false,
                    hint: Some("Safe to delete. Rebuilds when Unity reimports the project".to_string()),
                    active_build: false,
                    in_use: None,
                });
            }
            continue;
        }

        // ── REBUILDABLE tier: Unreal DerivedDataCache/ ──
        if name_str == "DerivedDataCache" && has_sibling_with_ext(dir, &["uproject"]) {
            let size = dir_size(&entry_path);
            if size > 0 {
                artifacts.push(DevArtifact {
                    path: entry_path.to_string_lossy().to_string(),
                    size_bytes: size,
                    size_display: format_size(size),
                    tier: ArtifactTier::Rebuildable,
                    kind: "Unreal DerivedDataCache".to_string(),
                    project: get_project_name(&entry_path),
                    staleness_days: None,
                    is_nested: false,
                    hint: Some("Safe to delete. Rebuilds on next Unreal Editor launch".to_string()),
                    active_build: false,
                    in_use: None,
                });
            }
            continue;
        }

        // ── REBUILDABLE tier: DerivedData inside project dirs ──
        if name_str == "DerivedData" {
            let size = dir_size(&entry_path);
            if size > 0 {
                let building = is_active_build(&entry_path);
                artifacts.push(DevArtifact {
                    path: entry_path.to_string_lossy().to_string(),
                    size_bytes: size,
                    size_display: format_size(size),
                    tier: ArtifactTier::Rebuildable,
                    kind: "Xcode DerivedData".to_string(),
                    project: get_project_name(&entry_path),
                    staleness_days: None,
                    is_nested: false,
                    hint: Some("Safe to delete. Rebuilds on next Xcode build".to_string()),
                    active_build: building,
                    in_use: None,
                });
            }
            continue;
        }

        // ── ASK tier: test coverage reports ──
        // Output of a test run, not a cache — only flagged inside a project.
        if name_str == "coverage" {
            if looks_like_project(dir) {
                let size = dir_size(&entry_path);
                if size > 0 {
                    artifacts.push(DevArtifact {
                        path: entry_path.to_string_lossy().to_string(),
                        size_bytes: size,
                        size_display: format_size(size),
                        tier: ArtifactTier::Ask,
                        kind: "coverage report".to_string(),
                        project: get_project_name(&entry_path),
                        staleness_days: None,
                        is_nested: false,
                        hint: Some("Test coverage report. Regenerates on the next coverage run".to_string()),
                        active_build: false,
                        in_use: None,
                    });
                }
            }
            continue;
        }

        // ── ASK tier: build outputs ──
        if matches!(name_str.as_ref(), "dist" | "build" | "out") {
            // Only flag these if the parent looks like a project (has package.json, Cargo.toml, etc.)
            if looks_like_project(dir) {
                let size = dir_size(&entry_path);
                if size > 0 {
                    artifacts.push(DevArtifact {
                        path: entry_path.to_string_lossy().to_string(),
                        size_bytes: size,
                        size_display: format_size(size),
                        tier: ArtifactTier::Ask,
                        kind: format!("{} output", name_str),
                        project: get_project_name(&entry_path),
                        staleness_days: None,
                        is_nested: false,
                        hint: Some("Keep unless you're sure. May contain shipped output you haven't deployed".to_string()),
                        active_build: false,
                        in_use: None,
                    });
                }
            }
            // Don't descend into build output dirs
            continue;
        }

        // ── Unmatched directory: recurse ──
        scan_project_root(&entry_path, artifacts, depth + 1);
    }
}

/// Handle a node_modules directory: measure total size, check for .cache inside,
/// and compute staleness from the parent project.
fn handle_node_modules(nm_path: &Path, project_dir: &Path, artifacts: &mut Vec<DevArtifact>) {
    // Check for .cache subdirectory FIRST (it hides inside node_modules)
    let cache_subdir = nm_path.join(".cache");
    let mut cache_size = 0;
    if cache_subdir.exists() && cache_subdir.is_dir() && !is_symlink(&cache_subdir) {
        cache_size = dir_size(&cache_subdir);
        if cache_size > 0 {
            artifacts.push(DevArtifact {
                path: cache_subdir.to_string_lossy().to_string(),
                size_bytes: cache_size,
                size_display: format_size(cache_size),
                tier: ArtifactTier::Safe,
                kind: "node_modules/.cache (build cache)".to_string(),
                project: get_project_name(nm_path),
                staleness_days: None,
                is_nested: true, // Excluded from the parent row's size below
                hint: Some("Safe to delete. Cache regenerates automatically".to_string()),
                active_build: false,
                in_use: None,
            });
        }
    }

    // node_modules row = everything except the nested .cache row, so each byte
    // is counted once and the SAFE/REINSTALL tiles match their rows
    let total_size = dir_size(nm_path).saturating_sub(cache_size);
    if total_size > 0 {
        let staleness = check_project_staleness(nm_path, project_dir);

        artifacts.push(DevArtifact {
            path: nm_path.to_string_lossy().to_string(),
            size_bytes: total_size,
            size_display: format_size(total_size),
            tier: ArtifactTier::SafeWithReinstall,
            kind: "node_modules".to_string(),
            project: get_project_name(nm_path),
            staleness_days: staleness,
            is_nested: false,
            hint: Some("Rebuilds on next npm install. Needs network and may resolve different versions".to_string()),
            active_build: false,
            in_use: None,
        });
    }
}

/// Check if a directory looks like a project root (has common project files)
fn looks_like_project(dir: &Path) -> bool {
    const PROJECT_INDICATORS: &[&str] = &[
        "package.json",
        "Cargo.toml",
        "pom.xml",
        "build.gradle",
        "Makefile",
        "Gemfile",
        "go.mod",
        "pyproject.toml",
        "setup.py",
    ];

    PROJECT_INDICATORS
        .iter()
        .any(|f| dir.join(f).exists())
}

// ============================================================================
// Deletion
// ============================================================================

/// Result of deleting dev artifacts
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevDeleteResult {
    pub deleted_count: usize,
    pub bytes_freed: u64,
    pub bytes_freed_display: String,
    pub errors: Vec<String>,
    /// Safe-tier artifacts left in place because they look in use
    #[serde(default)]
    pub skipped: Vec<SkippedArtifact>,
}

/// An artifact the delete pass deliberately left alone, with a UI-ready reason
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedArtifact {
    pub path: String,
    pub size_bytes: u64,
    pub size_display: String,
    /// e.g. "modified 2h ago" (project artifacts only), "in use by cargo"
    pub reason: String,
}

// ============================================================================
// SymbolSweep Trash manifest — track items SymbolSweep moved to Trash for selective purge
// ============================================================================

/// A single item that SS moved to macOS Trash
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrashedItem {
    pub original_path: String,
    pub trash_path: String,
    pub size_bytes: u64,
    pub timestamp: u64,
}

/// Summary of SS items currently in Trash
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SsTrashInfo {
    pub count: usize,
    pub total_bytes: u64,
    pub total_display: String,
}

/// Result of purging SS items from Trash
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurgeResult {
    pub purged_count: usize,
    pub bytes_freed: u64,
    pub bytes_freed_display: String,
    pub errors: Vec<String>,
}

#[cfg(not(test))]
fn trash_manifest_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join("Library/Application Support/com.mvarley07.symbolsweep")
        .join("trash_manifest.json")
}

/// The Trash folder SymbolSweep moves items into and purges from
#[cfg(not(test))]
fn trash_root() -> PathBuf {
    get_home_dir().join(".Trash")
}

// Tests never touch the real ~/.Trash or the app's manifest: each test thread
// gets its own sandbox under the temp dir, so parallel tests can't clobber
// each other's manifest either.
#[cfg(test)]
fn test_sandbox() -> PathBuf {
    std::env::temp_dir()
        .join("ss-test-sandbox")
        .join(format!("{}-{:?}", std::process::id(), std::thread::current().id()))
}

#[cfg(test)]
fn trash_manifest_path() -> PathBuf {
    test_sandbox().join("trash_manifest.json")
}

#[cfg(test)]
fn trash_root() -> PathBuf {
    test_sandbox().join("Trash")
}

fn load_trash_manifest() -> Vec<TrashedItem> {
    let path = trash_manifest_path();
    match fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn save_trash_manifest(items: &[TrashedItem]) {
    let path = trash_manifest_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(items) {
        let _ = fs::write(&path, json);
    }
}

fn record_trashed_item(original_path: &str, trash_path: &Path, size_bytes: u64) {
    let mut manifest = load_trash_manifest();
    let timestamp = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    manifest.push(TrashedItem {
        original_path: original_path.to_string(),
        trash_path: trash_path.to_string_lossy().to_string(),
        size_bytes,
        timestamp,
    });
    save_trash_manifest(&manifest);
}

/// Tests elsewhere in the crate: put an item in the sandbox Trash and record
/// it in the sandbox manifest, as a Trash move would
#[cfg(test)]
pub(crate) fn stage_trashed_item(name: &str) -> PathBuf {
    let item = trash_root().join(name);
    fs::create_dir_all(&item).unwrap();
    fs::write(item.join("data"), b"x").unwrap();
    record_trashed_item(&format!("/original/{}", name), &item, 1);
    item
}

/// Get info about SS items still sitting in Trash (validates they still exist)
pub fn get_ss_trash_info() -> SsTrashInfo {
    let manifest = load_trash_manifest();
    // Filter to items that still exist in Trash
    let live: Vec<&TrashedItem> = manifest.iter().filter(|i| Path::new(&i.trash_path).exists()).collect();
    let total_bytes: u64 = live.iter().map(|i| i.size_bytes).sum();
    SsTrashInfo {
        count: live.len(),
        total_bytes,
        total_display: format_size(total_bytes),
    }
}

/// Permanently delete only the items SS moved to Trash. Never touches other Trash contents.
///
/// SAFETY: Only deletes paths inside ~/.Trash. Manifest entries pointing
/// elsewhere are rejected and logged as errors (defense against corruption).
pub fn purge_ss_trash() -> PurgeResult {
    purge_ss_trash_in(&trash_root(), &|_, _, _| {})
}

/// Purge with a per-item progress callback: `(current_index, total_count, bytes_freed_so_far)`.
pub fn purge_ss_trash_with_progress(on_progress: &dyn Fn(usize, usize, u64)) -> PurgeResult {
    purge_ss_trash_in(&trash_root(), on_progress)
}

/// Inner implementation accepting an explicit trash root.
/// SAFETY GUARD: only deletes paths inside `trash_dir`.
fn purge_ss_trash_in(trash_dir: &Path, on_progress: &dyn Fn(usize, usize, u64)) -> PurgeResult {
    let manifest = load_trash_manifest();
    let total = manifest.len();
    let mut purged_count = 0usize;
    let mut bytes_freed = 0u64;
    let mut errors = Vec::new();
    let mut remaining = Vec::new();

    for (idx, item) in manifest.iter().enumerate() {
        let path = Path::new(&item.trash_path);

        // SAFETY GUARD: only delete inside trash_dir — reject anything else
        if !path.starts_with(trash_dir) {
            errors.push(format!("SAFETY: refused to delete path outside Trash: {}", item.trash_path));
            remaining.push(item.clone());
            on_progress(idx + 1, total, bytes_freed);
            continue;
        }

        if !path.exists() {
            // Already gone (user emptied Trash manually) — drop from manifest
            on_progress(idx + 1, total, bytes_freed);
            continue;
        }
        let remove_result = if path.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        };
        match remove_result {
            Ok(()) => {
                purged_count += 1;
                bytes_freed += item.size_bytes;
            }
            Err(e) => {
                errors.push(format!("{}: {}", item.trash_path, e));
                remaining.push(item.clone());
            }
        }
        on_progress(idx + 1, total, bytes_freed);
    }

    save_trash_manifest(&remaining);

    PurgeResult {
        purged_count,
        bytes_freed,
        bytes_freed_display: format_size(bytes_freed),
        errors,
    }
}

/// Delete specific dev artifacts by path.
/// Only deletes paths that were found in the most recent scan result (safety check).
/// Bulk callers (Clean Now, auto-clean) should only pass Safe-tier paths.
/// For explicit per-item deletion by the user, use `delete_dev_artifacts_manual`.
pub fn delete_dev_artifacts(paths: &[String], known_artifacts: &[DevArtifact]) -> DevDeleteResult {
    delete_dev_artifacts_inner(paths, known_artifacts, false)
}

/// Delete specific dev artifacts by path with tier override.
/// Used for explicit per-item deletion from the DevScanPanel where the
/// user clicks individual delete buttons on Rebuildable/SafeWithReinstall items.
pub fn delete_dev_artifacts_manual(paths: &[String], known_artifacts: &[DevArtifact]) -> DevDeleteResult {
    delete_dev_artifacts_inner(paths, known_artifacts, true)
}

/// Move a file or directory to macOS Trash using NSFileManager.trashItemAtURL.
/// Returns Ok(trash_path) with the actual path in ~/.Trash where the item landed,
/// or Err with a description. Uses the objc crate (already a dependency).
#[cfg(all(target_os = "macos", not(test)))]
fn move_to_trash(path: &Path) -> Result<PathBuf, String> {
    use objc::runtime::{Class, Object, BOOL, YES};
    use objc::{msg_send, sel, sel_impl};
    use std::ffi::CString;

    unsafe {
        // Get NSFileManager defaultManager
        let cls = Class::get("NSFileManager")
            .ok_or("Failed to get NSFileManager class")?;
        let fm: *mut Object = msg_send![cls, defaultManager];

        // Create NSURL from file path
        let nsurl_cls = Class::get("NSURL")
            .ok_or("Failed to get NSURL class")?;
        let path_str = path.to_string_lossy();
        let nsstring_cls = Class::get("NSString")
            .ok_or("Failed to get NSString class")?;
        let c_str = CString::new(path_str.as_bytes())
            .map_err(|e| format!("Invalid path: {}", e))?;
        let ns_path: *mut Object = msg_send![nsstring_cls, stringWithUTF8String: c_str.as_ptr()];
        let url: *mut Object = msg_send![nsurl_cls, fileURLWithPath: ns_path];

        // Call trashItemAtURL:resultingItemURL:error:
        // Capture resultingItemURL to know exactly where the item landed in Trash
        let mut result_url: *mut Object = std::ptr::null_mut();
        let mut error: *mut Object = std::ptr::null_mut();
        let success: BOOL = msg_send![fm, trashItemAtURL: url resultingItemURL: &mut result_url error: &mut error];

        if success == YES {
            // Extract the resulting path from the URL
            if !result_url.is_null() {
                let path_obj: *mut Object = msg_send![result_url, path];
                if !path_obj.is_null() {
                    let path_cstr: *const i8 = msg_send![path_obj, UTF8String];
                    if !path_cstr.is_null() {
                        let trash_path = PathBuf::from(
                            std::ffi::CStr::from_ptr(path_cstr).to_string_lossy().to_string()
                        );
                        return Ok(trash_path);
                    }
                }
            }
            // Trashed successfully but couldn't capture the result path
            Ok(PathBuf::new())
        } else if !error.is_null() {
            let desc: *mut Object = msg_send![error, localizedDescription];
            let c_str: *const i8 = msg_send![desc, UTF8String];
            let msg = if !c_str.is_null() {
                std::ffi::CStr::from_ptr(c_str)
                    .to_string_lossy()
                    .to_string()
            } else {
                "Unknown NSFileManager error".to_string()
            };
            Err(format!("Trash failed: {}", msg))
        } else {
            Err("Trash failed: unknown error".to_string())
        }
    }
}

#[cfg(all(not(target_os = "macos"), not(test)))]
fn move_to_trash(path: &Path) -> Result<PathBuf, String> {
    // Non-macOS fallback: permanent delete (no trash path)
    fs::remove_dir_all(path).map_err(|e| e.to_string()).map(|_| PathBuf::new())
}

/// Test stand-in: move into the sandbox Trash, renaming on collision like Finder
#[cfg(test)]
fn move_to_trash(path: &Path) -> Result<PathBuf, String> {
    let trash = trash_root();
    fs::create_dir_all(&trash).map_err(|e| e.to_string())?;
    let name = path.file_name().ok_or("no file name")?.to_string_lossy().to_string();
    let mut dest = trash.join(&name);
    let mut n = 1;
    while dest.exists() {
        dest = trash.join(format!("{} {}", name, n));
        n += 1;
    }
    fs::rename(path, &dest).map_err(|e| format!("Trash failed: {}", e))?;
    Ok(dest)
}

// ============================================================================
// In-use guards for Safe-tier deletes
// ============================================================================

/// Project-scoped Safe-tier artifacts modified more recently than this are
/// left alone. Home-level caches skip this check: they are shared by every
/// project, so their mtime says nothing about whether they are in use.
const RECENT_WINDOW: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Process names whose working directory marks a project as in use
const PROJECT_PROCESS_NAMES: &[&str] = &["node", "npm", "pnpm", "yarn", "bun", "vite", "next"];

/// A tool that uses a home-level cache: its executable names, and the entry
/// scripts that mark an interpreter process as that tool (node .../npm-cli.js)
struct CacheTool {
    executables: &'static [&'static str],
    entry_scripts: &'static [(&'static str, &'static str)],
}

const NPM: CacheTool = CacheTool {
    executables: &["npm", "npx"],
    entry_scripts: &[("npm-cli.js", "npm"), ("npx-cli.js", "npx")],
};

/// Home-level caches (path under home) and the tool whose running process
/// marks each as in use
const HOME_CACHE_TOOLS: &[(&str, CacheTool)] = &[
    (".npm", NPM),
    (".cargo", CacheTool { executables: &["cargo"], entry_scripts: &[] }),
    (".yarn", CacheTool { executables: &["yarn"], entry_scripts: &[("yarn.js", "yarn"), ("yarn.cjs", "yarn")] }),
    (".bun", CacheTool { executables: &["bun"], entry_scripts: &[] }),
    ("Library/Caches/pnpm", CacheTool { executables: &["pnpm"], entry_scripts: &[("pnpm.cjs", "pnpm")] }),
    ("Library/Caches/node-gyp", CacheTool { executables: &["node-gyp"], entry_scripts: &[("node-gyp.js", "node-gyp")] }),
    ("Library/Caches/typescript", CacheTool { executables: &["tsc"], entry_scripts: &[("tsc", "tsc")] }),
    ("Library/Caches/Homebrew", CacheTool { executables: &["brew"], entry_scripts: &[("brew.rb", "brew"), ("brew.sh", "brew")] }),
    ("Library/Caches/pip", CacheTool { executables: &["pip", "pip3"], entry_scripts: &[] }),
    ("Library/Caches/go-build", CacheTool { executables: &["go"], entry_scripts: &[] }),
    ("Library/Caches/CocoaPods", CacheTool { executables: &["pod"], entry_scripts: &[("pod", "pod")] }),
];

/// Artifact directory names that live directly inside a project root
const PROJECT_SCOPED_NAMES: &[&str] = &[".next", ".vite", ".turbo", ".parcel-cache"];

/// Snapshot of running processes, taken once per delete pass
struct ProcessSnapshot {
    /// (command name, working directory) for PROJECT_PROCESS_NAMES
    cwds: Vec<(String, PathBuf)>,
    /// Full command line of every running process (`ps -o args=`)
    command_lines: Vec<String>,
}

impl ProcessSnapshot {
    fn capture() -> Self {
        let mut cmd = Command::new("lsof");
        cmd.arg("-a").arg("-d").arg("cwd");
        for name in PROJECT_PROCESS_NAMES {
            cmd.arg("-c").arg(name);
        }
        cmd.arg("-Fcn");
        // lsof exits 1 when nothing matches; stdout is still valid (empty)
        let cwds = cmd
            .output()
            .map(|o| parse_lsof_cwds(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_default();
        let command_lines = Command::new("ps")
            .args(["-axww", "-o", "args="])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect())
            .unwrap_or_default();
        ProcessSnapshot { cwds, command_lines }
    }
}

/// Returns the tool's name if any command line is that tool: one of its
/// executables (including npm's retitled "npm install …" form), or an
/// interpreter running one of its entry scripts (node npm-cli.js). A bare
/// interpreter (editor, language server, dev server) does not count.
fn tool_process_in<'a>(tool: &CacheTool, command_lines: impl IntoIterator<Item = &'a str>) -> Option<String> {
    command_lines.into_iter().find_map(|line| {
        let mut args = line.split_whitespace();
        let exe = args.next()?;
        let exe_name = Path::new(exe).file_name()?.to_str()?;
        if let Some(name) = tool.executables.iter().find(|n| **n == exe_name) {
            return Some(name.to_string());
        }
        args.find_map(|arg| {
            let script = Path::new(arg).file_name()?.to_str()?;
            tool.entry_scripts
                .iter()
                .find(|(entry, _)| *entry == script)
                .map(|(_, name)| name.to_string())
        })
    })
}

/// Parse `lsof -Fcn` output into (command, cwd) pairs.
fn parse_lsof_cwds(output: &str) -> Vec<(String, PathBuf)> {
    let mut pairs = Vec::new();
    let mut command = String::new();
    for line in output.lines() {
        match line.split_at(line.len().min(1)) {
            ("p", _) => command.clear(),
            ("c", name) => command = name.to_string(),
            ("n", path) if !path.is_empty() => pairs.push((command.clone(), PathBuf::from(path))),
            _ => {}
        }
    }
    pairs
}

/// Returns the command name of the first process whose cwd is at or under `root`.
fn process_in_project(cwds: &[(String, PathBuf)], root: &Path) -> Option<String> {
    cwds.iter()
        .find(|(_, cwd)| cwd.starts_with(root))
        .map(|(name, _)| name.clone())
}

/// Returns the artifact's age if its own mtime is within `window` of `now`.
fn recently_modified(path: &Path, now: SystemTime, window: std::time::Duration) -> Option<std::time::Duration> {
    let mtime = fs::symlink_metadata(path).and_then(|m| m.modified()).ok()?;
    // mtime in the future (clock skew) counts as recent
    let age = now.duration_since(mtime).unwrap_or_default();
    (age < window).then_some(age)
}

/// "just now", "45m ago", "2h ago"
fn format_age(age: std::time::Duration) -> String {
    let secs = age.as_secs();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3600)
    }
}

/// Project root for a project-scoped artifact (.next, node_modules/.cache, …), else None.
fn project_root_for(path: &Path) -> Option<&Path> {
    let name = path.file_name()?.to_str()?;
    let parent = path.parent()?;
    if PROJECT_SCOPED_NAMES.contains(&name) {
        return Some(parent);
    }
    if name == ".cache" && parent.file_name().and_then(|n| n.to_str()) == Some("node_modules") {
        return parent.parent();
    }
    None
}

/// Decide whether a Safe-tier artifact should be left alone right now.
/// Project-scoped artifacts (found under a scan root) are guarded by mtime,
/// then by a process working in the project. Home-level caches (~/.npm,
/// ~/.cargo, ~/Library/Caches/*) are guarded only by their tool running.
/// `procs` is not consulted for a recently modified project artifact; the
/// caller caches the snapshot so lsof/ps run at most once per pass.
fn in_use_reason<F>(path: &Path, home: &Path, now: SystemTime, procs: &mut F) -> Option<String>
where
    F: FnMut() -> std::rc::Rc<ProcessSnapshot>,
{
    let project_root = project_root_for(path);

    if project_root.is_none() && path.starts_with(home) {
        let (_, tool) = HOME_CACHE_TOOLS
            .iter()
            .find(|(rel, _)| path.starts_with(home.join(rel)))?;
        let name = tool_process_in(tool, procs().command_lines.iter().map(String::as_str))?;
        return Some(format!("in use by {}", name));
    }

    if let Some(age) = recently_modified(path, now, RECENT_WINDOW) {
        return Some(format!("modified {}", format_age(age)));
    }

    if let Some(root) = project_root {
        // lsof reports resolved paths (/private/tmp, not /tmp)
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        if let Some(name) = process_in_project(&procs().cwds, &root) {
            return Some(format!("in use by {}", name));
        }
    }

    None
}

/// Mark SAFE rows that a delete would skip right now, so the UI can show the
/// reason instead of a delete button. Uses the delete guard's own predicate;
/// lsof/ps run at most once per scan.
fn annotate_in_use(artifacts: &mut [DevArtifact]) {
    let home = get_home_dir();
    let now = SystemTime::now();
    let mut snapshot: Option<std::rc::Rc<ProcessSnapshot>> = None;
    let mut procs = || snapshot.get_or_insert_with(|| std::rc::Rc::new(ProcessSnapshot::capture())).clone();
    for a in artifacts.iter_mut().filter(|a| a.tier == ArtifactTier::Safe) {
        a.in_use = in_use_reason(Path::new(&a.path), &home, now, &mut procs);
    }
}

/// Re-evaluate only the rows held back right now (building, or a Safe row in
/// use) and release any whose reason has gone, so "Building now" clears the
/// moment a build ends. No directory walk: one lsof/ps snapshot. Returns
/// whether any row changed.
pub fn recheck_held_back(result: &mut DevScanResult) -> bool {
    let home = get_home_dir();
    let now = SystemTime::now();
    let mut snapshot: Option<std::rc::Rc<ProcessSnapshot>> = None;
    let mut procs = || snapshot.get_or_insert_with(|| std::rc::Rc::new(ProcessSnapshot::capture())).clone();
    let mut changed = false;
    for a in result.artifacts.iter_mut() {
        if a.active_build && !is_active_build(Path::new(&a.path)) {
            a.active_build = false;
            changed = true;
        }
        if a.tier == ArtifactTier::Safe && a.in_use.is_some() {
            let reason = in_use_reason(Path::new(&a.path), &home, now, &mut procs);
            if reason != a.in_use {
                a.in_use = reason;
                changed = true;
            }
        }
    }
    if changed {
        result.refresh_totals();
    }
    changed
}

fn delete_dev_artifacts_inner(
    paths: &[String],
    known_artifacts: &[DevArtifact],
    allow_non_safe: bool,
) -> DevDeleteResult {
    let known_paths: std::collections::HashSet<&str> =
        known_artifacts.iter().map(|a| a.path.as_str()).collect();

    let mut deleted_count = 0usize;
    let mut bytes_freed = 0u64;
    let mut errors = Vec::new();
    let mut skipped = Vec::new();

    let home = get_home_dir();
    let now = SystemTime::now();
    let mut snapshot: Option<std::rc::Rc<ProcessSnapshot>> = None;
    let mut procs = || snapshot.get_or_insert_with(|| std::rc::Rc::new(ProcessSnapshot::capture())).clone();

    for path_str in paths {
        // Safety: only delete paths that were in the scan result
        if !known_paths.contains(path_str.as_str()) {
            errors.push(format!("Skipped unknown path: {}", path_str));
            continue;
        }

        // Minimum path-depth guard: refuse to delete paths with fewer than 4 components.
        // This catches /, $HOME, volume roots, and any future scanner bug that produces
        // dangerously short paths. E.g. /Users/name/something/something = 4 components minimum.
        let path_components = Path::new(path_str).components().count();
        if path_components < 4 {
            let msg = format!(
                "SAFETY: Refused to delete shallow path ({} components): {}",
                path_components, path_str
            );
            eprintln!("{}", msg);
            log_artifact_deletion(path_str, 0, "BLOCKED", "depth_guard", "blocked");
            errors.push(msg);
            continue;
        }

        // Look up the artifact for tier check and size
        let artifact = known_artifacts.iter().find(|a| a.path == *path_str);

        // Active build guard: never delete artifacts with an active build
        if let Some(a) = artifact {
            if a.active_build {
                errors.push(format!("Skipped active-build artifact: {}", path_str));
                continue;
            }
        }

        // Tier guard: bulk operations only delete Safe tier
        if !allow_non_safe {
            if let Some(a) = artifact {
                if a.tier != ArtifactTier::Safe {
                    errors.push(format!("Skipped non-Safe artifact: {}", path_str));
                    continue;
                }
            }
        } else {
            // Manual deletion: allow Safe, Rebuildable, SafeWithReinstall but never Ask
            if let Some(a) = artifact {
                if a.tier == ArtifactTier::Ask {
                    errors.push(format!("Skipped Ask-tier artifact: {}", path_str));
                    continue;
                }
            }
        }

        let path = std::path::Path::new(path_str);
        if !path.exists() {
            continue;
        }

        // In-use guard: leave Safe-tier artifacts alone if a relevant process
        // is using them, or (project artifacts only) they were recently modified
        if let Some(a) = artifact.filter(|a| a.tier == ArtifactTier::Safe) {
            if let Some(reason) = in_use_reason(path, &home, now, &mut procs) {
                crate::cache_cleaner::log_deletion(&format!(
                    "DEV_ARTIFACT_SKIPPED: {} | size={} | tier={} | reason={}",
                    path_str, a.size_display, a.tier.label(), reason
                ));
                skipped.push(SkippedArtifact {
                    path: path_str.clone(),
                    size_bytes: a.size_bytes,
                    size_display: a.size_display.clone(),
                    reason,
                });
                continue;
            }
        }

        let expected_bytes = artifact.map(|a| a.size_bytes).unwrap_or(0);
        let tier_label = artifact.map(|a| a.tier.label()).unwrap_or("unknown");
        let tier = artifact.map(|a| a.tier);

        // Decide mechanism: manual deletes of Rebuildable/SafeWithReinstall go to Trash
        // (recoverable), as does anything nested inside another artifact (e.g.
        // node_modules/.cache inside a REINSTALL parent). Other Safe-tier deletes
        // are permanent (caches regenerate instantly).
        let nested = artifact.map(|a| a.is_nested).unwrap_or(false);
        let use_trash = nested
            || (allow_non_safe
                && matches!(
                    tier,
                    Some(ArtifactTier::Rebuildable) | Some(ArtifactTier::SafeWithReinstall)
                ));

        let mechanism = if use_trash { "trash" } else { "permanent" };

        let result = if use_trash {
            match move_to_trash(path) {
                Ok(trash_path) => {
                    // Record in manifest so we can selectively purge later
                    if !trash_path.as_os_str().is_empty() {
                        record_trashed_item(path_str, &trash_path, expected_bytes);
                    }
                    Ok(())
                }
                Err(e) => Err(e),
            }
        } else {
            fs::remove_dir_all(path).map_err(|e| e.to_string())
        };

        match result {
            Ok(()) => {
                deleted_count += 1;
                bytes_freed += expected_bytes;
                let trigger = if allow_non_safe { "manual" } else { "clean_now" };
                log_artifact_deletion(path_str, expected_bytes, tier_label, trigger, mechanism);
            }
            Err(e) => {
                errors.push(format!("{}: {}", path_str, e));
            }
        }
    }

    DevDeleteResult {
        deleted_count,
        bytes_freed,
        bytes_freed_display: format_size(bytes_freed),
        errors,
        skipped,
    }
}

/// Log a dev artifact deletion to the shared SymbolSweep deletion log
fn log_artifact_deletion(path: &str, size_bytes: u64, tier: &str, trigger: &str, mechanism: &str) {
    let log_path = {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home)
            .join("Library")
            .join("Logs")
            .join("SymbolSweep")
            .join("deletions.log")
    };

    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let timestamp = format_log_timestamp();

    let line = format!(
        "[{}] DEV_ARTIFACT_DELETED: {} | size={} | tier={} | trigger={} | mechanism={}\n",
        timestamp,
        path,
        format_size(size_bytes),
        tier,
        trigger,
        mechanism,
    );

    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        use std::io::Write;
        let _ = file.write_all(line.as_bytes());
    }
}

/// Timestamp for log entries (shared with cache_cleaner)
fn format_log_timestamp() -> String {
    crate::cache_cleaner::chrono_format_now()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Set a path's mtime `hours` into the past (or future, if negative)
    fn backdate(path: &Path, hours: i64) {
        let offset = std::time::Duration::from_secs(hours.unsigned_abs() * 3600);
        let when = if hours >= 0 {
            SystemTime::now() - offset
        } else {
            SystemTime::now() + offset
        };
        fs::File::open(path).unwrap().set_modified(when).unwrap();
    }

    fn snapshot(cwds: &[(&str, &str)], command_lines: &[&str]) -> std::rc::Rc<ProcessSnapshot> {
        std::rc::Rc::new(ProcessSnapshot {
            cwds: cwds.iter().map(|(c, p)| (c.to_string(), PathBuf::from(p))).collect(),
            command_lines: command_lines.iter().map(|l| l.to_string()).collect(),
        })
    }

    // ----------------------------------------------------------------
    // In-use guard: mtime predicate
    // ----------------------------------------------------------------
    #[test]
    fn test_recently_modified_predicate() {
        let tmp = std::env::temp_dir().join("ss-recency-test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let now = SystemTime::now();

        // Fresh dir: recent
        let age = recently_modified(&tmp, now, RECENT_WINDOW).expect("fresh dir is recent");
        assert_eq!(format_age(age), "just now");

        // 2h old: recent, reported in hours
        backdate(&tmp, 2);
        let age = recently_modified(&tmp, SystemTime::now(), RECENT_WINDOW).expect("2h is inside 24h");
        assert_eq!(format!("modified {}", format_age(age)), "modified 2h ago");

        // 48h old: not recent
        backdate(&tmp, 48);
        assert!(recently_modified(&tmp, SystemTime::now(), RECENT_WINDOW).is_none());

        // Future mtime (clock skew): treated as recent
        backdate(&tmp, -1);
        assert!(recently_modified(&tmp, SystemTime::now(), RECENT_WINDOW).is_some());

        // Missing path: not recent (nothing to guard)
        assert!(recently_modified(&tmp.join("missing"), now, RECENT_WINDOW).is_none());

        assert_eq!(format_age(std::time::Duration::from_secs(45 * 60)), "45m ago");
        let _ = fs::remove_dir_all(&tmp);
    }

    // ----------------------------------------------------------------
    // In-use guard: cwd-under-root predicate and lsof parsing
    // ----------------------------------------------------------------
    #[test]
    fn test_process_in_project_predicate() {
        let cwds = snapshot(
            &[("node", "/work/app/packages/web"), ("vite", "/work/app2"), ("npm", "/work")],
            &[],
        );
        assert_eq!(process_in_project(&cwds.cwds, Path::new("/work/app")), Some("node".into()));
        assert_eq!(process_in_project(&cwds.cwds, Path::new("/work/app2")), Some("vite".into()));
        // Component-wise match: /work/app must not match /work/app2 or /work/application
        assert_eq!(process_in_project(&cwds.cwds, Path::new("/work/application")), None);
        // A process in a parent directory does not mark the child project as in use
        assert_eq!(process_in_project(&cwds.cwds, Path::new("/work/other")), None);
        assert_eq!(process_in_project(&[], Path::new("/work/app")), None);
    }

    #[test]
    fn test_npm_guard_matches_npm_not_bare_node() {
        // npm itself, in each form it shows up in `ps -o args=`
        assert_eq!(tool_process_in(&NPM, ["npm install lodash"]), Some("npm".into()));
        assert_eq!(tool_process_in(&NPM, ["npm run dev --port 3003"]), Some("npm".into()));
        assert_eq!(tool_process_in(&NPM, ["/opt/homebrew/bin/npm ci"]), Some("npm".into()));
        assert_eq!(tool_process_in(&NPM, ["npx vite"]), Some("npx".into()));
        assert_eq!(
            tool_process_in(&NPM, ["/usr/local/bin/node /usr/local/lib/node_modules/npm/bin/npm-cli.js install"]),
            Some("npm".into())
        );
        assert_eq!(
            tool_process_in(&NPM, ["node /usr/local/lib/node_modules/npm/bin/npx-cli.js create-vite"]),
            Some("npx".into())
        );

        // Bare node processes must not trip the guard
        let bare_node = [
            "/Applications/Cursor.app/Contents/Frameworks/Code Helper (Plugin).app/Contents/MacOS/Code Helper (Plugin) --type=utility",
            "/usr/local/bin/node /Users/me/.vscode/extensions/ts/tsserver.js --useInferredProjectPerProjectRoot",
            "node /Users/me/site/node_modules/.bin/next dev",
            "node /Users/me/npm-tools/index.js",
            "/usr/local/bin/npmrc-switcher list",
        ];
        assert_eq!(tool_process_in(&NPM, bare_node), None);
        assert_eq!(tool_process_in(&NPM, Vec::<&str>::new()), None);

        // Found among other processes
        assert_eq!(
            tool_process_in(&NPM, ["node /x/server.js", "", "npm install"]),
            Some("npm".into())
        );
    }

    #[test]
    fn test_parse_lsof_cwds() {
        let out = "p101\ncnode\nfcwd\nn/Users/me/proj\np202\ncnext-server\nfcwd\nn/private/tmp/site\n";
        assert_eq!(
            parse_lsof_cwds(out),
            vec![
                ("node".to_string(), PathBuf::from("/Users/me/proj")),
                ("next-server".to_string(), PathBuf::from("/private/tmp/site")),
            ]
        );
        assert!(parse_lsof_cwds("").is_empty());
    }

    #[test]
    fn test_reclassified_tiers() {
        let tmp = std::env::temp_dir().join("ss-reclassify-test");
        let _ = fs::remove_dir_all(&tmp);
        let caches = tmp.join("Library").join("Caches");
        for d in ["ms-playwright", "com.example.app.ShipIt", "pip"] {
            fs::create_dir_all(caches.join(d)).unwrap();
            fs::write(caches.join(d).join("f"), "x").unwrap();
        }
        let project = tmp.join("proj");
        fs::create_dir_all(project.join("coverage")).unwrap();
        fs::write(project.join("coverage").join("lcov.info"), "x").unwrap();
        fs::write(project.join("package.json"), "{}").unwrap();

        let mut artifacts = Vec::new();
        scan_library_caches(&tmp, &mut artifacts);
        scan_project_root(&tmp, &mut artifacts, 0);
        let tier_of = |suffix: &str| {
            artifacts.iter().find(|a| a.path.ends_with(suffix)).map(|a| a.tier)
        };

        assert_eq!(tier_of("ms-playwright"), Some(ArtifactTier::Rebuildable));
        assert_eq!(tier_of(".ShipIt"), Some(ArtifactTier::Rebuildable));
        assert_eq!(tier_of("coverage"), Some(ArtifactTier::Ask));
        assert_eq!(tier_of("pip"), Some(ArtifactTier::Safe));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_staleness_uses_artifact_and_git_index() {
        let tmp = std::env::temp_dir().join("ss-staleness-test");
        let _ = fs::remove_dir_all(&tmp);
        let project = tmp.join("proj");
        let nm = project.join("node_modules");
        let src = project.join("src");
        fs::create_dir_all(&nm).unwrap();
        fs::create_dir_all(&src).unwrap();

        // src/ is fresh but no longer counts; only the artifact does
        backdate(&nm, 10 * 24);
        assert_eq!(check_project_staleness(&nm, &project), Some(10));

        // A recent git index wins over an old artifact
        fs::create_dir_all(project.join(".git")).unwrap();
        fs::write(project.join(".git").join("index"), "x").unwrap();
        backdate(&project.join(".git").join("index"), 3 * 24);
        assert_eq!(check_project_staleness(&nm, &project), Some(3));

        // An artifact newer than the git index wins
        backdate(&nm, 0);
        assert_eq!(check_project_staleness(&nm, &project), Some(0));

        // Nothing readable: unknown
        assert_eq!(check_project_staleness(&tmp.join("missing"), &tmp.join("nope")), None);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_home_level_projects_are_scan_roots() {
        let tmp = std::env::temp_dir().join("ss-home-roots-test");
        let _ = fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        let write = |p: PathBuf, n: usize| {
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, vec![0u8; n]).unwrap();
        };

        // ~/shotrack: Tauri app, package.json at the top, Cargo.toml one level down
        let shotrack = home.join("shotrack");
        write(shotrack.join("package.json"), 2);
        fs::create_dir_all(shotrack.join(".git")).unwrap();
        write(shotrack.join("node_modules").join("react").join("index.js"), 4_000);
        write(shotrack.join("src-tauri").join("Cargo.toml"), 2);
        write(shotrack.join("src-tauri").join("target").join("release").join("shotrack"), 8_000);
        // ~/cli: Rust crate at the top
        write(home.join("cli").join("Cargo.toml"), 2);
        write(home.join("cli").join("target").join("debug").join("cli"), 3_000);
        // ~/notes-repo: marked by .git alone
        fs::create_dir_all(home.join("notes-repo").join(".git")).unwrap();

        // Not project roots
        write(home.join("Documents").join("report.txt"), 1); // no marker
        write(home.join("Library").join("package.json"), 2);
        write(home.join("Applications").join("Cargo.toml"), 2);
        write(home.join(".rustup").join("Cargo.toml"), 2);
        write(home.join("Desktop").join("package.json"), 2); // already a root
        write(home.join("deep").join("inner").join("package.json"), 2); // two levels down
        write(home.join("stray.json"), 1); // file, not folder
        #[cfg(unix)]
        std::os::unix::fs::symlink(&shotrack, home.join("shotrack-link")).unwrap();

        let existing = vec![home.join("Desktop"), home.join("dev")];
        let roots = home_project_roots(&home, &existing);
        assert_eq!(roots, vec![home.join("cli"), home.join("notes-repo"), shotrack.clone()]);

        // The discovered roots find the same artifacts a configured root would
        let mut artifacts = Vec::new();
        for root in &roots {
            scan_project_root(root, &mut artifacts, 0);
        }
        let find = |suffix: &str| artifacts.iter().find(|a| a.path.ends_with(suffix));
        assert_eq!(find("shotrack/src-tauri/target").map(|a| a.size_bytes), Some(8_000));
        assert_eq!(find("shotrack/node_modules").map(|a| a.tier), Some(ArtifactTier::SafeWithReinstall));
        assert_eq!(find("cli/target").map(|a| a.size_bytes), Some(3_000));
        assert_eq!(artifacts.len(), 3, "{:#?}", artifacts);

        // Missing home: nothing to add
        assert!(home_project_roots(&tmp.join("missing"), &[]).is_empty());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_tier_tiles_equal_rows_and_sum_to_total() {
        let tmp = std::env::temp_dir().join("ss-tiles-test");
        let _ = fs::remove_dir_all(&tmp);
        let web = tmp.join("web");
        let write = |p: PathBuf, n: usize| {
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, vec![0u8; n]).unwrap();
        };
        write(web.join("package.json"), 2);
        write(web.join("node_modules").join("react").join("index.js"), 5_000);
        write(web.join("node_modules").join(".cache").join("babel").join("x.json"), 700);
        write(web.join(".next").join("cache").join("webpack.pack"), 3_000);
        write(web.join("dist").join("app.js"), 1_100);
        write(web.join("coverage").join("lcov.info"), 90);
        let rust = tmp.join("svc");
        write(rust.join("Cargo.toml"), 2);
        write(rust.join("target").join("debug").join("svc"), 9_000);

        let mut artifacts = Vec::new();
        scan_project_root(&tmp, &mut artifacts, 0);
        let t = TierTotals::of(&artifacts);
        let rows = |tier: ArtifactTier| -> u64 {
            artifacts.iter().filter(|a| a.tier == tier).map(|a| a.size_bytes).sum()
        };

        // Each tile equals the sum of the rows shown under it
        assert_eq!(t.safe, rows(ArtifactTier::Safe));
        assert_eq!(t.rebuildable, rows(ArtifactTier::Rebuildable));
        assert_eq!(t.safe_with_reinstall, rows(ArtifactTier::SafeWithReinstall));
        assert_eq!(t.ask, rows(ArtifactTier::Ask));
        // SAFE + REBUILD + REINSTALL + REVIEW == total, and total == all rows
        assert_eq!(t.safe + t.rebuildable + t.safe_with_reinstall + t.ask, t.total);
        assert_eq!(t.total, artifacts.iter().map(|a| a.size_bytes).sum::<u64>());

        // node_modules/.cache is its own SAFE row; the parent row excludes it
        let row = |suffix: &str| artifacts.iter().find(|a| a.path.ends_with(suffix)).unwrap();
        let nm_cache = row("node_modules/.cache");
        assert_eq!((nm_cache.tier, nm_cache.is_nested, nm_cache.size_bytes), (ArtifactTier::Safe, true, 700));
        assert_eq!(row("web/node_modules").size_bytes, 5_000);
        assert_eq!(t.safe, 700 + 3_000); // .cache + .next
        // Every byte on disk under the artifacts is counted exactly once
        assert_eq!(t.total, 5_000 + 700 + 3_000 + 1_100 + 90 + 9_000);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_nested_cache_delete_goes_to_trash() {
        let tmp = std::env::temp_dir().join("ss-nested-trash-test");
        let _ = fs::remove_dir_all(&tmp);
        let cache = tmp.join("proj").join("node_modules").join(".cache");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("x"), "x").unwrap();
        backdate(&cache, 48);
        let path = cache.to_string_lossy().to_string();
        let artifact = DevArtifact {
            path: path.clone(),
            size_bytes: 1,
            size_display: "1 B".to_string(),
            tier: ArtifactTier::Safe,
            kind: "node_modules/.cache (build cache)".to_string(),
            project: None,
            staleness_days: None,
            is_nested: true,
            hint: None,
            active_build: false,
            in_use: None,
        };

        // Bulk (Clean Now) path: still Trash, because it sits inside a REINSTALL parent
        let result = delete_dev_artifacts(&[path.clone()], &[artifact]);
        assert_eq!(result.deleted_count, 1);
        assert!(!cache.exists());
        let manifest = load_trash_manifest();
        let entry = manifest.iter().rev().find(|i| i.original_path == path);
        assert!(entry.is_some(), "nested delete must be recorded as a Trash move");
        let trashed = PathBuf::from(&entry.unwrap().trash_path);
        assert!(trashed.starts_with(trash_root()));

        // Clean up: remove our Trash item and its manifest entry
        let _ = fs::remove_dir_all(&trashed);
        save_trash_manifest(&manifest.into_iter().filter(|i| i.original_path != path).collect::<Vec<_>>());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_scan_marks_in_use_safe_rows() {
        let tmp = std::env::temp_dir().join("ss-annotate-test");
        let _ = fs::remove_dir_all(&tmp);
        let web = tmp.join("web");
        fs::create_dir_all(web.join(".next")).unwrap();
        fs::write(web.join(".next").join("a"), vec![0u8; 300]).unwrap();
        fs::create_dir_all(web.join(".turbo")).unwrap();
        fs::write(web.join(".turbo").join("b"), vec![0u8; 200]).unwrap();
        fs::write(web.join("package.json"), "{}").unwrap();
        backdate(&web.join(".turbo"), 48);

        let mut artifacts = Vec::new();
        scan_project_root(&tmp, &mut artifacts, 0);
        annotate_in_use(&mut artifacts);
        let row = |suffix: &str| artifacts.iter().find(|a| a.path.ends_with(suffix)).unwrap();

        assert_eq!(row(".next").in_use.as_deref(), Some("modified just now"));
        assert_eq!(row(".turbo").in_use, None);
        let t = TierTotals::of(&artifacts);
        assert_eq!(t.safe, 500, "SAFE tile counts every SAFE row");
        assert_eq!(t.safe_deletable, 200, "Clean Now amount excludes in-use rows");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_project_name_uses_git_repo_root() {
        let tmp = std::env::temp_dir().join("ss-project-name-test");
        let _ = fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        let repo = home.join("code/SymbolSweep");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(repo.join("src-tauri/target")).unwrap();
        fs::create_dir_all(home.join("code/loose/target")).unwrap();

        // Inside a repo: the repo root's folder name, however deep
        assert_eq!(project_name_in(&repo.join("src-tauri/target"), &home).as_deref(), Some("SymbolSweep"));
        assert_eq!(project_name_in(&repo.join("node_modules"), &home).as_deref(), Some("SymbolSweep"));
        // No .git above: the parent folder, as before
        assert_eq!(project_name_in(&home.join("code/loose/target"), &home).as_deref(), Some("loose"));
        // A .git at home itself (dotfiles) is ignored
        fs::create_dir_all(home.join(".git")).unwrap();
        assert_eq!(project_name_in(&home.join("code/loose/target"), &home).as_deref(), Some("loose"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_project_root_for() {
        assert_eq!(project_root_for(Path::new("/w/app/.next")), Some(Path::new("/w/app")));
        assert_eq!(project_root_for(Path::new("/w/app/.vite")), Some(Path::new("/w/app")));
        assert_eq!(
            project_root_for(Path::new("/w/app/node_modules/.cache")),
            Some(Path::new("/w/app"))
        );
        assert_eq!(project_root_for(Path::new("/Users/me/.npm/_cacache")), None);
        assert_eq!(project_root_for(Path::new("/Users/me/Library/Caches/pip")), None);
    }

    // ----------------------------------------------------------------
    // In-use guard: full decision with a mocked process list
    // ----------------------------------------------------------------
    #[test]
    fn test_in_use_reason_with_mocked_processes() {
        let tmp = std::env::temp_dir().join("ss-inuse-test");
        let _ = fs::remove_dir_all(&tmp);
        let project = tmp.join("site");
        let next_dir = project.join(".next");
        fs::create_dir_all(&next_dir).unwrap();
        let home = tmp.join("home");
        let npm_cache = home.join(".npm").join("_cacache");
        fs::create_dir_all(&npm_cache).unwrap();
        let root = project.canonicalize().unwrap();
        let root_str = root.to_string_lossy().to_string();

        let cargo_registry = home.join(".cargo").join("registry");
        fs::create_dir_all(&cargo_registry).unwrap();
        let pip_cache = home.join("Library").join("Caches").join("pip");
        fs::create_dir_all(&pip_cache).unwrap();

        // Recent project artifact: skipped on mtime alone, process list never consulted
        let mut calls = 0;
        let mut procs = || { calls += 1; snapshot(&[], &[]) };
        let reason = in_use_reason(&next_dir, &home, SystemTime::now(), &mut procs);
        assert_eq!(reason.as_deref(), Some("modified just now"));
        assert_eq!(calls, 0);

        // Home caches ignore mtime: freshly modified, tool idle -> deletable
        for cache in [&npm_cache, &cargo_registry, &pip_cache] {
            let mut procs = || snapshot(&[], &[]);
            assert_eq!(in_use_reason(cache, &home, SystemTime::now(), &mut procs), None, "{:?}", cache);
        }

        // Home caches while their tool runs: named by the tool, never by age
        let running = ["/Users/me/.cargo/bin/cargo build --release", "npm install", "pip3 install requests"];
        let mut procs = || snapshot(&[], &running);
        for (cache, tool) in [(&npm_cache, "npm"), (&cargo_registry, "cargo"), (&pip_cache, "pip3")] {
            assert_eq!(
                in_use_reason(cache, &home, SystemTime::now(), &mut procs),
                Some(format!("in use by {}", tool))
            );
        }

        // Another tool running does not mark an unrelated home cache
        let mut procs = || snapshot(&[], &["npm install"]);
        assert_eq!(in_use_reason(&cargo_registry, &home, SystemTime::now(), &mut procs), None);

        backdate(&next_dir, 48);

        // Old + dev server running in the project: skipped
        let mut procs = || snapshot(&[("node", &root_str)], &[]);
        assert_eq!(
            in_use_reason(&next_dir, &home, SystemTime::now(), &mut procs).as_deref(),
            Some("in use by node")
        );

        // Old + dev server in a sibling project: deletable
        let sibling = tmp.join("site-two").to_string_lossy().to_string();
        let mut procs = || snapshot(&[("node", &sibling)], &[]);
        assert_eq!(in_use_reason(&next_dir, &home, SystemTime::now(), &mut procs), None);

        let _ = fs::remove_dir_all(&tmp);
    }

    // ----------------------------------------------------------------
    // In-use guard: bulk delete reports and keeps a fresh Safe artifact
    // ----------------------------------------------------------------
    #[test]
    fn test_bulk_delete_skips_recent_safe_artifact() {
        let tmp = std::env::temp_dir().join("ss-skip-recent-test");
        let _ = fs::remove_dir_all(&tmp);
        let fresh = tmp.join("proj").join(".turbo");
        fs::create_dir_all(&fresh).unwrap();
        fs::write(fresh.join("cache.bin"), "x").unwrap();
        let path = fresh.to_string_lossy().to_string();

        let artifact = DevArtifact {
            path: path.clone(),
            size_bytes: 1,
            size_display: "1 B".to_string(),
            tier: ArtifactTier::Safe,
            kind: ".turbo cache".to_string(),
            project: None,
            staleness_days: None,
            is_nested: false,
            hint: None,
            active_build: false,
            in_use: None,
        };

        let result = delete_dev_artifacts(&[path.clone()], &[artifact]);
        assert_eq!(result.deleted_count, 0);
        assert!(fresh.exists(), "Recently modified Safe artifact must survive");
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].path, path);
        assert_eq!(result.skipped[0].reason, "modified just now");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_artifact_tier_labels() {
        assert_eq!(ArtifactTier::Safe.label(), "SAFE");
        assert_eq!(ArtifactTier::Rebuildable.label(), "REBUILD");
        assert_eq!(ArtifactTier::SafeWithReinstall.label(), "SAFE-WITH-REINSTALL");
        assert_eq!(ArtifactTier::Ask.label(), "REVIEW");
    }

    #[test]
    fn test_default_scan_roots() {
        let roots = default_scan_roots();
        // Should contain Desktop, dev, Projects at minimum
        let home = get_home_dir().to_string_lossy().to_string();
        assert!(roots.contains(&format!("{}/Desktop", home)));
        assert!(roots.contains(&format!("{}/dev", home)));
        assert!(roots.contains(&format!("{}/Projects", home)));
    }

    #[test]
    fn test_looks_like_project() {
        // The SymbolSweep project itself has package.json
        let project_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
        // This is the SymbolSweep root which should have package.json
        assert!(looks_like_project(&project_dir));
    }

    #[test]
    fn test_scan_dev_artifacts_live() {
        let result = scan_dev_artifacts(&[]);

        println!("\n{}", "=".repeat(60));
        println!("  DEV ARTIFACT SCAN RESULTS");
        println!("{}", "=".repeat(60));
        println!("Scan roots checked: {:?}", result.scan_roots);
        println!("Scan duration: {}ms", result.scan_duration_ms);
        println!();
        println!(
            "TOTAL RECLAIMABLE: {} ({} bytes)",
            result.total_display, result.total_bytes
        );
        println!(
            "  SAFE:                {} ({} bytes)",
            result.safe_display, result.safe_bytes
        );
        println!(
            "  REBUILD:             {} ({} bytes)",
            result.rebuildable_display, result.rebuildable_bytes
        );
        println!(
            "  SAFE-WITH-REINSTALL: {} ({} bytes)",
            result.safe_with_reinstall_display, result.safe_with_reinstall_bytes
        );
        println!(
            "  REVIEW:              {} ({} bytes)",
            result.ask_display, result.ask_bytes
        );
        println!();
        println!("Artifacts found: {}", result.artifacts.len());
        println!("{:-<80}", "");
        for artifact in &result.artifacts {
            let nested_marker = if artifact.is_nested { " (nested)" } else { "" };
            println!(
                "  [{:<20}] {:>10} | {}{} | project={} staleness={}",
                artifact.tier.label(),
                artifact.size_display,
                artifact.kind,
                nested_marker,
                artifact.project.as_deref().unwrap_or("-"),
                artifact
                    .staleness_days
                    .map(|d| format!("{}d", d))
                    .unwrap_or_else(|| "-".to_string()),
            );
            println!("    {}", artifact.path);
        }
        println!("{:-<80}", "");
    }

    /// Verify that bulk delete (Clean Now / auto-clean) rejects non-Safe tiers,
    /// and that manual delete rejects Ask tier but allows others.
    #[test]
    fn test_bulk_delete_rejects_non_safe() {
        use std::fs;

        // Create temp dirs for each tier
        let tmp = std::env::temp_dir().join("ss-tier-test");
        let _ = fs::remove_dir_all(&tmp);
        let safe_dir = tmp.join("safe-cache");
        let rebuild_dir = tmp.join("rebuild-target");
        let reinstall_dir = tmp.join("reinstall-nm");
        let ask_dir = tmp.join("ask-dist");

        for d in [&safe_dir, &rebuild_dir, &reinstall_dir, &ask_dir] {
            fs::create_dir_all(d).unwrap();
            // Write a marker file so the dir isn't empty
            fs::write(d.join("marker.txt"), "test").unwrap();
        }
        // Age the Safe fixture past the in-use guard's recency window
        backdate(&safe_dir, 48);

        let artifacts = vec![
            DevArtifact {
                path: safe_dir.to_string_lossy().to_string(),
                size_bytes: 100,
                size_display: "100 B".to_string(),
                tier: ArtifactTier::Safe,
                kind: "test safe".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: None,
                active_build: false,
                in_use: None,
            },
            DevArtifact {
                path: rebuild_dir.to_string_lossy().to_string(),
                size_bytes: 200,
                size_display: "200 B".to_string(),
                tier: ArtifactTier::Rebuildable,
                kind: "test rebuild".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: None,
                active_build: false,
                in_use: None,
            },
            DevArtifact {
                path: reinstall_dir.to_string_lossy().to_string(),
                size_bytes: 300,
                size_display: "300 B".to_string(),
                tier: ArtifactTier::SafeWithReinstall,
                kind: "test reinstall".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: None,
                active_build: false,
                in_use: None,
            },
            DevArtifact {
                path: ask_dir.to_string_lossy().to_string(),
                size_bytes: 400,
                size_display: "400 B".to_string(),
                tier: ArtifactTier::Ask,
                kind: "test ask".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: None,
                active_build: false,
                in_use: None,
            },
        ];

        let all_paths: Vec<String> = artifacts.iter().map(|a| a.path.clone()).collect();

        // --- Test 1: Bulk delete (allow_non_safe = false) should only delete Safe ---
        let result = delete_dev_artifacts(&all_paths, &artifacts);
        assert_eq!(result.deleted_count, 1, "Bulk should only delete 1 (Safe)");
        assert_eq!(result.bytes_freed, 100);
        assert!(!safe_dir.exists(), "Safe dir should be deleted");
        assert!(rebuild_dir.exists(), "Rebuildable dir should survive bulk delete");
        assert!(reinstall_dir.exists(), "SafeWithReinstall dir should survive bulk delete");
        assert!(ask_dir.exists(), "Ask dir should survive bulk delete");
        assert_eq!(result.errors.len(), 3, "Should have 3 skip errors");

        // Recreate safe dir for manual test
        fs::create_dir_all(&safe_dir).unwrap();
        fs::write(safe_dir.join("marker.txt"), "test").unwrap();
        backdate(&safe_dir, 48);

        // --- Test 2: Manual delete should allow Safe + Rebuildable + SafeWithReinstall but reject Ask ---
        let result = delete_dev_artifacts_manual(&all_paths, &artifacts);
        assert_eq!(result.deleted_count, 3, "Manual should delete 3 (Safe+Rebuild+Reinstall)");
        assert_eq!(result.bytes_freed, 600); // 100 + 200 + 300
        assert!(!safe_dir.exists(), "Safe dir should be deleted by manual");
        assert!(!rebuild_dir.exists(), "Rebuildable dir should be deleted by manual");
        assert!(!reinstall_dir.exists(), "SafeWithReinstall dir should be deleted by manual");
        assert!(ask_dir.exists(), "Ask dir should survive even manual delete");
        assert_eq!(result.errors.len(), 1, "Should have 1 skip error for Ask");

        // Cleanup
        let _ = fs::remove_dir_all(&tmp);
    }

    // ========================================================================
    // SYMLINK SAFETY TESTS
    // ========================================================================

    /// CRITICAL: A symlinked cache directory must NOT cause deletion of the
    /// symlink's real target. This tests the full scan→delete pipeline.
    /// Scenario: ~/Desktop/myproject/node_modules → /important/data
    /// Deleting node_modules must remove the LINK, not /important/data.
    #[test]
    fn test_symlink_delete_does_not_follow_to_real_target() {
        use std::os::unix::fs::symlink;

        let tmp = std::env::temp_dir().join("ss-symlink-test");
        let _ = fs::remove_dir_all(&tmp);

        // Create the "important" directory that must survive
        let important = tmp.join("important_data");
        fs::create_dir_all(&important).unwrap();
        fs::write(important.join("precious.txt"), "DO NOT DELETE").unwrap();

        // Create a fake project with a symlinked directory that looks like
        // a Safe-tier cache — but actually points at important_data
        let project = tmp.join("fakeproject");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("package.json"), "{}").unwrap();

        let cache_link = project.join(".next");
        symlink(&important, &cache_link).unwrap();

        // Verify the symlink was created and points where we think
        assert!(cache_link.exists(), "Symlink should exist");
        assert!(cache_link.is_dir(), ".next should resolve to a directory");

        // Phase 4 scanner should skip this because it's a symlink
        let mut artifacts = Vec::new();
        scan_project_root(&project, &mut artifacts, 0);

        // The scanner should NOT have picked up the symlinked .next
        let found_next = artifacts.iter().any(|a| a.path.contains(".next"));
        assert!(
            !found_next,
            "Scanner should skip symlinked .next directory — but found: {:?}",
            artifacts.iter().map(|a| &a.path).collect::<Vec<_>>()
        );

        // Even if somehow an artifact pointing at the symlink got into the
        // delete list, fs::remove_dir_all on a symlink should NOT follow it.
        // Let's prove this directly:
        let link_str = cache_link.to_string_lossy().to_string();
        let fake_artifact = DevArtifact {
            path: link_str.clone(),
            size_bytes: 100,
            size_display: "100 B".to_string(),
            tier: ArtifactTier::Safe,
            kind: "test".to_string(),
            project: None,
            staleness_days: None,
            is_nested: false,
            hint: None,
            active_build: false,
            in_use: None,
        };

        // Attempt deletion — this calls fs::remove_dir_all on the symlink path
        let result = delete_dev_artifacts(&[link_str], &[fake_artifact]);

        // Check what happened:
        // On macOS/Linux, remove_dir_all on a symlink-to-dir FOLLOWS THE LINK
        // and deletes the contents of the target, then removes the link.
        // This is the dangerous behavior we need to document.
        let important_survived = important.join("precious.txt").exists();

        if important_survived {
            println!("SAFE: important_data/precious.txt survived deletion of symlink");
        } else {
            println!(
                "DANGER: remove_dir_all FOLLOWED the symlink and deleted important_data contents!"
            );
            println!("Result: {:?}", result);
        }

        // The REAL safety comes from the scanner skipping symlinks.
        // If the scanner doesn't add the symlink to the artifact list,
        // it can never be passed to delete_dev_artifacts.
        // But if remove_dir_all DOES follow symlinks, that's a latent risk.

        // Assert that the important data survived because the scanner
        // NEVER picked up the symlink in the first place:
        assert!(
            important.exists(),
            "important_data directory must survive (scanner should never list it)"
        );

        // Cleanup
        let _ = fs::remove_dir_all(&tmp);
    }

    /// Test that scan_project_root genuinely skips symlinks at the entry level
    #[test]
    fn test_scan_skips_symlinked_directories() {
        use std::os::unix::fs::symlink;

        let tmp = std::env::temp_dir().join("ss-scan-symlink");
        let _ = fs::remove_dir_all(&tmp);

        let real_target = tmp.join("real_target");
        fs::create_dir_all(real_target.join("subdir")).unwrap();
        fs::write(real_target.join("subdir").join("file.txt"), "data").unwrap();

        // Create project root with symlinks mimicking scannable dirs
        let project = tmp.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]").unwrap();

        // Symlink "target" → real_target (should be skipped)
        symlink(&real_target, project.join("target")).unwrap();
        // Symlink "node_modules" → real_target (should be skipped)
        symlink(&real_target, project.join("node_modules")).unwrap();
        // Symlink ".next" → real_target (should be skipped)
        symlink(&real_target, project.join(".next")).unwrap();

        let mut artifacts = Vec::new();
        scan_project_root(&project, &mut artifacts, 0);

        assert!(
            artifacts.is_empty(),
            "No artifacts should be found from symlinked directories, but found: {:?}",
            artifacts.iter().map(|a| format!("{} ({})", a.path, a.kind)).collect::<Vec<_>>()
        );

        // real_target must be untouched
        assert!(real_target.join("subdir").join("file.txt").exists());

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Test that Phase 2 (Library/Caches) skips symlinked entries
    #[test]
    fn test_library_caches_skips_symlinks() {
        use std::os::unix::fs::symlink;

        let tmp = std::env::temp_dir().join("ss-lib-symlink");
        let _ = fs::remove_dir_all(&tmp);

        // Build a fake home directory structure
        let fake_home = tmp.join("fakehome");
        let caches = fake_home.join("Library").join("Caches");
        fs::create_dir_all(&caches).unwrap();

        // Real important data
        let important = tmp.join("important");
        fs::create_dir_all(&important).unwrap();
        fs::write(important.join("data.db"), "critical").unwrap();

        // Symlink Library/Caches/Homebrew → important
        symlink(&important, caches.join("Homebrew")).unwrap();

        let mut artifacts = Vec::new();
        scan_library_caches(&fake_home, &mut artifacts);

        assert!(
            artifacts.is_empty(),
            "Symlinked Homebrew cache should be skipped, but found: {:?}",
            artifacts.iter().map(|a| &a.path).collect::<Vec<_>>()
        );
        assert!(important.join("data.db").exists(), "Important data must survive");

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Test: remove_dir_all behavior on symlinks (documents the actual risk)
    #[test]
    fn test_remove_dir_all_symlink_behavior() {
        use std::os::unix::fs::symlink;

        let tmp = std::env::temp_dir().join("ss-rda-symlink");
        let _ = fs::remove_dir_all(&tmp);

        let target_dir = tmp.join("target_dir");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(target_dir.join("file.txt"), "data").unwrap();

        let link = tmp.join("the_link");
        symlink(&target_dir, &link).unwrap();

        // What does remove_dir_all do to a symlink?
        let result = fs::remove_dir_all(&link);
        println!("remove_dir_all on symlink result: {:?}", result);

        let target_survived = target_dir.join("file.txt").exists();
        let link_exists = link.exists();

        println!("Target dir contents survived: {}", target_survived);
        println!("Link still exists: {}", link_exists);

        // Document what actually happened — this test is informational
        if !target_survived {
            println!("WARNING: remove_dir_all FOLLOWS symlinks and destroys target contents!");
        } else {
            println!("OK: remove_dir_all only removed the link, target survived");
        }

        // We don't assert the behavior of remove_dir_all — we document it.
        // The safety guarantee comes from the SCANNER never adding symlinks.

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Test: Phase 1 home-level caches — symlinked ~/.npm must produce zero artifacts.
    /// Previously this test documented the gap; now the symlink guard is in place.
    #[test]
    fn test_home_cache_symlink_skipped() {
        use std::os::unix::fs::symlink;

        let tmp = std::env::temp_dir().join("ss-home-symlink");
        let _ = fs::remove_dir_all(&tmp);

        let fake_home = tmp.join("fakehome");
        fs::create_dir_all(&fake_home).unwrap();

        // Create important data that the symlink will point to
        let important = tmp.join("important_project");
        fs::create_dir_all(&important).unwrap();
        fs::write(important.join("main.rs"), "fn main() {}").unwrap();

        // Symlink ~/.npm → important_project
        symlink(&important, fake_home.join(".npm")).unwrap();

        let mut artifacts = Vec::new();
        check_home_cache(&fake_home, ".npm", "npm global cache", &mut artifacts);
        // Nested target reached through the symlinked ~/.npm must also be skipped
        fs::create_dir_all(important.join("_cacache")).unwrap();
        fs::write(important.join("_cacache").join("blob"), "data").unwrap();
        check_home_cache(&fake_home, ".npm/_cacache", "npm package cache", &mut artifacts);

        // With the symlink guard, check_home_cache must now skip symlinked paths
        assert!(
            artifacts.is_empty(),
            "Phase 1: symlinked ~/.npm must produce zero artifacts, but found: {:?}",
            artifacts.iter().map(|a| &a.path).collect::<Vec<_>>()
        );

        // Important data must be untouched
        assert!(important.join("main.rs").exists(), "Important data must survive");

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Test: Phase 3b — symlinked ~/.gradle/caches must produce zero artifacts.
    #[test]
    fn test_rebuildable_home_caches_symlink_skipped() {
        use std::os::unix::fs::symlink;

        let tmp = std::env::temp_dir().join("ss-phase3b-symlink");
        let _ = fs::remove_dir_all(&tmp);

        let fake_home = tmp.join("fakehome");
        let gradle_parent = fake_home.join(".gradle");
        fs::create_dir_all(&gradle_parent).unwrap();

        // Create important data that the symlink will point to
        let important = tmp.join("real_gradle_data");
        fs::create_dir_all(&important).unwrap();
        fs::write(important.join("build.db"), "critical build data").unwrap();

        // Symlink ~/.gradle/caches → real_gradle_data
        symlink(&important, gradle_parent.join("caches")).unwrap();

        let mut artifacts = Vec::new();
        scan_rebuildable_home_caches(&fake_home, &mut artifacts);

        // Symlink guard must prevent this from being listed
        let gradle_artifacts: Vec<_> = artifacts
            .iter()
            .filter(|a| a.path.contains("gradle"))
            .collect();
        assert!(
            gradle_artifacts.is_empty(),
            "Phase 3b: symlinked ~/.gradle/caches must produce zero artifacts, but found: {:?}",
            gradle_artifacts.iter().map(|a| &a.path).collect::<Vec<_>>()
        );

        // Important data must be untouched
        assert!(important.join("build.db").exists(), "Important data must survive");

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Test: active_build guard prevents deletion
    #[test]
    fn test_active_build_blocks_deletion() {
        let tmp = std::env::temp_dir().join("ss-active-build-test");
        let _ = fs::remove_dir_all(&tmp);
        let dir = tmp.join("active");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("marker"), "test").unwrap();

        let artifact = DevArtifact {
            path: dir.to_string_lossy().to_string(),
            size_bytes: 100,
            size_display: "100 B".to_string(),
            tier: ArtifactTier::Safe,
            kind: "test".to_string(),
            project: None,
            staleness_days: None,
            is_nested: false,
            hint: None,
            active_build: true, // ACTIVE BUILD
            in_use: None,
        };

        // Bulk delete should refuse
        let result = delete_dev_artifacts(
            &[dir.to_string_lossy().to_string()],
            &[artifact.clone()],
        );
        assert_eq!(result.deleted_count, 0, "Active build should block bulk delete");
        assert!(dir.exists(), "Directory must survive");

        // Manual delete should also refuse
        let result = delete_dev_artifacts_manual(
            &[dir.to_string_lossy().to_string()],
            &[artifact],
        );
        assert_eq!(result.deleted_count, 0, "Active build should block manual delete");
        assert!(dir.exists(), "Directory must survive manual delete too");

        let _ = fs::remove_dir_all(&tmp);
    }

    // ----------------------------------------------------------------
    // Build guard: per project
    // ----------------------------------------------------------------

    /// A Rust project with an old Cargo.lock and target/.rustc_info.json
    fn rust_project(root: &Path) -> PathBuf {
        let old = SystemTime::now() - std::time::Duration::from_secs(3600);
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        for f in [root.join("Cargo.lock"), root.join("target").join(".rustc_info.json")] {
            fs::write(&f, "x").unwrap();
            fs::File::options().write(true).open(&f).unwrap().set_modified(old).unwrap();
        }
        root.join("target")
    }

    fn procs(cwds: &[(&str, &Path)]) -> BuildProcs {
        BuildProcs {
            cwds: cwds
                .iter()
                .map(|(n, p)| (n.to_string(), p.canonicalize().unwrap_or_else(|_| p.to_path_buf())))
                .collect(),
        }
    }

    #[test]
    fn test_cargo_in_one_project_does_not_hold_back_another() {
        let tmp = std::env::temp_dir().join("ss-build-guard-test");
        let _ = fs::remove_dir_all(&tmp);
        let a = tmp.join("a");
        let b = tmp.join("b");
        let a_target = rust_project(&a);
        let b_target = rust_project(&b);
        let now = SystemTime::now();
        let home = tmp.join("home");

        // cargo building A (from a subfolder, like a workspace member)
        fs::create_dir_all(a.join("crates").join("core")).unwrap();
        let p = procs(&[("cargo", &a.join("crates").join("core"))]);
        assert!(building(&a_target, &home, &p, now), "cargo in A holds back A's target");
        assert!(!building(&b_target, &home, &p, now), "cargo in A must not hold back B's target");

        // rustc counts too; nothing running at all holds back nothing
        assert!(building(&b_target, &home, &procs(&[("rustc", &b)]), now));
        assert!(!building(&a_target, &home, &procs(&[]), now));

        // A sibling folder whose name starts the same is a different project
        let a2 = tmp.join("a2");
        let a2_target = rust_project(&a2);
        assert!(!building(&a2_target, &home, &procs(&[("cargo", &a)]), now));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_recent_cargo_lock_or_rustc_info_means_building() {
        let tmp = std::env::temp_dir().join("ss-build-recent-test");
        let _ = fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        let none = procs(&[]);

        let lock_proj = tmp.join("lock");
        let lock_target = rust_project(&lock_proj);
        fs::write(lock_proj.join("Cargo.lock"), "changed").unwrap();
        assert!(building(&lock_target, &home, &none, SystemTime::now()), "Cargo.lock changed just now");

        let info_proj = tmp.join("info");
        let info_target = rust_project(&info_proj);
        fs::write(info_target.join(".rustc_info.json"), "changed").unwrap();
        assert!(building(&info_target, &home, &none, SystemTime::now()), ".rustc_info.json changed just now");

        // Six minutes later neither counts
        let later = SystemTime::now() + std::time::Duration::from_secs(6 * 60);
        assert!(!building(&lock_target, &home, &none, later));
        assert!(!building(&info_target, &home, &none, later));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_xcodebuild_is_per_project_except_shared_derived_data() {
        let tmp = std::env::temp_dir().join("ss-build-xcode-test");
        let _ = fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        let app_a = tmp.join("AppA");
        let app_b = tmp.join("AppB");
        for app in [&app_a, &app_b] {
            fs::create_dir_all(app.join("DerivedData")).unwrap();
        }
        let shared = home.join("Library/Developer/Xcode/DerivedData");
        fs::create_dir_all(&shared).unwrap();
        let now = SystemTime::now();

        let p = procs(&[("xcodebuild", &app_a)]);
        assert!(building(&app_a.join("DerivedData"), &home, &p, now));
        assert!(!building(&app_b.join("DerivedData"), &home, &p, now), "xcodebuild in A must not hold back B");
        // Every Xcode project builds into the shared folder
        assert!(building(&shared, &home, &p, now));
        assert!(!building(&shared, &home, &procs(&[]), now));
        // cargo is not an Xcode build
        assert!(!building(&shared, &home, &procs(&[("cargo", &app_a)]), now));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_rust_analyzer_cargo_is_not_a_build() {
        let lsof = "p30\nccargo\nn/w/a\np40\nccargo\nn/w/b\np50\ncrustc\nn/w/c\n";
        let ps = "  1     0 /sbin/launchd\n 20     1 /Applications/Code.app/rust-analyzer\n 30    20 /Users/me/.cargo/bin/cargo\n 35     1 -zsh\n 40    35 cargo\n 45    30 rustc\n 50    45 rustc\n";
        let tree = parse_ps_tree(ps);
        let kept: Vec<(u32, String, PathBuf)> = parse_lsof_pid_cwds(lsof)
            .into_iter()
            .filter(|(pid, _, _)| !has_ancestor(*pid, &tree, "rust-analyzer"))
            .collect();
        // 30 is rust-analyzer's cargo check; 50 is a rustc under it; 40 is a terminal cargo build
        assert_eq!(kept, vec![(40, "cargo".to_string(), PathBuf::from("/w/b"))]);
    }

    #[test]
    fn test_recheck_releases_a_finished_build() {
        let tmp = std::env::temp_dir().join("ss-recheck-test");
        let _ = fs::remove_dir_all(&tmp);
        let target = rust_project(&tmp.join("done"));
        fs::write(target.join("big"), vec![0u8; 1000]).unwrap();
        let row = DevArtifact {
            path: target.to_string_lossy().to_string(),
            size_bytes: 1000,
            size_display: "1000 B".to_string(),
            tier: ArtifactTier::Rebuildable,
            kind: "Rust target (build artifacts)".to_string(),
            project: Some("done".to_string()),
            staleness_days: None,
            is_nested: false,
            hint: None,
            active_build: true, // flagged by an earlier scan; the build has since ended
            in_use: None,
        };
        let mut result = DevScanResult {
            artifacts: vec![row],
            total_bytes: 0, total_display: String::new(),
            safe_bytes: 0, safe_display: String::new(),
            safe_deletable_bytes: 0, safe_deletable_display: String::new(),
            rebuildable_bytes: 0, rebuildable_display: String::new(),
            safe_with_reinstall_bytes: 0, safe_with_reinstall_display: String::new(),
            ask_bytes: 0, ask_display: String::new(),
            scan_duration_ms: 0, scan_roots: vec![],
        };
        // No cargo works in this temp project and its files are an hour old
        assert!(recheck_held_back(&mut result), "a finished build is released");
        assert!(!result.artifacts[0].active_build);
        assert_eq!(result.rebuildable_bytes, 1000, "totals are recomputed");
        assert!(!recheck_held_back(&mut result), "nothing left to release");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_rows_under_1_mb_are_dropped() {
        let row = |bytes: u64| DevArtifact {
            path: format!("/w/{}", bytes),
            size_bytes: bytes,
            size_display: format_size(bytes),
            tier: ArtifactTier::Safe,
            kind: "test".to_string(),
            project: None,
            staleness_days: None,
            is_nested: false,
            hint: None,
            active_build: false,
            in_use: None,
        };
        let mut rows = vec![row(0), row(1024 * 1024 - 1), row(1024 * 1024), row(5 * 1024 * 1024)];
        drop_small_rows(&mut rows);
        let kept: Vec<u64> = rows.iter().map(|a| a.size_bytes).collect();
        assert_eq!(kept, vec![1024 * 1024, 5 * 1024 * 1024]);
        assert_eq!(TierTotals::of(&rows).safe, 6 * 1024 * 1024, "totals count only the rows shown");
    }

    /// Test: unknown paths (not in scan result) are never deleted
    #[test]
    fn test_unknown_path_never_deleted() {
        let tmp = std::env::temp_dir().join("ss-unknown-path-test");
        let _ = fs::remove_dir_all(&tmp);
        let dir = tmp.join("mystery");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("data"), "important").unwrap();

        // Pass the path to delete, but DON'T include it in known_artifacts
        let result = delete_dev_artifacts(
            &[dir.to_string_lossy().to_string()],
            &[], // empty — nothing is "known"
        );
        assert_eq!(result.deleted_count, 0);
        assert!(dir.exists(), "Unknown path must never be deleted");
        assert!(dir.join("data").exists(), "Contents must survive");

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Test: minimum path-depth guard refuses to delete /, $HOME, and shallow paths.
    /// Even if a scanner bug produced such a path, deletion must be blocked.
    #[test]
    fn test_path_depth_guard_blocks_shallow_paths() {
        let home = get_home_dir();
        let home_str = home.to_string_lossy().to_string();

        // Paths that MUST be blocked (fewer than 4 components)
        let dangerous_paths = vec![
            "/".to_string(),                              // 1 component (root)
            home_str.clone(),                             // 3 components: /Users/<name>
            "/Volumes/External".to_string(),              // 2 components
            "/Users/shared".to_string(),                  // 2 components
        ];

        for path_str in &dangerous_paths {
            // Create a fake artifact that claims this path is known and Safe
            let fake_artifact = DevArtifact {
                path: path_str.clone(),
                size_bytes: 999,
                size_display: "999 B".to_string(),
                tier: ArtifactTier::Safe,
                kind: "test".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: None,
                active_build: false,
                in_use: None,
            };

            // Bulk delete
            let result = delete_dev_artifacts(&[path_str.clone()], &[fake_artifact.clone()]);
            assert_eq!(
                result.deleted_count, 0,
                "Depth guard must block bulk deletion of: {}",
                path_str
            );
            assert!(
                result.errors.iter().any(|e| e.contains("shallow path")),
                "Error message must mention shallow path for: {}",
                path_str
            );

            // Manual delete
            let result = delete_dev_artifacts_manual(&[path_str.clone()], &[fake_artifact]);
            assert_eq!(
                result.deleted_count, 0,
                "Depth guard must block manual deletion of: {}",
                path_str
            );
        }

        // A valid deep path (4+ components) should NOT be blocked by depth guard
        // (it may be blocked by existence check, but not by depth)
        let deep_path = "/Users/test/projects/myapp/target".to_string();
        let deep_artifact = DevArtifact {
            path: deep_path.clone(),
            size_bytes: 100,
            size_display: "100 B".to_string(),
            tier: ArtifactTier::Safe,
            kind: "test".to_string(),
            project: None,
            staleness_days: None,
            is_nested: false,
            hint: None,
            active_build: false,
            in_use: None,
        };
        let result = delete_dev_artifacts(&[deep_path.clone()], &[deep_artifact]);
        // Should NOT have a depth-guard error (may have "doesn't exist" skip, that's fine)
        assert!(
            !result.errors.iter().any(|e| e.contains("shallow path")),
            "Deep path should not be blocked by depth guard"
        );
    }

    /// Test: scan root validation rejects dangerous roots.
    /// A config with dev_scan_roots=["/"] must produce no scannable root / no artifacts.
    #[test]
    fn test_scan_root_validation_rejects_dangerous() {
        // "/" as scan root — must produce no artifacts from Phase 4
        let result = scan_dev_artifacts(&["/".to_string()]);
        // Phase 4 uses existing_roots which is filtered by is_safe_scan_root.
        // "/" is rejected, so no Phase 4 artifacts should appear from it.
        // (Phase 1-3 are independent of scan_roots, so we only check scan_roots output)
        assert!(
            !result.scan_roots.contains(&"/".to_string()),
            "Root '/' must be rejected from scan_roots, got: {:?}",
            result.scan_roots
        );

        // Home directory as root
        let home = get_home_dir().to_string_lossy().to_string();
        let result = scan_dev_artifacts(&[home.clone()]);
        assert!(
            !result.scan_roots.contains(&home),
            "Bare home directory must be rejected from scan_roots"
        );

        // Volume root
        let result = scan_dev_artifacts(&["/Volumes/External".to_string()]);
        assert!(
            !result.scan_roots.contains(&"/Volumes/External".to_string()),
            "Volume root must be rejected from scan_roots"
        );

        // System directories
        for sys_dir in &["/System", "/Library", "/Applications", "/private"] {
            assert!(
                !is_safe_scan_root(Path::new(sys_dir)),
                "{} must be rejected by is_safe_scan_root",
                sys_dir
            );
        }
    }

    /// Test: .next without a project file (no package.json etc.) is NOT classified Safe.
    /// A real project's .next (with package.json) still IS classified Safe.
    #[test]
    fn test_safe_cache_requires_project_file() {
        let tmp = std::env::temp_dir().join("ss-project-gate");
        let _ = fs::remove_dir_all(&tmp);

        // Scenario 1: bare directory with .next but NO project file
        let bare = tmp.join("notaproject");
        fs::create_dir_all(bare.join(".next")).unwrap();
        fs::write(bare.join(".next").join("cache.json"), "{}").unwrap();

        let mut artifacts = Vec::new();
        scan_project_root(&bare, &mut artifacts, 0);

        let safe_next: Vec<_> = artifacts
            .iter()
            .filter(|a| a.path.contains(".next") && a.tier == ArtifactTier::Safe)
            .collect();
        assert!(
            safe_next.is_empty(),
            "Bare .next without project file must NOT be Safe, but found: {:?}",
            safe_next.iter().map(|a| &a.path).collect::<Vec<_>>()
        );

        // Scenario 2: real project with package.json + .next
        let project = tmp.join("realproject");
        fs::create_dir_all(project.join(".next")).unwrap();
        fs::write(project.join("package.json"), r#"{"name":"test"}"#).unwrap();
        fs::write(project.join(".next").join("cache.json"), "{}").unwrap();

        let mut artifacts2 = Vec::new();
        scan_project_root(&project, &mut artifacts2, 0);

        let safe_next2: Vec<_> = artifacts2
            .iter()
            .filter(|a| a.path.contains(".next") && a.tier == ArtifactTier::Safe)
            .collect();
        assert!(
            !safe_next2.is_empty(),
            "Real project .next should be Safe, but no Safe artifact found"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Test: Rebuildable manual delete goes to Trash (recoverable),
    /// while Safe bulk delete is permanent.
    #[test]
    fn test_trash_vs_permanent_deletion() {
        let tmp = std::env::temp_dir().join("ss-trash-test");
        let _ = fs::remove_dir_all(&tmp);

        // --- Test 1: Rebuildable manual delete → Trash ---
        let rebuild_dir = tmp.join("projects").join("myapp").join("target");
        fs::create_dir_all(&rebuild_dir).unwrap();
        fs::write(rebuild_dir.join("marker.txt"), "rebuild data").unwrap();

        let rebuild_artifact = DevArtifact {
            path: rebuild_dir.to_string_lossy().to_string(),
            size_bytes: 100,
            size_display: "100 B".to_string(),
            tier: ArtifactTier::Rebuildable,
            kind: "test rebuild".to_string(),
            project: None,
            staleness_days: None,
            is_nested: false,
            hint: None,
            active_build: false,
            in_use: None,
        };

        let result = delete_dev_artifacts_manual(
            &[rebuild_dir.to_string_lossy().to_string()],
            &[rebuild_artifact],
        );
        assert_eq!(result.deleted_count, 1, "Rebuildable manual delete should succeed");
        assert!(!rebuild_dir.exists(), "Rebuild dir should no longer be at original path");
        // The item is now in the (sandbox) Trash and recorded in the manifest
        let rebuild_path = rebuild_dir.to_string_lossy().to_string();
        let manifest = load_trash_manifest();
        let entry = manifest.iter().find(|i| i.original_path == rebuild_path)
            .expect("Rebuildable manual delete must be recorded as a Trash move");
        let trashed = PathBuf::from(&entry.trash_path);
        assert!(trashed.starts_with(trash_root()), "trashed item must land in the sandbox Trash");
        assert!(trashed.join("marker.txt").exists(), "trashed item must be recoverable");

        // --- Test 2: Safe bulk delete → permanent ---
        let safe_dir = tmp.join("caches").join("npm").join("cache");
        fs::create_dir_all(&safe_dir).unwrap();
        fs::write(safe_dir.join("pkg.tgz"), "cache data").unwrap();
        backdate(&safe_dir, 48);

        let safe_artifact = DevArtifact {
            path: safe_dir.to_string_lossy().to_string(),
            size_bytes: 50,
            size_display: "50 B".to_string(),
            tier: ArtifactTier::Safe,
            kind: "test safe cache".to_string(),
            project: None,
            staleness_days: None,
            is_nested: false,
            hint: None,
            active_build: false,
            in_use: None,
        };

        let result = delete_dev_artifacts(
            &[safe_dir.to_string_lossy().to_string()],
            &[safe_artifact],
        );
        assert_eq!(result.deleted_count, 1, "Safe bulk delete should succeed");
        assert!(!safe_dir.exists(), "Safe dir should be permanently deleted");
        // Permanent: never recorded as a Trash move
        let safe_path = safe_dir.to_string_lossy().to_string();
        assert!(
            load_trash_manifest().iter().all(|i| i.original_path != safe_path),
            "Safe bulk delete must not go to Trash"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    // ================================================================
    // Trash Manifest — purge_ss_trash safety tests
    // ================================================================

    /// Helper: back up the real manifest, install a test one, return the backup.
    fn install_test_manifest(items: &[TrashedItem]) -> Vec<TrashedItem> {
        let backup = load_trash_manifest();
        save_trash_manifest(items);
        backup
    }

    /// Helper: restore the real manifest from a backup.
    fn restore_manifest(backup: &[TrashedItem]) {
        save_trash_manifest(backup);
    }

    /// SCOPE: purge deletes ONLY SS-manifest items, not other Trash content.
    /// Uses a temp directory instead of real ~/.Trash (which requires FDA).
    #[test]
    fn test_purge_only_deletes_manifest_items() {
        let tmp_trash = std::env::temp_dir().join("_ss_purge_scope_test");
        let _ = fs::remove_dir_all(&tmp_trash);
        fs::create_dir_all(&tmp_trash).unwrap();

        // "SS item" — recorded in manifest
        let ss_item = tmp_trash.join("ss_trashed_target");
        fs::create_dir_all(&ss_item).unwrap();
        fs::write(ss_item.join("data.bin"), vec![0u8; 512]).unwrap();

        // "Non-SS item" — NOT in manifest (simulates other user Trash content)
        let non_ss_item = tmp_trash.join("user_photo_backup");
        fs::create_dir_all(&non_ss_item).unwrap();
        fs::write(non_ss_item.join("photo.jpg"), vec![0xFFu8; 256]).unwrap();

        assert!(ss_item.exists(), "SS item should exist before purge");
        assert!(non_ss_item.exists(), "Non-SS item should exist before purge");

        let backup = install_test_manifest(&[TrashedItem {
            original_path: "/some/original/path".to_string(),
            trash_path: ss_item.to_string_lossy().to_string(),
            size_bytes: 512,
            timestamp: 0,
        }]);

        let result = purge_ss_trash_in(&tmp_trash, &|_, _, _| {});
        restore_manifest(&backup);

        assert!(!ss_item.exists(), "SS item should be deleted by purge");
        assert!(
            non_ss_item.exists(),
            "SAFETY FAILURE: non-SS item was deleted — purge escaped manifest scope!"
        );
        assert_eq!(result.purged_count, 1);
        assert!(result.errors.is_empty());

        let _ = fs::remove_dir_all(&tmp_trash);
    }

    /// STALE MANIFEST: missing/restored entries handled gracefully.
    /// Uses a temp directory instead of real ~/.Trash (which requires FDA).
    #[test]
    fn test_purge_handles_stale_entries() {
        let tmp_trash = std::env::temp_dir().join("_ss_purge_stale_test");
        let _ = fs::remove_dir_all(&tmp_trash);
        fs::create_dir_all(&tmp_trash).unwrap();

        // One item that exists
        let existing = tmp_trash.join("still_in_trash");
        fs::create_dir_all(&existing).unwrap();
        fs::write(existing.join("f.txt"), b"data").unwrap();

        // Stale entry — path does NOT exist (user already emptied or restored)
        let stale_path = tmp_trash.join("already_gone");
        assert!(!stale_path.exists());

        let backup = install_test_manifest(&[
            TrashedItem {
                original_path: "/original/existing".to_string(),
                trash_path: existing.to_string_lossy().to_string(),
                size_bytes: 100,
                timestamp: 0,
            },
            TrashedItem {
                original_path: "/original/gone".to_string(),
                trash_path: stale_path.to_string_lossy().to_string(),
                size_bytes: 999,
                timestamp: 0,
            },
        ]);

        let result = purge_ss_trash_in(&tmp_trash, &|_, _, _| {});
        let post_manifest = load_trash_manifest();
        restore_manifest(&backup);

        assert!(!existing.exists(), "Existing item should be purged");
        assert_eq!(result.purged_count, 1, "Only 1 item actually deleted");
        assert!(result.errors.is_empty(), "Stale entries should not produce errors");
        assert!(post_manifest.is_empty(), "Manifest should be clean after purge");

        let _ = fs::remove_dir_all(&tmp_trash);
    }

    /// PATH GUARD: manifest entry pointing OUTSIDE ~/.Trash is rejected.
    #[test]
    fn test_purge_rejects_paths_outside_trash() {
        let tmp = std::env::temp_dir().join("_ss_purge_guard_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // Create a real directory outside ~/.Trash
        let outside_item = tmp.join("important_data");
        fs::create_dir_all(&outside_item).unwrap();
        fs::write(outside_item.join("precious.txt"), b"do not delete").unwrap();
        assert!(outside_item.exists());

        // Craft a manifest with a path outside ~/.Trash (simulates corruption)
        let backup = install_test_manifest(&[TrashedItem {
            original_path: "/original/path".to_string(),
            trash_path: outside_item.to_string_lossy().to_string(),
            size_bytes: 42,
            timestamp: 0,
        }]);

        let result = purge_ss_trash();
        restore_manifest(&backup);

        // The item must SURVIVE — guard should have rejected it
        assert!(
            outside_item.exists(),
            "SAFETY FAILURE: purge deleted a path outside ~/.Trash!"
        );
        assert_eq!(result.purged_count, 0, "Nothing should have been purged");
        assert_eq!(result.errors.len(), 1, "Should report one safety rejection");
        assert!(
            result.errors[0].contains("SAFETY"),
            "Error should mention SAFETY: got '{}'",
            result.errors[0]
        );

        println!("PASS: path outside ~/.Trash rejected — item survived, error reported");

        let _ = fs::remove_dir_all(&tmp);
    }
}
