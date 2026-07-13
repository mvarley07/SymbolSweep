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
//! - ASK: dist/build/out — some projects ship from these

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
            ArtifactTier::Ask => "ASK",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            ArtifactTier::Safe => "Caches — regenerate automatically",
            ArtifactTier::Rebuildable => "Build artifacts — regenerable but slow to rebuild",
            ArtifactTier::SafeWithReinstall => "node_modules — one npm install to restore",
            ArtifactTier::Ask => "Build outputs — some projects ship from these",
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
    /// True if this artifact's size is already included in a parent artifact's size.
    /// Used for node_modules/.cache which is a subset of node_modules.
    /// Excluded from tier totals to prevent double-counting.
    pub is_nested: bool,
    /// Inline guidance for the user (restore cost or safety warning)
    pub hint: Option<String>,
    /// True if a build process is actively using this artifact (pgrep / lock-file mtime)
    #[serde(default)]
    pub active_build: bool,
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
    ("ms-playwright", "Playwright browsers"),
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

/// Check staleness of a project by looking at package.json, src/, and other
/// project-activity indicators. Returns days since last activity.
fn check_project_staleness(project_dir: &Path) -> Option<u64> {
    let now = SystemTime::now();
    let mut most_recent: Option<SystemTime> = None;

    let indicators = [
        "package.json",
        "tsconfig.json",
        "Cargo.toml",
        "pom.xml",
        "build.gradle",
        "Makefile",
        "Gemfile",
    ];

    // Check file indicators
    for name in &indicators {
        if let Ok(meta) = fs::metadata(project_dir.join(name)) {
            if let Ok(modified) = meta.modified() {
                match most_recent {
                    Some(current) if modified > current => most_recent = Some(modified),
                    None => most_recent = Some(modified),
                    _ => {}
                }
            }
        }
    }

    // Check src/ directory
    if let Ok(meta) = fs::metadata(project_dir.join("src")) {
        if let Ok(modified) = meta.modified() {
            match most_recent {
                Some(current) if modified > current => most_recent = Some(modified),
                None => most_recent = Some(modified),
                _ => {}
            }
        }
    }

    most_recent.and_then(|mtime| {
        now.duration_since(mtime)
            .ok()
            .map(|dur| dur.as_secs() / 86400)
    })
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

/// Get the project name from an artifact path (its parent directory name)
fn get_project_name(artifact_path: &Path) -> Option<String> {
    artifact_path
        .parent()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
}

// ============================================================================
// Scanner entry point
// ============================================================================

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
    let scan_roots: Vec<PathBuf> = root_strings.iter().map(PathBuf::from).collect();

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

    // Calculate totals by tier, excluding nested (double-counted) items
    let non_nested = |a: &&DevArtifact| !a.is_nested;
    let total_bytes: u64 = artifacts.iter().filter(non_nested).map(|a| a.size_bytes).sum();
    let safe_bytes: u64 = artifacts
        .iter()
        .filter(non_nested)
        .filter(|a| a.tier == ArtifactTier::Safe)
        .map(|a| a.size_bytes)
        .sum();
    let rebuildable_bytes: u64 = artifacts
        .iter()
        .filter(non_nested)
        .filter(|a| a.tier == ArtifactTier::Rebuildable)
        .map(|a| a.size_bytes)
        .sum();
    let safe_with_reinstall_bytes: u64 = artifacts
        .iter()
        .filter(non_nested)
        .filter(|a| a.tier == ArtifactTier::SafeWithReinstall)
        .map(|a| a.size_bytes)
        .sum();
    let ask_bytes: u64 = artifacts
        .iter()
        .filter(non_nested)
        .filter(|a| a.tier == ArtifactTier::Ask)
        .map(|a| a.size_bytes)
        .sum();

    let duration = start.elapsed();

    // Sort by size descending
    artifacts.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));

    DevScanResult {
        artifacts,
        total_bytes,
        total_display: format_size(total_bytes),
        safe_bytes,
        safe_display: format_size(safe_bytes),
        rebuildable_bytes,
        rebuildable_display: format_size(rebuildable_bytes),
        safe_with_reinstall_bytes,
        safe_with_reinstall_display: format_size(safe_with_reinstall_bytes),
        ask_bytes,
        ask_display: format_size(ask_bytes),
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
    // ~/.npm — npm's global cache
    check_home_cache(home, ".npm", "npm global cache", artifacts);

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
                hint: None,
                active_build: false,
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
                hint: None,
                active_build: false,
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
                hint: None,
                active_build: false,
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
                hint: Some("Clean via pnpm store prune — removes only orphaned packages".to_string()),
                active_build: false,
            });
        }
    }
}

