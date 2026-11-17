//! The packed binary format: a launcher with a payload and a footer
//! appended.
//!
//! ```text
//! [ launcher binary ][ payload: tar.zst ][ footer: 58 bytes ]
//! ```
//!
//! The footer (little-endian) is `magic (8) | version (2) | payload_offset
//! (8) | payload_len (8) | sha256 (32)`. The sha256 covers the compressed
//! payload bytes; it names the cache folder and is checked whenever the
//! payload is unpacked.
//!
//! The payload is a tar archive, compressed with zstd, laid out as:
//!
//! - `app.toml`: the app's config, with `sidecar.path` / `static.root`
//!   rewritten to point into `app/`.
//! - `pack.toml`: what `pack` recorded ([`PackInfo`]).
//! - `icon.<ext>` (optional): the app icon.
//! - `app/`: the sidecar (one file or a folder) or the static site.
//!
//! `app.toml` and `pack.toml` come first so a launcher can read them
//! without decompressing the rest.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Marks a packed exe. Absent = a plain launcher (the CLI).
pub const MAGIC: [u8; 8] = *b"PRTHOLE\0";
/// Footer format version this build writes and reads.
pub const VERSION: u16 = 1;
pub const FOOTER_LEN: usize = 58;

/// Payload entry names.
pub const APP_TOML: &str = "app.toml";
pub const PACK_TOML: &str = "pack.toml";
pub const APP_DIR: &str = "app";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Footer {
    pub version: u16,
    pub payload_offset: u64,
    pub payload_len: u64,
    pub sha256: [u8; 32],
}

impl Footer {
    pub fn encode(&self) -> [u8; FOOTER_LEN] {
        let mut out = [0u8; FOOTER_LEN];
        out[0..8].copy_from_slice(&MAGIC);
        out[8..10].copy_from_slice(&self.version.to_le_bytes());
        out[10..18].copy_from_slice(&self.payload_offset.to_le_bytes());
        out[18..26].copy_from_slice(&self.payload_len.to_le_bytes());
        out[26..58].copy_from_slice(&self.sha256);
        out
    }

    /// Parse the last [`FOOTER_LEN`] bytes of a file. `Ok(None)` if the
    /// magic is absent (not a packed exe).
    pub fn decode(bytes: &[u8; FOOTER_LEN]) -> Result<Option<Self>> {
        if bytes[0..8] != MAGIC {
            return Ok(None);
        }
        let u64_at = |i: usize| u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        let footer = Footer {
            version: u16::from_le_bytes([bytes[8], bytes[9]]),
            payload_offset: u64_at(10),
            payload_len: u64_at(18),
            sha256: bytes[26..58].try_into().unwrap(),
        };
        if footer.version != VERSION {
            bail!(
                "this app was packed with footer format {}, but this launcher reads format {VERSION}",
                footer.version
            );
        }
        Ok(Some(footer))
    }

    /// The sha256 as lowercase hex.
    pub fn sha256_hex(&self) -> String {
        hex(&self.sha256)
    }

    /// The cache folder name: the first 16 hex digits of the sha256.
    pub fn cache_key(&self) -> String {
        self.sha256_hex()[..16].to_owned()
    }
}

/// Read the footer of the file at `path`. `Ok(None)` for a file without
/// one; an error if the footer is present but does not fit the file.
pub fn read_footer(path: &Path) -> Result<Option<Footer>> {
    let mut file =
        File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    read_footer_from(&mut file).with_context(|| format!("{} is damaged", path.display()))
}

fn read_footer_from(file: &mut File) -> Result<Option<Footer>> {
    let file_len = file.metadata()?.len();
    if file_len < FOOTER_LEN as u64 {
        return Ok(None);
    }
    file.seek(SeekFrom::End(-(FOOTER_LEN as i64)))?;
    let mut bytes = [0u8; FOOTER_LEN];
    file.read_exact(&mut bytes)?;
    let Some(footer) = Footer::decode(&bytes)? else {
        return Ok(None);
    };
    // Unsigned files end with the footer. (Signed Windows exes, where the
    // certificate table follows it, are a v0.5 concern.)
    let end = footer
        .payload_offset
        .checked_add(footer.payload_len)
        .and_then(|e| e.checked_add(FOOTER_LEN as u64));
    if end != Some(file_len) {
        bail!(
            "the payload footer says the payload is {} bytes at offset {}, which does not fit a {file_len}-byte file",
            footer.payload_len,
            footer.payload_offset
        );
    }
    Ok(Some(footer))
}

/// Recorded by `pack` in `pack.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackInfo {
    /// The Porthole version that packed the app.
    pub porthole_version: String,
    /// Allow the web inspector in the packed app (`pack --devtools`).
    pub devtools: bool,
    /// Number of files under `app/`.
    pub files: u64,
    /// Total size of the files under `app/` before compression.
    pub unpacked_bytes: u64,
}

