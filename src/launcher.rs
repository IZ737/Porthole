//! Which launcher `pack` copies: this executable, one given with
//! `--launcher`, or a prebuilt one for another platform (`--target`),
//! downloaded once from the GitHub release matching this version and
//! checked against the release's `SHA256SUMS`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::cache;
use crate::exe::{self, TARGETS};
use crate::payload::hex;

/// Where releases live. Overridable for mirrors and tests: the launcher is
/// fetched from `<base>/v<version>/<asset>`.
pub const RELEASES_URL_ENV: &str = "PORTHOLE_RELEASES_URL";
pub const DEFAULT_RELEASES_URL: &str = "https://github.com/IZ737/Porthole/releases/download";

/// The release asset holding the launcher for `target`.
pub fn asset_name(version: &str, target: &str) -> String {
    let ext = if exe::target_os(target) == "windows" {
        ".exe"
    } else {
        ""
    };
    format!("porthole-{version}-{target}{ext}")
}

/// The launcher to pack onto.
pub fn resolve(target: Option<&str>, explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    match target {
        None => std::env::current_exe().context("could not find the porthole executable"),
        Some(t) if t == exe::host_target() => {
            std::env::current_exe().context("could not find the porthole executable")
        }
        Some(t) => download(t),
    }
}

fn download(target: &str) -> Result<PathBuf> {
    if !TARGETS.contains(&target) {
        bail!(
            "no prebuilt launcher for {target}; releases have {}. \
             For another platform, build porthole there and pass it with --launcher",
            TARGETS.join(", ")
        );
    }
    let version = env!("CARGO_PKG_VERSION");
    let asset = asset_name(version, target);
    let dir = cache::root()?.join("launchers").join(version);
    let path = dir.join(&asset);
    // Checked against SHA256SUMS before it was moved here.
    if path.is_file() {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;

    let base = std::env::var(RELEASES_URL_ENV).unwrap_or_else(|_| DEFAULT_RELEASES_URL.to_owned());
    let base = format!("{}/v{version}", base.trim_end_matches('/'));
    let url = format!("{base}/{asset}");
    eprintln!("porthole: downloading the {target} launcher from {url}");

    let tmp = dir.join(format!(".{asset}.{}.download", std::process::id()));
    let sums_tmp = dir.join(format!(".SHA256SUMS.{}.download", std::process::id()));
    let result = (|| -> Result<()> {
        fetch(&format!("{base}/SHA256SUMS"), &sums_tmp)?;
        let sums = std::fs::read_to_string(&sums_tmp)?;
        let expected = expected_sha256(&sums, &asset)
            .with_context(|| format!("{base}/SHA256SUMS has no entry for {asset}"))?;
        fetch(&url, &tmp)?;
        let actual = hex(&Sha256::digest(std::fs::read(&tmp)?));
        if actual != expected {
            bail!("{url} has sha256 {actual}, but SHA256SUMS says {expected}");
        }
        let kind = exe::kind(&tmp)?;
        if !kind.runs_on(target) {
            bail!(
                "{url} is a {} launcher, not one for {target}",
                kind.describe()
            );
        }
        std::fs::rename(&tmp, &path)?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(&sums_tmp);
    result.with_context(|| {
        format!(
            "could not get the {target} launcher; download {url} yourself and pass it with --launcher"
        )
    })?;
    Ok(path)
}

/// Download with `curl`, which ships with Windows 10 and later, macOS and
/// most Linux systems (this keeps an HTTP client out of every packed app).
fn fetch(url: &str, dest: &Path) -> Result<()> {
    let status = Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location"])
        .args(["--retry", "3", "--output"])
        .arg(dest)
        .arg(url)
        .status()
        .context("could not run curl")?;
    if !status.success() {
        bail!("downloading {url} failed (curl {status})");
    }
    Ok(())
}

/// The hash for `asset` in a `sha256sum`-style file.
fn expected_sha256(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim_start().trim_start_matches('*');
        (name == asset && hash.len() == 64).then(|| hash.to_ascii_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_names() {
        assert_eq!(
            asset_name("0.5.0", "x86_64-pc-windows-msvc"),
            "porthole-0.5.0-x86_64-pc-windows-msvc.exe"
        );
        assert_eq!(
            asset_name("0.5.0", "aarch64-apple-darwin"),
            "porthole-0.5.0-aarch64-apple-darwin"
        );
    }

    #[test]
    fn reads_sha256sums() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!("{a}  porthole-x\n{b} *porthole-y.exe\nbad line\n");
        assert_eq!(expected_sha256(&sums, "porthole-x"), Some(a));
        assert_eq!(
            expected_sha256(&sums, "porthole-y.exe"),
            Some("b".repeat(64))
        );
        assert_eq!(expected_sha256(&sums, "porthole-z"), None);
    }

    #[test]
    fn unknown_targets_are_refused() {
        let err = format!(
            "{:#}",
            resolve(Some("riscv64gc-unknown-linux-gnu"), None).unwrap_err()
        );
        assert!(err.contains("--launcher"), "{err}");
        assert_eq!(
            resolve(Some(exe::host_target()), None).unwrap(),
            std::env::current_exe().unwrap()
        );
    }
}
