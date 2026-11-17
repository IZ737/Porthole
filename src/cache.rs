//! The extract-once cache for packed apps.
//!
//! Each payload unpacks to `<cache dir>/porthole/<first 16 hex of sha256>/`
//! (`%LOCALAPPDATA%`, `~/Library/Caches` or `$XDG_CACHE_HOME`), once per
//! version:
//!
//! 1. If the folder holds a `.complete` marker, use it as is.
//! 2. Otherwise unpack into a sibling `<key>.tmp-<pid>`, verify the sha256,
//!    write `.complete`, and rename it into place. If another instance won
//!    the rename, use its folder and delete ours.
//!
//! Each launch touches `.complete`; [`prune`] deletes sibling folders
//! untouched for 30 days.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

use crate::logs;
use crate::payload::{Footer, Payload};

/// Overrides the cache root (tests, CI, portable setups).
pub const CACHE_DIR_ENV: &str = "PORTHOLE_CACHE_DIR";
pub const COMPLETE_MARKER: &str = ".complete";
/// Cache folders not launched for this long are deleted.
pub const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Leftover temp folders (a crash mid-extraction) older than this are
/// deleted.
const TMP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// `<cache dir>/porthole`, shared by every packed app.
pub fn root() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(CACHE_DIR_ENV) {
        return Ok(PathBuf::from(dir));
    }
    let base = directories::BaseDirs::new().context("could not find the user's home folder")?;
    Ok(base.cache_dir().join("porthole"))
}

/// Where `footer`'s payload unpacks to.
pub fn folder(root: &Path, footer: &Footer) -> PathBuf {
    root.join(footer.cache_key())
}

/// Whether the folder has been fully unpacked.
pub fn is_complete(dir: &Path) -> bool {
    dir.join(COMPLETE_MARKER).is_file()
}

/// Make sure the payload is unpacked at [`folder`] and return that path.
/// `fresh` (`--verify`) deletes and re-extracts an existing copy.
pub fn ensure_extracted(root: &Path, exe: &Path, footer: &Footer, fresh: bool) -> Result<PathBuf> {
    let dir = folder(root, footer);
    if fresh && dir.exists() {
        logs::info!("--verify: removing the cached copy in {}", dir.display());
        fs::remove_dir_all(&dir).with_context(|| format!("could not remove {}", dir.display()))?;
    }
    if is_complete(&dir) {
        touch(&dir);
        logs::info!("using the cached app in {}", dir.display());
        return Ok(dir);
    }

    fs::create_dir_all(root).with_context(|| format!("could not create {}", root.display()))?;
    let tmp = root.join(format!("{}.tmp-{}", footer.cache_key(), std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir(&tmp).with_context(|| format!("could not create {}", tmp.display()))?;

    let started = std::time::Instant::now();
    let result = Payload::new(exe, footer).extract(&tmp).and_then(|()| {
        let marker = format!(
            "sha256 = \"{}\"\nporthole = \"{}\"\n",
            footer.sha256_hex(),
            env!("CARGO_PKG_VERSION")
        );
        fs::write(tmp.join(COMPLETE_MARKER), marker).context("could not write the cache marker")
    });
    if let Err(err) = result {
        let _ = fs::remove_dir_all(&tmp);
        return Err(err.context(format!("could not unpack the app into {}", tmp.display())));
    }

    if let Err(err) = fs::rename(&tmp, &dir) {
        if is_complete(&dir) {
            // Another instance got there first; its copy is as good as ours.
            let _ = fs::remove_dir_all(&tmp);
        } else {
            // A half-deleted or foreign folder without the marker: replace it.
            let _ = fs::remove_dir_all(&dir);
            if let Err(err2) = fs::rename(&tmp, &dir) {
                let _ = fs::remove_dir_all(&tmp);
                return Err(anyhow::Error::new(err2).context(format!(
                    "could not move the unpacked app into {} (first attempt: {err})",
                    dir.display()
                )));
            }
        }
    }
    logs::info!(
        "unpacked the app into {} in {:.2}s",
        dir.display(),
        started.elapsed().as_secs_f64()
    );
    Ok(dir)
}

/// Mark the folder as used now, so [`prune`] keeps it.
pub fn touch(dir: &Path) {
    let marker = dir.join(COMPLETE_MARKER);
    let result = fs::File::options()
        .append(true)
        .open(&marker)
        .and_then(|f| f.set_modified(SystemTime::now()));
    if let Err(err) = result {
        logs::warning!("could not update {}: {err}", marker.display());
    }
}

/// Delete cache folders other than `keep` that have not been used for
/// [`MAX_AGE`], and stale temp folders. Returns what it removed.
pub fn prune(root: &Path, keep: &str, now: SystemTime) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return removed;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if !is_dir || name == keep {
            continue;
        }
        let age_of = |p: &Path| {
            fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .unwrap_or_default()
        };
        let stale = if is_cache_key(&name) {
            let marker = path.join(COMPLETE_MARKER);
            let age = if marker.exists() {
                age_of(&marker)
            } else {
                age_of(&path)
            };
            age > MAX_AGE
        } else if let Some((key, _pid)) = name.split_once(".tmp-") {
            is_cache_key(key) && age_of(&path) > TMP_MAX_AGE
        } else {
            false
        };
        if stale {
            match fs::remove_dir_all(&path) {
                Ok(()) => removed.push(path),
                Err(err) => logs::warning!("could not prune {}: {err}", path.display()),
            }
        }
    }
    removed
}

fn is_cache_key(name: &str) -> bool {
    name.len() == 16 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("porthole-cache-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn prunes_only_old_unused_folders() {
        let root = temp_root("prune");
        let mk = |name: &str, marker: bool| {
            let d = root.join(name);
            fs::create_dir_all(&d).unwrap();
            if marker {
                fs::write(d.join(COMPLETE_MARKER), "").unwrap();
            }
            d
        };
        let current = mk("aaaaaaaaaaaaaaaa", true);
        let recent = mk("bbbbbbbbbbbbbbbb", true);
        let old_tmp = mk("cccccccccccccccc.tmp-123", false);
        let other = mk("not-a-cache-folder", false);

        // Nothing is old yet.
        assert!(prune(&root, "aaaaaaaaaaaaaaaa", SystemTime::now()).is_empty());

        // 31 days on, everything but the current folder and unknown folders goes.
        let later = SystemTime::now() + Duration::from_secs(31 * 24 * 60 * 60);
        let mut removed = prune(&root, "aaaaaaaaaaaaaaaa", later);
        removed.sort();
        assert_eq!(removed, vec![recent, old_tmp]);
        assert!(current.exists() && other.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cache_keys() {
        assert!(is_cache_key("0123456789abcdef"));
        assert!(!is_cache_key("0123456789abcde"));
        assert!(!is_cache_key("0123456789abcdeg"));
    }
}