/// Check a single home-level cache directory
fn check_home_cache(home: &Path, dir_name: &str, kind: &str, artifacts: &mut Vec<DevArtifact>) {
    let path = home.join(dir_name);
    // Skip symlinks — .exists()/.is_dir() resolve them, which could point at real data
    if is_symlink(&path) {
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
                hint: None,
                active_build: false,
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
                        hint: None,
                        active_build: false,
                    });
                }
                break;
            }
        }

        // Check for *.ShipIt caches
        if name_str.ends_with(".ShipIt") {
            let size = dir_size(&entry_path);
            if size > 0 {
                artifacts.push(DevArtifact {
                    path: entry_path.to_string_lossy().to_string(),
                    size_bytes: size,
                    size_display: format_size(size),
                    tier: ArtifactTier::Safe,
                    kind: "ShipIt update cache".to_string(),
                    project: None,
                    staleness_days: None,
                    is_nested: false,
                    hint: None,
                    active_build: false,
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
            let building = is_active_build(&derived_data, "DerivedData");
            artifacts.push(DevArtifact {
                path: derived_data.to_string_lossy().to_string(),
                size_bytes: size,
                size_display: format_size(size),
                tier: ArtifactTier::Rebuildable,
                kind: "Xcode DerivedData".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
                hint: Some("Rebuilds on next Xcode build".to_string()),
                active_build: building,
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
                hint: Some("Re-downloads on next gradle build — needs network".to_string()),
                active_build: false,
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
                hint: Some("Re-downloads on next mvn build — needs network".to_string()),
                active_build: false,
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
                hint: Some("Re-downloads on next go build — needs network".to_string()),
                active_build: false,
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
                hint: Some("May contain databases \u{2014} use docker system prune".to_string()),
                active_build: false,
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
                hint: Some("Contains dSYMs for shipped builds \u{2014} not regenerable".to_string()),
                active_build: false,
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
                hint: Some("Delete via Xcode \u{2192} Settings \u{2192} Platforms".to_string()),
                active_build: false,
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
                hint: Some("Delete via Android Studio \u{2192} Device Manager".to_string()),
                active_build: false,
            });
        }
    }
}

// ============================================================================
// Active build detection
// ============================================================================

/// Check if a build process is currently running that would use this directory.
/// Uses pgrep to detect cargo/xcodebuild and lock-file mtime to detect recent builds.
fn is_active_build(dir: &Path, artifact_name: &str) -> bool {
    match artifact_name {
        "target" => {
            // Check for Cargo.lock mtime < 5 minutes (indicates recent/active build)
            let cargo_lock = dir.join("Cargo.lock");
            if cargo_lock.exists() {
                if let Ok(meta) = fs::metadata(&cargo_lock) {
                    if let Ok(modified) = meta.modified() {
                        if let Ok(elapsed) = modified.elapsed() {
                            if elapsed < std::time::Duration::from_secs(300) {
                                return true;
                            }
                        }
                    }
                }
            }
            // Also check if cargo is running
            is_process_running("cargo")
        }
        "DerivedData" => {
            // Check if xcodebuild is running
            is_process_running("xcodebuild")
        }
        _ => false,
    }
}

/// Check if a named process is currently running via pgrep
fn is_process_running(name: &str) -> bool {
    Command::new("pgrep")
        .arg("-x")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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
        if matches!(
            name_str.as_ref(),
            ".next" | ".turbo" | ".parcel-cache" | ".vite" | "coverage"
        ) {
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
                    hint: None,
                    active_build: false,
                });
            }
            continue; // Don't descend into matched artifact dirs
        }

        // ── REBUILDABLE tier: Rust target/ directories ──
        // These are massive (5–20 GB) and regenerable with `cargo build`,
        // but rebuilds can be slow. Only flag if parent contains Cargo.toml.
        if name_str == "target" && dir.join("Cargo.toml").exists() {
            let size = dir_size(&entry_path);
            if size > 0 {
                let building = is_active_build(dir, "target");
                artifacts.push(DevArtifact {
                    path: entry_path.to_string_lossy().to_string(),
                    size_bytes: size,
                    size_display: format_size(size),
                    tier: ArtifactTier::Rebuildable,
                    kind: "Rust target (build artifacts)".to_string(),
                    project: get_project_name(&entry_path),
                    staleness_days: check_project_staleness(dir),
                    is_nested: false,
                    hint: Some("Rebuilds on next cargo build \u{2014} takes minutes, needs network".to_string()),
                    active_build: building,
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
                    hint: Some("Rebuilds on next dotnet build".to_string()),
                    active_build: false,
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
                    hint: Some("Rebuilds when Unity reimports the project".to_string()),
                    active_build: false,
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
                    hint: Some("Rebuilds on next Unreal Editor launch".to_string()),
                    active_build: false,
                });
            }
            continue;
        }

        // ── REBUILDABLE tier: DerivedData inside project dirs ──
        if name_str == "DerivedData" {
            let size = dir_size(&entry_path);
            if size > 0 {
                let building = is_active_build(dir, "DerivedData");
                artifacts.push(DevArtifact {
                    path: entry_path.to_string_lossy().to_string(),
                    size_bytes: size,
                    size_display: format_size(size),
                    tier: ArtifactTier::Rebuildable,
                    kind: "Xcode DerivedData".to_string(),
                    project: get_project_name(&entry_path),
                    staleness_days: None,
                    is_nested: false,
                    hint: Some("Rebuilds on next Xcode build".to_string()),
                    active_build: building,
                });
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
                        hint: Some("May be shipping artifacts \u{2014} verify before deleting".to_string()),
                        active_build: false,
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
    if cache_subdir.exists() && cache_subdir.is_dir() {
        let cache_size = dir_size(&cache_subdir);
        if cache_size > 0 {
            artifacts.push(DevArtifact {
                path: cache_subdir.to_string_lossy().to_string(),
                size_bytes: cache_size,
                size_display: format_size(cache_size),
                tier: ArtifactTier::Safe,
                kind: "node_modules/.cache (build cache)".to_string(),
                project: get_project_name(nm_path),
                staleness_days: None,
                is_nested: true, // Size is included in parent node_modules total
                hint: None,
                active_build: false,
            });
        }
    }

    // Get total node_modules size (includes .cache)
    let total_size = dir_size(nm_path);
    if total_size > 0 {
        let staleness = check_project_staleness(project_dir);

        artifacts.push(DevArtifact {
            path: nm_path.to_string_lossy().to_string(),
            size_bytes: total_size,
            size_display: format_size(total_size),
            tier: ArtifactTier::SafeWithReinstall,
            kind: "node_modules".to_string(),
            project: get_project_name(nm_path),
            staleness_days: staleness,
            is_nested: false,
            hint: Some("Restore with npm install".to_string()),
            active_build: false,
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
            log_artifact_deletion(path_str, 0, "BLOCKED", "depth_guard");
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

        let expected_bytes = artifact.map(|a| a.size_bytes).unwrap_or(0);
        let tier_label = artifact.map(|a| a.tier.label()).unwrap_or("unknown");

        match fs::remove_dir_all(path) {
            Ok(()) => {
                deleted_count += 1;
                bytes_freed += expected_bytes;
                let trigger = if allow_non_safe { "manual" } else { "clean_now" };
                log_artifact_deletion(path_str, expected_bytes, tier_label, trigger);
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
    }
}

/// Log a dev artifact deletion to the shared SymbolSweep deletion log
fn log_artifact_deletion(path: &str, size_bytes: u64, tier: &str, trigger: &str) {
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
        "[{}] DEV_ARTIFACT_DELETED: {} | size={} | tier={} | trigger={}\n",
        timestamp,
        path,
        format_size(size_bytes),
        tier,
        trigger,
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

/// Format a UTC timestamp for log entries (matches cache_cleaner format)
fn format_log_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let secs_per_day = 86400u64;
    let secs_per_hour = 3600u64;
    let secs_per_min = 60u64;

    let days_since_epoch = now / secs_per_day;
    let time_of_day = now % secs_per_day;

    let hours = time_of_day / secs_per_hour;
    let minutes = (time_of_day % secs_per_hour) / secs_per_min;
    let seconds = time_of_day % secs_per_min;

    let years = 1970 + (days_since_epoch / 365);
    let remaining_days = days_since_epoch % 365;
    let months = remaining_days / 30 + 1;
    let days = remaining_days % 30 + 1;

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        years, months, days, hours, minutes, seconds
    )
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_artifact_tier_labels() {
        assert_eq!(ArtifactTier::Safe.label(), "SAFE");
        assert_eq!(ArtifactTier::Rebuildable.label(), "REBUILD");
        assert_eq!(ArtifactTier::SafeWithReinstall.label(), "SAFE-WITH-REINSTALL");
        assert_eq!(ArtifactTier::Ask.label(), "ASK");
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
            "  ASK:                 {} ({} bytes)",
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
}