/// Counts and hashes the bytes read through it.
struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
    count: u64,
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.count += n as u64;
        Ok(n)
    }
}

/// Counts and hashes the bytes written through it.
pub struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
    count: u64,
}

impl<W: Write> HashingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            count: 0,
        }
    }

    /// The writer, the number of bytes written and their sha256.
    pub fn finish(self) -> (W, u64, [u8; 32]) {
        (self.inner, self.count, self.hasher.finalize().into())
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.count += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

type Source = HashingReader<io::Take<File>>;
type Decoder = zstd::Decoder<'static, BufReader<Source>>;

/// The payload of a packed exe, read straight from the file.
pub struct Payload<'a> {
    exe: &'a Path,
    footer: &'a Footer,
}

impl<'a> Payload<'a> {
    pub fn new(exe: &'a Path, footer: &'a Footer) -> Self {
        Self { exe, footer }
    }

    fn open(&self) -> Result<Source> {
        let mut file = File::open(self.exe)
            .with_context(|| format!("could not open {}", self.exe.display()))?;
        file.seek(SeekFrom::Start(self.footer.payload_offset))?;
        Ok(HashingReader {
            inner: file.take(self.footer.payload_len),
            hasher: Sha256::new(),
            count: 0,
        })
    }

    /// Walk the archive, calling `visit` on each entry until it returns
    /// `false`. Returns whether every entry was visited, and the source,
    /// read to the end in that case so [`Payload::check`] can verify it.
    fn walk_unchecked(
        &self,
        mut visit: impl FnMut(&mut tar::Entry<'_, Decoder>) -> Result<bool>,
    ) -> Result<(bool, Source)> {
        let decoder = zstd::Decoder::new(self.open()?).context("the payload is not zstd data")?;
        let mut archive = tar::Archive::new(decoder);
        for entry in archive
            .entries()
            .context("the payload is not a tar archive")?
        {
            let mut entry = entry.context("the payload's archive is damaged")?;
            if !visit(&mut entry)? {
                return Ok((false, archive.into_inner().finish().into_inner()));
            }
        }
        // Drain the archive's end padding through the decompressor, then
        // any bytes after the end of the zstd frame. Bytes the decoder
        // buffered were already hashed when they were read.
        let mut decoder = archive.into_inner();
        io::copy(&mut decoder, &mut io::sink()).context("the payload is damaged")?;
        let mut rest = decoder.finish();
        io::copy(&mut rest, &mut io::sink())?;
        Ok((true, rest.into_inner()))
    }

    /// [`Payload::walk_unchecked`], then verify the sha256 unless `visit`
    /// stopped early. `Ok(true)` means every entry came from an intact
    /// payload.
    fn walk(
        &self,
        visit: impl FnMut(&mut tar::Entry<'_, Decoder>) -> Result<bool>,
    ) -> Result<bool> {
        let (finished, source) = self.walk_unchecked(visit)?;
        if finished {
            self.check(source)?;
        }
        Ok(finished)
    }

    fn check(&self, source: Source) -> Result<()> {
        let digest: [u8; 32] = source.hasher.finalize().into();
        if source.count != self.footer.payload_len || digest != self.footer.sha256 {
            bail!(
                "the app's payload is damaged (its sha256 is {}, expected {}); download or pack it again",
                hex(&digest),
                self.footer.sha256_hex()
            );
        }
        Ok(())
    }

    /// Read `app.toml` and `pack.toml` from the front of the payload.
    /// They are not verified here: whatever runs the app checks the hash
    /// before using any of its files.
    pub fn read_meta(&self) -> Result<(String, PackInfo)> {
        let (mut app_toml, mut pack_toml) = (None, None);
        self.walk(|entry| {
            let name = entry.path()?.to_string_lossy().into_owned();
            let slot = match name.as_str() {
                APP_TOML => &mut app_toml,
                PACK_TOML => &mut pack_toml,
                _ => return Ok(false),
            };
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            *slot = Some(text);
            Ok(app_toml.is_none() || pack_toml.is_none())
        })?;
        let app_toml = app_toml.context("the payload has no app.toml")?;
        let pack_toml = pack_toml.context("the payload has no pack.toml")?;
        let info = toml::from_str(&pack_toml).context("the payload's pack.toml is invalid")?;
        Ok((app_toml, info))
    }

    /// Unpack everything into `dest` (which must exist), then verify the
    /// sha256. On error `dest` holds a partial copy; the caller deletes it.
    pub fn extract(&self, dest: &Path) -> Result<()> {
        self.walk(|entry| {
            let path = entry.path()?.into_owned();
            check_entry_path(&path)?;
            // `unpack_in` also refuses paths that escape `dest`, including
            // through symlinks unpacked earlier.
            entry.set_preserve_permissions(true);
            entry.set_preserve_mtime(true);
            entry.set_unpack_xattrs(false);
            if !entry
                .unpack_in(dest)
                .with_context(|| format!("could not unpack {}", path.display()))?
            {
                bail!(
                    "the payload entry {} points outside the app",
                    path.display()
                );
            }
            Ok(true)
        })?;
        Ok(())
    }

    /// Load the files under `app/` into memory, keyed by their path below
    /// `app/` with `/` separators, then verify the sha256. `Ok(None)` if
    /// they add up to more than `limit` bytes.
    pub fn load_app_files(&self, limit: u64) -> Result<Option<HashMap<String, Vec<u8>>>> {
        let mut files = HashMap::new();
        let mut total = 0u64;
        let finished = self.walk(|entry| {
            let path = entry.path()?.into_owned();
            check_entry_path(&path)?;
            let Ok(rel) = path.strip_prefix(APP_DIR) else {
                return Ok(true);
            };
            match entry.header().entry_type() {
                tar::EntryType::Regular | tar::EntryType::Continuous => {}
                // Folders are implied by the files in them. Links are
                // unusual in a static site; they only work from disk.
                tar::EntryType::Directory => return Ok(true),
                other => bail!(
                    "{} is a {other:?} entry, which static mode cannot serve from memory",
                    path.display()
                ),
            }
            total += entry.size();
            if total > limit {
                return Ok(false);
            }
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut bytes)?;
            let key = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            files.insert(key, bytes);
            Ok(true)
        })?;
        Ok(finished.then_some(files))
    }

    /// Every entry, for `inspect`, and whether the sha256 matched.
    pub fn list(&self) -> Result<(Vec<ListedEntry>, Result<()>)> {
        let mut out = Vec::new();
        let (_, source) = self.walk_unchecked(|entry| {
            let header = entry.header();
            out.push(ListedEntry {
                path: entry.path()?.to_string_lossy().into_owned(),
                mode: header.mode().unwrap_or(0),
                size: entry.size(),
                kind: header.entry_type(),
                link: entry
                    .link_name()
                    .ok()
                    .flatten()
                    .map(|l| l.to_string_lossy().into_owned()),
            });
            Ok(true)
        })?;
        Ok((out, self.check(source)))
    }
}

pub struct ListedEntry {
    pub path: String,
    pub mode: u32,
    pub size: u64,
    pub kind: tar::EntryType,
    pub link: Option<String>,
}

/// Payload paths must be plain relative paths.
fn check_entry_path(path: &Path) -> Result<()> {
    if path
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        Ok(())
    } else {
        bail!(
            "the payload entry {} is not a plain relative path",
            path.display()
        )
    }
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn footer() -> Footer {
        Footer {
            version: VERSION,
            payload_offset: 1000,
            payload_len: 234,
            sha256: [7; 32],
        }
    }

