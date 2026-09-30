//! Every action that deletes or trashes anything goes through here, and each
//! checks the license first. The UI gate (the unlock sheet) is presentation
//! only: without a stored, validated license these return LICENSE_REQUIRED
//! and remove nothing. Scanning never passes through here and stays free.

use crate::cache_cleaner::{clean_cache, CleanResult};
use crate::dev_scanner::{self, DevArtifact, DevDeleteResult, PurgeResult};
use crate::scheduler::Settings;

/// Clean the symbolication cache (Clean Now). A dry run deletes nothing, so it
/// needs no license.
pub fn clean(settings: &Settings, dry_run: bool) -> Result<CleanResult, String> {
    clean_with(settings, dry_run, |dry_run| clean_cache(dry_run).map_err(|e| e.to_string()))
}

fn clean_with(
    settings: &Settings,
    dry_run: bool,
    cleaner: impl FnOnce(bool) -> Result<CleanResult, String>,
) -> Result<CleanResult, String> {
    settings.require_license(dry_run)?;
    cleaner(dry_run)
}

/// Automatic cache cleaning: runs only when due, and only when licensed
pub fn autoclean(settings: &Settings, cache_size_bytes: u64, now: u64) -> Option<Result<CleanResult, String>> {
    settings.autoclean_due(cache_size_bytes, now).then(|| clean(settings, false))
}

/// Delete dev artifacts: Clean Safe (bulk) or a user-chosen delete (manual)
pub fn delete_artifacts(
    settings: &Settings,
    paths: &[String],
    known: &[DevArtifact],
    manual: bool,
) -> Result<DevDeleteResult, String> {
    settings.require_license(false)?;
    Ok(if manual {
        dev_scanner::delete_dev_artifacts_manual(paths, known)
    } else {
        dev_scanner::delete_dev_artifacts(paths, known)
    })
}

/// Permanently delete the items SymbolSweep moved to Trash
pub fn purge_trash(settings: &Settings, on_progress: &dyn Fn(usize, usize, u64)) -> Result<PurgeResult, String> {
    settings.require_license(false)?;
    Ok(dev_scanner::purge_ss_trash_with_progress(on_progress))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dev_scanner::{stage_trashed_item, ArtifactTier};
    use crate::scheduler::LICENSE_REQUIRED;
    use std::cell::Cell;
    use std::fs;
    use std::path::Path;

    fn unlicensed() -> Settings {
        Settings::default()
    }

    fn licensed() -> Settings {
        let mut s = Settings::default();
        s.license_key = Some("KEY".to_string());
        s.license_instance_id = Some("INSTANCE".to_string());
        s.license_last_validated = 1;
        s
    }

    fn artifact(path: &Path, tier: ArtifactTier) -> DevArtifact {
        DevArtifact {
            path: path.to_string_lossy().to_string(),
            size_bytes: 2 * 1024 * 1024,
            size_display: "2 MB".to_string(),
            tier,
            kind: "test".to_string(),
            project: None,
            staleness_days: None,
            is_nested: false,
            hint: None,
            active_build: false,
            in_use: None,
        }
    }

    /// A deletable fixture: an old Safe cache and a Rebuild target
    fn fixtures(name: &str) -> (std::path::PathBuf, DevArtifact, DevArtifact) {
        let tmp = std::env::temp_dir().join(format!("ss-gate-{}", name));
        let _ = fs::remove_dir_all(&tmp);
        let safe = tmp.join("safe-cache");
        let target = tmp.join("proj").join("target");
        for dir in [&safe, &target] {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("data"), vec![0u8; 64]).unwrap();
        }
        // Old enough that no in-use guard applies
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 24 * 3600);
        fs::File::open(&safe).unwrap().set_modified(old).unwrap();
        (tmp, artifact(&safe, ArtifactTier::Safe), artifact(&target, ArtifactTier::Rebuildable))
    }

    #[test]
    fn test_cache_clean_without_license_removes_nothing() {
        let called = Cell::new(false);
        let spy = |_dry: bool| {
            called.set(true);
            Err("cleaner ran".to_string())
        };
        let result = clean_with(&unlicensed(), false, spy);
        assert_eq!(result.unwrap_err(), LICENSE_REQUIRED);
        assert!(!called.get(), "the cleaner must not run without a license");

        // A dry run deletes nothing, so it's allowed; so is a licensed clean
        let dry = Cell::new(false);
        let _ = clean_with(&unlicensed(), true, |_| { dry.set(true); Err("dry".to_string()) });
        assert!(dry.get());
        let paid = Cell::new(false);
        let _ = clean_with(&licensed(), false, |_| { paid.set(true); Err("paid".to_string()) });
        assert!(paid.get());
    }

    #[test]
    fn test_autoclean_without_license_removes_nothing() {
        let mut s = unlicensed();
        s.auto_clean_on_threshold = true;
        s.auto_clean_threshold = 1;
        assert!(autoclean(&s, u64::MAX, 1_000_000).is_none(), "autoclean must not run without a license");
    }

    #[test]
    fn test_artifact_deletes_without_license_remove_nothing() {
        let (tmp, safe, target) = fixtures("delete");
        let known = vec![safe.clone(), target.clone()];

        for manual in [false, true] {
            let paths = if manual { vec![target.path.clone()] } else { vec![safe.path.clone()] };
            let result = delete_artifacts(&unlicensed(), &paths, &known, manual);
            assert_eq!(result.unwrap_err(), LICENSE_REQUIRED, "manual={}", manual);
        }
        assert!(Path::new(&safe.path).join("data").exists(), "Clean Safe removed nothing");
        assert!(Path::new(&target.path).join("data").exists(), "manual delete removed nothing");

        // Licensed, the same bulk delete goes through
        let done = delete_artifacts(&licensed(), &[safe.path.clone()], &known, false).unwrap();
        assert_eq!(done.deleted_count, 1);
        assert!(!Path::new(&safe.path).exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_trash_purge_without_license_removes_nothing() {
        let item = stage_trashed_item("gate-purge");
        let result = purge_trash(&unlicensed(), &|_, _, _| {});
        assert_eq!(result.unwrap_err(), LICENSE_REQUIRED);
        assert!(item.join("data").exists(), "purge removed nothing");

        let done = purge_trash(&licensed(), &|_, _, _| {}).unwrap();
        assert_eq!(done.purged_count, 1);
        assert!(!item.exists());
    }
}
