//! `pack --macos-app`: a macOS `.app` bundle.
//!
//! `codesign` refuses Mach-O files with data appended, so a bundle keeps
//! the payload out of the executable:
//!
//! ```text
//! MyApp.app/Contents/
//!   Info.plist
//!   MacOS/MyApp                  the launcher, unchanged
//!   Resources/payload.tar.zst    payload + footer (payload at offset 0)
//!   Resources/AppIcon.icns       from app.icon, if it is a PNG
//! ```
//!
//! A launcher that finds no footer on itself looks for
//! `../Resources/payload.tar.zst` when it runs from `Contents/MacOS`.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::Config;
use crate::icon;
use crate::payload::{self, FOOTER_LEN, Footer};

pub const PAYLOAD_FILE: &str = "payload.tar.zst";
const ICON_FILE: &str = "AppIcon.icns";

/// The bundle's payload file, for a launcher running at `exe` inside one.
pub fn payload_for_exe(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    if macos.file_name()? != "MacOS" || contents.file_name()? != "Contents" {
        return None;
    }
    let payload = contents.join("Resources").join(PAYLOAD_FILE);
    payload.is_file().then_some(payload)
}

/// The payload file of the bundle at `app`.
pub fn payload_in_bundle(app: &Path) -> Option<PathBuf> {
    let payload = app.join("Contents/Resources").join(PAYLOAD_FILE);
    payload.is_file().then_some(payload)
}

/// The bundle's executable (the only file in `Contents/MacOS`).
pub fn executable_in_bundle(app: &Path) -> Option<PathBuf> {
    fs::read_dir(app.join("Contents/MacOS"))
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.is_file())
}

/// Write the bundle at `dest` (which must not exist). `write_payload`
/// writes the compressed payload and returns its length and sha256.
/// Returns the footer and the bundle's total size.
pub fn write(
    dest: &Path,
    config: &Config,
    launcher: &Path,
    launcher_len: u64,
    icon_png: Option<&[u8]>,
    write_payload: impl FnOnce(BufWriter<File>) -> Result<(BufWriter<File>, u64, [u8; 32])>,
) -> Result<(Footer, u64)> {
    let _ = fs::remove_dir_all(dest);
    let contents = dest.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    fs::create_dir_all(&macos)?;
    fs::create_dir_all(&resources)?;

    let exe_name = executable_name(&config.app.name);
    let exe_path = macos.join(&exe_name);
    let mut out = File::create(&exe_path)?;
    io::copy(&mut File::open(launcher)?.take(launcher_len), &mut out)
        .context("could not copy the launcher")?;
    out.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&exe_path, fs::Permissions::from_mode(0o755))?;
    }

    let out = BufWriter::new(File::create(resources.join(PAYLOAD_FILE))?);
    let (mut out, payload_len, sha256) = write_payload(out)?;
    let footer = Footer {
        version: payload::VERSION,
        payload_offset: 0,
        payload_len,
        sha256,
    };
    out.write_all(&footer.encode())?;
    out.into_inner().map_err(|e| e.into_error())?.sync_all()?;

    let icns = icon_png.and_then(|png| {
        let icns = icns(png);
        if icns.is_none() {
            eprintln!(
                "porthole: warning: `app.icon` should be a square PNG of 16, 32, 64, 128, 256, \
                 512 or 1024 pixels to become the Dock icon; the bundle has none"
            );
        }
        icns
    });
    if let Some(icns) = &icns {
        fs::write(resources.join(ICON_FILE), icns)?;
    }
    fs::write(
        contents.join("Info.plist"),
        info_plist(config, &exe_name, icns.is_some()),
    )?;
    fs::write(contents.join("PkgInfo"), "APPL????")?;

    let total = launcher_len + payload_len + FOOTER_LEN as u64;
    Ok((footer, total))
}