    #[test]
    fn footer_round_trips_and_is_58_bytes() {
        let bytes = footer().encode();
        assert_eq!(bytes.len(), 58);
        assert_eq!(&bytes[..8], b"PRTHOLE\0");
        assert_eq!(Footer::decode(&bytes).unwrap(), Some(footer()));
        assert_eq!(footer().cache_key(), "0707070707070707");
    }

    #[test]
    fn no_magic_means_not_packed() {
        assert_eq!(Footer::decode(&[0; FOOTER_LEN]).unwrap(), None);
    }

    #[test]
    fn rejects_other_versions() {
        let mut f = footer();
        f.version = 2;
        let err = Footer::decode(&f.encode()).unwrap_err().to_string();
        assert!(err.contains("format 2"), "{err}");
    }

    #[test]
    fn footer_must_fit_the_file() {
        let path = std::env::temp_dir().join(format!("porthole-footer-{}", std::process::id()));
        let mut bytes = vec![0u8; 1000 + 234];
        bytes.extend_from_slice(&footer().encode());
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(read_footer(&path).unwrap(), Some(footer()));

        bytes.insert(0, 0);
        std::fs::write(&path, &bytes).unwrap();
        assert!(read_footer(&path).is_err());

        std::fs::write(&path, b"short").unwrap();
        assert_eq!(read_footer(&path).unwrap(), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn entry_paths_must_be_relative() {
        assert!(check_entry_path(Path::new("app/a/b.txt")).is_ok());
        assert!(check_entry_path(Path::new("./app.toml")).is_ok());
        assert!(check_entry_path(Path::new("../x")).is_err());
        assert!(check_entry_path(Path::new("app/../../x")).is_err());
        assert!(check_entry_path(Path::new("/etc/passwd")).is_err());
    }
}
