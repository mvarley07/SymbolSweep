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
    /// node_modules — one npm install to restore
    SafeWithReinstall,
    /// dist/build/out — some projects ship from these
    Ask,
}

impl ArtifactTier {
    pub fn label(&self) -> &'static str {
        match self {
            ArtifactTier::Safe => "SAFE",
            ArtifactTier::SafeWithReinstall => "SAFE-WITH-REINSTALL",
            ArtifactTier::Ask => "ASK",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            ArtifactTier::Safe => "Caches — regenerate automatically",
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

    // Phase 4: Project roots
    let existing_roots: Vec<PathBuf> = scan_roots
        .iter()
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
    if yarn_cache.exists() && yarn_cache.is_dir() {
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
            });
        }
    }

    // ~/.bun/install/cache
    let bun_cache = home.join(".bun").join("install").join("cache");
    if bun_cache.exists() && bun_cache.is_dir() {
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
            });
        }
    }

    // ~/.cargo/registry (Rust crate downloads)
    let cargo_registry = home.join(".cargo").join("registry");
    if cargo_registry.exists() && cargo_registry.is_dir() {
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
            });
        }
    }
}

/// Check a single home-level cache directory
fn check_home_cache(home: &Path, dir_name: &str, kind: &str, artifacts: &mut Vec<DevArtifact>) {
    let path = home.join(dir_name);
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

    if derived_data.exists() && derived_data.is_dir() {
        let size = dir_size(&derived_data);
        if size > 0 {
            artifacts.push(DevArtifact {
                path: derived_data.to_string_lossy().to_string(),
                size_bytes: size,
                size_display: format_size(size),
                tier: ArtifactTier::Safe,
                kind: "Xcode DerivedData".to_string(),
                project: None,
                staleness_days: None,
                is_nested: false,
            });
        }
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
                });
            }
            continue; // Don't descend into matched artifact dirs
        }

        // ── SAFE tier: DerivedData inside project dirs ──
        if name_str == "DerivedData" {
            let size = dir_size(&entry_path);
            if size > 0 {
                artifacts.push(DevArtifact {
                    path: entry_path.to_string_lossy().to_string(),
                    size_bytes: size,
                    size_display: format_size(size),
                    tier: ArtifactTier::Safe,
                    kind: "Xcode DerivedData".to_string(),
                    project: get_project_name(&entry_path),
                    staleness_days: None,
                    is_nested: false,
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
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_artifact_tier_labels() {
        assert_eq!(ArtifactTier::Safe.label(), "SAFE");
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
}