/// The executable's file name: the app's name, minus characters that
/// are awkward in a path.
fn executable_name(app_name: &str) -> String {
    let name: String = app_name
        .chars()
        .map(|c| {
            if matches!(c, '/' | ':' | '\\') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim().trim_start_matches('.').to_owned();
    if name.is_empty() {
        "app".to_owned()
    } else {
        name
    }
}

/// `CFBundleIdentifier` allows letters, digits, `-` and `.`.
fn bundle_id(app_id: &str) -> String {
    app_id
        .chars()
        .map(|c| if c == '_' { '-' } else { c })
        .collect()
}

fn info_plist(config: &Config, exe_name: &str, has_icon: bool) -> String {
    let version = config.app.version.as_deref().unwrap_or("1.0.0");
    let mut entries = vec![
        ("CFBundleDevelopmentRegion", "en".to_owned()),
        ("CFBundleDisplayName", config.app.name.clone()),
        ("CFBundleExecutable", exe_name.to_owned()),
        ("CFBundleIdentifier", bundle_id(&config.app.id)),
        ("CFBundleInfoDictionaryVersion", "6.0".to_owned()),
        ("CFBundleName", config.app.name.clone()),
        ("CFBundlePackageType", "APPL".to_owned()),
        ("CFBundleShortVersionString", version.to_owned()),
        ("CFBundleVersion", version.to_owned()),
        ("LSMinimumSystemVersion", "10.15".to_owned()),
    ];
    if has_icon {
        entries.push(("CFBundleIconFile", "AppIcon".to_owned()));
    }
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    for (key, value) in entries {
        out.push_str(&format!(
            "\t<key>{key}</key>\n\t<string>{}</string>\n",
            xml_escape(&value)
        ));
    }
    out.push_str("\t<key>NSHighResolutionCapable</key>\n\t<true/>\n</dict>\n</plist>\n");
    out
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// An `.icns` holding the PNG as is (macOS 10.7+ reads PNG entries), or
/// `None` if its size has no icns slot.
fn icns(png: &[u8]) -> Option<Vec<u8>> {
    let (w, h) = icon::png_size(png)?;
    let kind: &[u8; 4] = match (w == h).then_some(w)? {
        16 => b"icp4",
        32 => b"icp5",
        64 => b"icp6",
        128 => b"ic07",
        256 => b"ic08",
        512 => b"ic09",
        1024 => b"ic10",
        _ => return None,
    };
    let entry_len = 8 + png.len() as u32;
    let mut out = Vec::with_capacity(8 + entry_len as usize);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&(8 + entry_len).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(&entry_len.to_be_bytes());
    out.extend_from_slice(png);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_plist() {
        assert_eq!(executable_name("My App"), "My App");
        assert_eq!(executable_name("a/b:c"), "a-b-c");
        assert_eq!(executable_name(" .. "), "app");
        assert_eq!(bundle_id("com.example.my_app"), "com.example.my-app");
        let config = Config::parse(
            "[app]\nname = \"A & B\"\nid = \"a_b\"\nversion = \"2.1\"\n[static]\nroot = \"w\"\n",
            Path::new("."),
        )
        .unwrap();
        let plist = info_plist(&config, "A & B", true);
        assert!(plist.contains("<string>A &amp; B</string>"));
        assert!(plist.contains("<key>CFBundleIdentifier</key>\n\t<string>a-b</string>"));
        assert!(plist.contains("<string>2.1</string>"));
        assert!(plist.contains("CFBundleIconFile"));
    }

    #[test]
    fn icns_wraps_the_png() {
        let png = icon::tests::sample_png(32, 32);
        let icns = icns(&png).unwrap();
        assert_eq!(&icns[..4], b"icns");
        assert_eq!(
            u32::from_be_bytes(icns[4..8].try_into().unwrap()) as usize,
            icns.len()
        );
        assert_eq!(&icns[8..12], b"icp5");
        assert_eq!(&icns[16..], &png[..]);
        assert!(super::icns(&icon::tests::sample_png(30, 30)).is_none());
        assert!(super::icns(&icon::tests::sample_png(32, 64)).is_none());
    }

    #[test]
    fn finds_the_payload_from_inside_the_bundle() {
        let app =
            std::env::temp_dir().join(format!("porthole-bundle-{}/X.app", std::process::id()));
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::create_dir_all(app.join("Contents/Resources")).unwrap();
        let exe = app.join("Contents/MacOS/X");
        fs::write(&exe, "").unwrap();
        assert_eq!(payload_for_exe(&exe), None);
        fs::write(app.join("Contents/Resources").join(PAYLOAD_FILE), "").unwrap();
        assert_eq!(
            payload_for_exe(&exe),
            Some(app.join("Contents/Resources").join(PAYLOAD_FILE))
        );
        assert_eq!(payload_in_bundle(&app), payload_for_exe(&exe));
        assert_eq!(executable_in_bundle(&app), Some(exe.clone()));
        assert_eq!(payload_for_exe(Path::new("/usr/bin/X")), None);
        let _ = fs::remove_dir_all(app.parent().unwrap());
    }
}
