//! `porthole pack` and `porthole inspect`.
//!
//! `pack` copies a launcher (this one, or a prebuilt one for another
//! platform with `--target`), appends the app as a zstd-compressed tar
//! payload, and ends the file with the footer described in [`payload`].
//! With `--macos-app` it writes a `.app` bundle instead, with the payload
//! in `Contents/Resources` (see [`crate::macos_app`]).

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};

use crate::cache;
use crate::config::{Config, Mode};
use crate::exe;
use crate::launcher;
use crate::macos_app;
use crate::payload::{self, APP_DIR, APP_TOML, FOOTER_LEN, Footer, PACK_TOML, PackInfo, Payload};
use crate::sidecar;
use crate::win_icon;

/// zstd level for `pack`: slow to compress, fast to decompress, and it
/// only runs once.
const ZSTD_LEVEL: i32 = 19;

#[derive(Default)]
pub struct PackOptions {
    pub output: Option<PathBuf>,
    pub devtools: bool,
    /// Pack for another platform (a target triple).
    pub target: Option<String>,
    /// Use this launcher binary instead of this one or a downloaded one.
    pub launcher: Option<PathBuf>,
    /// Write a macOS `.app` bundle.
    pub macos_app: bool,
}

/// One file or folder to put in the payload.
struct Item {
    /// Path inside the archive, `/`-separated.
    name: String,
    source: PathBuf,
    kind: Kind,
}

enum Kind {
    Dir,
    File(u64),
    #[cfg_attr(not(unix), allow(dead_code))]
    Symlink(PathBuf),
}

pub fn pack(config_path: &Path, opts: PackOptions) -> Result<()> {
    let started = Instant::now();
    let launcher = launcher::resolve(opts.target.as_deref(), opts.launcher.as_deref())?;
    let packed = pack_with(config_path, opts, &launcher)?;
    let footer = &packed.footer;
    println!(
        "Packed {} ({}) for {} into {} in {:.1}s",
        packed.config.app.name,
        match packed.config.mode {
            Mode::Sidecar(_) => "sidecar",
            Mode::Static(_) => "static",
        },
        packed.kind.describe(),
        packed.output.display(),
        started.elapsed().as_secs_f64()
    );
    println!(
        "  {} files, {} unpacked -> {} payload; {} total",
        packed.info.files,
        size(packed.info.unpacked_bytes),
        size(footer.payload_len),
        size(packed.total_bytes)
    );
    println!("  sha256 {}", footer.sha256_hex());
    if let Mode::Sidecar(sc) = &packed.config.mode
        && packed.kind.os() != std::env::consts::OS
    {
        println!(
            "  note: the sidecar ({}) must be built for {} too",
            sc.command.as_deref().unwrap_or("app/"),
            packed.kind.os()
        );
    }
    match packed.kind.os() {
        "windows" => {
            println!("  sign it with signtool now if you distribute it (see README, Signing)")
        }
        "macos" if packed.bundle => println!(
            "  sign and notarize the bundle before you distribute it (see README, Signing)"
        ),
        "macos" => println!(
            "  to sign it for distribution, pack with --macos-app instead: codesign can't sign this file"
        ),
        _ => {}
    }
    Ok(())
}

pub struct Packed {
    pub output: PathBuf,
    pub footer: Footer,
    pub config: Config,
    pub info: PackInfo,
    /// The launcher's platform.
    pub kind: exe::Kind,
    /// A `.app` bundle rather than one file.
    pub bundle: bool,
    pub total_bytes: u64,
}

/// Pack `config_path` onto the launcher at `launcher`.
pub fn pack_with(config_path: &Path, opts: PackOptions, launcher: &Path) -> Result<Packed> {
    let config = Config::load(config_path)?;
    let text = fs::read_to_string(config_path)?;
    let base_dir = config_path.parent().unwrap_or(Path::new("."));

    let kind = exe::kind(launcher)?;
    if let Some(target) = &opts.target
        && !kind.runs_on(target)
    {
        bail!(
            "{} is a {} launcher, not one for {target}",
            launcher.display(),
            kind.describe()
        );
    }
    if opts.macos_app && kind.os() != "macos" {
        bail!(
            "--macos-app needs a macOS launcher, and {} is {}; add --target aarch64-apple-darwin \
             (or x86_64-apple-darwin)",
            launcher.display(),
            kind.describe()
        );
    }

    let output = match opts.output {
        Some(o) => o,
        None => PathBuf::from(format!(
            "{}{}",
            config.app.name,
            if opts.macos_app {
                ".app"
            } else if kind.os() == "windows" {
                ".exe"
            } else {
                ""
            }
        )),
    };
    if opts.macos_app && output.extension().is_none_or(|e| e != "app") {
        bail!("with --macos-app, `-o` must end in `.app`");
    }
    let output = std::path::absolute(&output)?;
    let tmp_output = output.with_file_name(format!(
        ".{}.porthole-tmp",
        output
            .file_name()
            .context("`-o` must name a file")?
            .to_string_lossy()
    ));

    // What goes under `app/`, and what `app.toml` will say about it.
    let (source, rewritten_key, rewritten_value) = match &config.mode {
        Mode::Sidecar(s) => {
            // Fails early on a missing command, as `run` would. (Only for
            // this platform: a cross-packed sidecar is the user's call.)
            if opts.target.is_none() {
                sidecar::resolve_program(&s.path, s.command.as_deref())?;
            } else if !s.path.exists() {
                bail!("`sidecar.path` {} does not exist", s.path.display());
            }
            let value = if s.path.is_dir() {
                APP_DIR.to_owned()
            } else {
                let name = s
                    .path
                    .file_name()
                    .context("`sidecar.path` has no file name")?;
                format!("{APP_DIR}/{}", name.to_string_lossy())
            };
            (s.path.clone(), ("sidecar", "path"), value)
        }
        Mode::Static(s) => {
            if !s.root.is_dir() {
                bail!("`static.root` {} is not a folder", s.root.display());
            }
            if !s.root.join("index.html").is_file() {
                eprintln!(
                    "porthole: warning: {} has no index.html, so `/` will be a 404",
                    s.root.display()
                );
            }
            (s.root.clone(), ("static", "root"), APP_DIR.to_owned())
        }
    };

    let skip = [output.clone(), tmp_output.clone()];
    let mut items = Vec::new();
    if source.is_dir() {
        items.push(Item {
            name: APP_DIR.to_owned(),
            source: source.clone(),
            kind: Kind::Dir,
        });
        walk(&source, APP_DIR, &skip, &mut items)?;
    } else {
        let name = source
            .file_name()
            .context("no file name")?
            .to_string_lossy();
        items.push(Item {
            name: APP_DIR.to_owned(),
            source: source.clone(),
            kind: Kind::Dir,
        });
        items.push(Item {
            name: format!("{APP_DIR}/{name}"),
            source: source.clone(),
            kind: Kind::File(fs::metadata(&source)?.len()),
        });
    }
    let (files, unpacked_bytes) = items.iter().fold((0u64, 0u64), |(n, b), i| match i.kind {
        Kind::File(len) => (n + 1, b + len),
        _ => (n, b),
    });

    // Rewrite app.toml so its paths point into the payload.
    let mut table: toml::Table = toml::from_str(&text)?;
    set(
        &mut table,
        rewritten_key,
        toml::Value::String(rewritten_value),
    );
    let icon = match &config.app.icon {
        Some(icon) => {
            let path = base_dir.join(icon);
            if !path.is_file() {
                bail!("`app.icon` {} does not exist", path.display());
            }
            let name = match path.extension() {
                Some(ext) => format!("icon.{}", ext.to_string_lossy()),
                None => "icon".to_owned(),
            };
            set(
                &mut table,
                ("app", "icon"),
                toml::Value::String(name.clone()),
            );
            Some((name, path))
        }
        None => None,
    };
    let app_toml = format!(
        "# Packed by porthole {}. Paths point into this payload.\n{}",
        env!("CARGO_PKG_VERSION"),
        toml::to_string(&table)?
    );
    // The packed app must parse with this launcher.
    Config::parse(&app_toml, Path::new(".")).context("the rewritten app.toml does not parse")?;
    let info = PackInfo {
        porthole_version: env!("CARGO_PKG_VERSION").to_owned(),
        devtools: opts.devtools,
        files,
        unpacked_bytes,
    };
    let pack_toml = toml::to_string(&info)?;

    // The launcher, minus any payload it carries itself, and minus a
    // signature (which would not cover the packed file anyway).
    let launcher_file_len = fs::metadata(launcher)?.len();
    let mut launcher_len = match payload::read_footer(launcher)? {
        Some(f) => f.payload_offset,
        None => launcher_file_len,
    };
    let pe = exe::pe_header(&mut File::open(launcher)?)?;
    if let Some(end) = pe.as_ref().and_then(|h| h.signed_end(launcher_file_len)) {
        launcher_len = launcher_len.min(end);
    }
    let icon = match &icon {
        Some((name, path)) => Some((name.clone(), fs::read(path)?)),
        None => None,
    };
    let contents = Contents {
        app_toml: &app_toml,
        pack_toml: &pack_toml,
        icon: icon.as_ref(),
        items: &items,
    };

    let result = if opts.macos_app {
        macos_app::write(
            &tmp_output,
            &config,
            launcher,
            launcher_len,
            icon.as_ref().map(|(_, bytes)| bytes.as_slice()),
            |out| write_payload(out, &contents),
        )
    } else {
        (|| -> Result<(Footer, u64)> {
            let mut out = BufWriter::new(File::create(&tmp_output)?);
            let launcher_len = match &pe {
                // Windows: patch the launcher's headers, and add the icon
                // Explorer shows for the file.
                Some(header) => {
                    let mut image = Vec::with_capacity(launcher_len as usize);
                    io::Read::take(File::open(launcher)?, launcher_len)
                        .read_to_end(&mut image)
                        .context("could not read the launcher")?;
                    exe::prepare_windows_copy(&mut image, header);
                    if let Some((_, png)) = &icon {
                        add_windows_icon(&mut image, png);
                    }
                    out.write_all(&image)?;
                    image.len() as u64
                }
                None => {
                    io::copy(
                        &mut io::Read::take(File::open(launcher)?, launcher_len),
                        &mut out,
                    )
                    .context("could not copy the launcher")?;
                    launcher_len
                }
            };
            let (mut out, payload_len, sha256) = write_payload(out, &contents)?;
            let footer = Footer {
                version: payload::VERSION,
                payload_offset: launcher_len,
                payload_len,
                sha256,
            };
            out.write_all(&footer.encode())?;
            out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
            Ok((footer, launcher_len + payload_len + FOOTER_LEN as u64))
        })()
    };
    let (footer, total_bytes) = match result {
        Ok(f) => f,
        Err(err) => {
            let _ = fs::remove_file(&tmp_output);
            let _ = fs::remove_dir_all(&tmp_output);
            return Err(err.context(format!("could not write {}", tmp_output.display())));
        }
    };

    if opts.macos_app {
        if output.exists() {
            if !output.join("Contents/Info.plist").is_file() {
                let _ = fs::remove_dir_all(&tmp_output);
                bail!(
                    "{} exists and is not an app bundle; not replacing it",
                    output.display()
                );
            }
            fs::remove_dir_all(&output)
                .with_context(|| format!("could not replace {}", output.display()))?;
        }
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp_output, fs::Permissions::from_mode(0o755))?;
        }
    }
    fs::rename(&tmp_output, &output)
        .with_context(|| format!("could not write {}", output.display()))?;

    Ok(Packed {
        output,
        footer,
        config,
        info,
        kind,
        bundle: opts.macos_app,
        total_bytes,
    })
}

/// Give a Windows launcher image `png` as its file icon, or warn why not.
fn add_windows_icon(image: &mut Vec<u8>, png: &[u8]) {
    let warn = |why: String| {
        eprintln!("porthole: warning: the .exe file gets no icon in Explorer: {why}");
    };
    match win_icon::images(png) {
        Ok(images) => match win_icon::embed(image, &images) {
            Ok(true) => {}
            Ok(false) => warn("the launcher has no room for another section".into()),
            Err(err) => warn(format!("{err:#}")),
        },
        Err(err) => warn(format!("`app.icon` is not a usable PNG ({err:#})")),
    }
}

/// Everything that goes into the payload.
struct Contents<'a> {
    app_toml: &'a str,
    pack_toml: &'a str,
    icon: Option<&'a (String, Vec<u8>)>,
    items: &'a [Item],
}

/// Write the compressed payload to `out`. Returns `out`, the payload's
/// length and its sha256.
fn write_payload<W: Write>(out: W, c: &Contents) -> Result<(W, u64, [u8; 32])> {
    let mut hashed = payload::HashingWriter::new(out);
    {
        let mut encoder = zstd::Encoder::new(&mut hashed, ZSTD_LEVEL)?;
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        // Multithreading is a nice-to-have; ignore it if unsupported.
        let _ = encoder.multithread(threads as u32);
        let mut tar = tar::Builder::new(encoder);
        tar.mode(tar::HeaderMode::Deterministic);
        append_bytes(&mut tar, APP_TOML, c.app_toml.as_bytes())?;
        append_bytes(&mut tar, PACK_TOML, c.pack_toml.as_bytes())?;
        if let Some((name, bytes)) = c.icon {
            append_bytes(&mut tar, name, bytes)?;
        }
        for item in c.items {
            append(&mut tar, item)
                .with_context(|| format!("could not add {}", item.source.display()))?;
        }
        tar.into_inner()?.finish()?;
    }
    Ok(hashed.finish())
}

/// `porthole inspect <exe>`: print the footer and the payload's contents.
pub fn inspect(path: &Path) -> Result<()> {
    // A `.app` bundle: the payload and the launcher are separate files.
    let (exe, launcher) = if path.is_dir() {
        let payload = macos_app::payload_in_bundle(path).with_context(|| {
            format!(
                "{} is a folder, but not an app bundle made by `pack --macos-app`",
                path.display()
            )
        })?;
        (payload, macos_app::executable_in_bundle(path))
    } else {
        (path.to_path_buf(), Some(path.to_path_buf()))
    };
    let exe = exe.as_path();
    let footer = payload::read_footer(exe)?.with_context(|| {
        format!(
            "{} is not a packed Porthole app (no payload footer)",
            path.display()
        )
    })?;
    let payload = Payload::new(exe, &footer);
    let (entries, verified) = payload.list()?;
    let (app_toml, info) = payload.read_meta()?;

    println!("{}", path.display());
    println!("  footer version   {}", footer.version);
    if let Some(launcher) = &launcher {
        let kind = exe::kind(launcher)?;
        let len = if launcher == exe {
            footer.payload_offset
        } else {
            fs::metadata(launcher)?.len()
        };
        println!("  launcher         {}, {}", size(len), kind.describe());
        let file_len = fs::metadata(launcher)?.len();
        if let Some(pe) = exe::pe_header(&mut File::open(launcher)?)?
            && let Some(at) = pe.signed_end(file_len)
        {
            println!(
                "  signature        Authenticode, {} at offset {at}",
                size(file_len - at)
            );
        }
        if matches!(kind, exe::Kind::Pe { .. }) {
            let mut image = Vec::new();
            File::open(launcher)?.take(len).read_to_end(&mut image)?;
            let sides = match win_icon::read_icon(&image) {
                Ok(icon) if !icon.is_empty() => {
                    icon.iter()
                        .map(|(side, _)| side.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                        + " px"
                }
                Ok(_) => "none".to_owned(),
                Err(err) => format!("unreadable ({err:#})"),
            };
            println!("  file icon        {sides}");
        }
    }
    println!(
        "  payload          {} at offset {}",
        size(footer.payload_len),
        footer.payload_offset
    );
    println!(
        "  sha256           {} ({})",
        footer.sha256_hex(),
        match &verified {
            Ok(()) => "matches".to_owned(),
            Err(_) => "DOES NOT MATCH: the file is damaged".to_owned(),
        }
    );
    if let Ok(root) = cache::root() {
        let dir = cache::folder(&root, &footer);
        let state = if cache::is_complete(&dir) {
            "unpacked"
        } else {
            "not unpacked yet"
        };
        println!("  cache            {} ({state})", dir.display());
    }
    println!(
        "  packed by        porthole {}{}",
        info.porthole_version,
        if info.devtools { ", devtools on" } else { "" }
    );

    match Config::parse(&app_toml, Path::new(".")) {
        Ok(config) => {
            println!("\nApp");
            println!("  name             {}", config.app.name);
            println!("  id               {}", config.app.id);
            match &config.mode {
                Mode::Sidecar(s) => {
                    println!("  mode             sidecar");
                    if let Some(c) = &s.command {
                        println!("  command          {c} {}", s.args.join(" "));
                    }
                }
                Mode::Static(s) => {
                    let where_ = if info.unpacked_bytes <= s.in_memory_limit {
                        "served from memory"
                    } else {
                        "served from the cache folder (over the in-memory limit)"
                    };
                    println!("  mode             static, {where_}");
                    println!(
                        "  SPA fallback     {}",
                        if s.spa_fallback { "on" } else { "off" }
                    );
                }
            }
        }
        Err(err) => println!("\napp.toml does not parse with this porthole: {err:#}"),
    }

    println!(
        "\nPayload ({} files, {} unpacked)",
        info.files,
        size(info.unpacked_bytes)
    );
    for e in &entries {
        let kind = match e.kind {
            tar::EntryType::Directory => 'd',
            tar::EntryType::Symlink => 'l',
            _ => '-',
        };
        let link = e
            .link
            .as_ref()
            .map(|l| format!(" -> {l}"))
            .unwrap_or_default();
        println!(
            "  {kind}{} {:>10}  {}{link}",
            mode_string(e.mode),
            if kind == 'd' {
                String::new()
            } else {
                e.size.to_string()
            },
            e.path
        );
    }
    verified
}

/// Add a folder's contents in sorted order, so the same input packs to the
/// same payload.
fn walk(dir: &Path, prefix: &str, skip: &[PathBuf], items: &mut Vec<Item>) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("could not read {}", dir.display()))?
        .collect::<io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let source = entry.path();
        if skip
            .iter()
            .any(|s| std::path::absolute(&source).is_ok_and(|a| &a == s))
        {
            continue;
        }
        let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
            bail!("{} is not a valid UTF-8 file name", source.display());
        };
        let name = format!("{prefix}/{file_name}");
        let meta = fs::symlink_metadata(&source)?;
        #[cfg(unix)]
        if meta.file_type().is_symlink() {
            items.push(Item {
                name,
                kind: Kind::Symlink(fs::read_link(&source)?),
                source,
            });
            continue;
        }
        // Elsewhere, follow links.
        let meta = if meta.file_type().is_symlink() {
            fs::metadata(&source)?
        } else {
            meta
        };
        if meta.is_dir() {
            items.push(Item {
                name: name.clone(),
                source: source.clone(),
                kind: Kind::Dir,
            });
            walk(&source, &name, skip, items)?;
        } else {
            items.push(Item {
                name,
                source,
                kind: Kind::File(meta.len()),
            });
        }
    }
    Ok(())
}

fn append<W: Write>(tar: &mut tar::Builder<W>, item: &Item) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    match &item.kind {
        Kind::Dir => {
            header
                .set_metadata_in_mode(&fs::metadata(&item.source)?, tar::HeaderMode::Deterministic);
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            tar.append_data(&mut header, &item.name, io::empty())?;
        }
        Kind::File(_) => {
            let file = File::open(&item.source)?;
            let meta = file.metadata()?;
            // Deterministic mode keeps the executable bit and drops owner
            // names; keep real modification times, which tools such as
            // Python's bytecode cache compare against.
            header.set_metadata_in_mode(&meta, tar::HeaderMode::Deterministic);
            if let Ok(mtime) = meta.modified()
                && let Ok(since) = mtime.duration_since(std::time::UNIX_EPOCH)
            {
                header.set_mtime(since.as_secs());
            }
            tar.append_data(&mut header, &item.name, file)?;
        }
        Kind::Symlink(target) => {
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_mode(0o777);
            header.set_size(0);
            tar.append_link(&mut header, &item.name, target)?;
        }
    }
    Ok(())
}

fn append_bytes<W: Write>(tar: &mut tar::Builder<W>, name: &str, bytes: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o644);
    header.set_size(bytes.len() as u64);
    header.set_mtime(tar::DETERMINISTIC_TIMESTAMP);
    tar.append_data(&mut header, name, bytes)?;
    Ok(())
}

/// Set `table[section][key]`.
fn set(table: &mut toml::Table, (section, key): (&str, &str), value: toml::Value) {
    if let Some(toml::Value::Table(t)) = table.get_mut(section) {
        t.insert(key.to_owned(), value);
    }
}

fn mode_string(mode: u32) -> String {
    let mut s = String::with_capacity(9);
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    s
}

/// `1.2 MB`-style sizes.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("porthole-pack-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Pack a small sidecar folder onto a fake launcher and unpack it.
    #[test]
    fn round_trip_through_the_cache() {
        let dir = temp_dir("roundtrip");
        let launcher = dir.join("launcher");
        fs::write(&launcher, b"#!fake launcher\n").unwrap();
        let src = dir.join("backend");
        fs::create_dir_all(src.join("lib/empty")).unwrap();
        fs::write(src.join("run.sh"), "#!/bin/sh\necho hi\n").unwrap();
        fs::write(src.join("lib/data.txt"), "data").unwrap();
        fs::write(dir.join("icon.png"), "png").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(src.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
            std::os::unix::fs::symlink("data.txt", src.join("lib/link.txt")).unwrap();
        }
        fs::write(
            dir.join("app.toml"),
            "# comment\n[app]\nname = \"T\"\nid = \"t\"\nicon = \"icon.png\"\n\
             [sidecar]\npath = \"backend\"\ncommand = \"run.sh\"\n",
        )
        .unwrap();

        let out = dir.join("T.bin");
        let packed = pack_with(
            &dir.join("app.toml"),
            PackOptions {
                output: Some(out.clone()),
                devtools: true,
                ..Default::default()
            },
            &launcher,
        )
        .unwrap();
        let bytes = fs::read(&out).unwrap();
        assert!(bytes.starts_with(b"#!fake launcher\n"));
        let footer = payload::read_footer(&out).unwrap().unwrap();
        assert_eq!(footer, packed.footer);
        assert_eq!(footer.payload_offset, 16);
        assert_eq!(packed.info.files, 2);

        // Packing a packed file reuses only its launcher part.
        let again = pack_with(
            &dir.join("app.toml"),
            PackOptions {
                output: Some(dir.join("T2.bin")),
                devtools: true,
                ..Default::default()
            },
            &out,
        )
        .unwrap();
        assert_eq!(again.footer.payload_offset, 16);

        let payload = Payload::new(&out, &footer);
        let (app_toml, info) = payload.read_meta().unwrap();
        assert!(info.devtools);
        let config = Config::parse(&app_toml, Path::new("/cache/x")).unwrap();
        let Mode::Sidecar(sc) = &config.mode else {
            panic!("not sidecar")
        };
        assert_eq!(sc.path, Path::new("/cache/x/app"));
        assert_eq!(config.app.icon.as_deref(), Some("icon.png"));

        let root = dir.join("cache");
        let got = cache::ensure_extracted(&root, &out, &footer, false).unwrap();
        assert_eq!(got, root.join(footer.cache_key()));
        assert!(cache::is_complete(&got));
        assert_eq!(
            fs::read_to_string(got.join("app/lib/data.txt")).unwrap(),
            "data"
        );
        assert_eq!(fs::read_to_string(got.join("icon.png")).unwrap(), "png");
        assert!(got.join("app/lib/empty").is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(got.join("app/run.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "executable bit kept");
            assert_eq!(
                fs::read_link(got.join("app/lib/link.txt")).unwrap(),
                Path::new("data.txt")
            );
        }

        // A cache hit leaves the folder alone; --verify unpacks afresh.
        fs::write(got.join("app/lib/data.txt"), "tampered").unwrap();
        cache::ensure_extracted(&root, &out, &footer, false).unwrap();
        assert_eq!(
            fs::read_to_string(got.join("app/lib/data.txt")).unwrap(),
            "tampered"
        );
        cache::ensure_extracted(&root, &out, &footer, true).unwrap();
        assert_eq!(
            fs::read_to_string(got.join("app/lib/data.txt")).unwrap(),
            "data"
        );

        // A folder without the marker (a crash mid-delete, say) is replaced.
        fs::remove_dir_all(&got).unwrap();
        fs::create_dir_all(&got).unwrap();
        fs::write(got.join("other"), "x").unwrap();
        cache::ensure_extracted(&root, &out, &footer, false).unwrap();
        assert!(!got.join("other").exists());
        assert!(cache::is_complete(&got));
        let leftovers: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());

        // A damaged payload is refused and leaves nothing behind.
        let mut damaged = bytes.clone();
        let mid = footer.payload_offset as usize + footer.payload_len as usize / 2;
        damaged[mid] ^= 0xff;
        let bad = dir.join("bad.bin");
        fs::write(&bad, &damaged).unwrap();
        let bad_root = dir.join("bad-cache");
        assert!(cache::ensure_extracted(&bad_root, &bad, &footer, false).is_err());
        assert!(!cache::folder(&bad_root, &footer).exists());
        assert!(fs::read_dir(&bad_root).unwrap().next().is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn static_site_loads_into_memory() {
        let dir = temp_dir("static");
        let launcher = dir.join("launcher");
        fs::write(&launcher, b"L").unwrap();
        fs::create_dir_all(dir.join("web/assets")).unwrap();
        fs::write(dir.join("web/index.html"), "<h1>hi</h1>").unwrap();
        fs::write(dir.join("web/assets/app.js"), "console.log(1)").unwrap();
        fs::write(
            dir.join("app.toml"),
            "[app]\nname = \"S\"\nid = \"s\"\n[static]\nroot = \"web\"\n",
        )
        .unwrap();
        let out = dir.join("S.bin");
        let packed = pack_with(
            &dir.join("app.toml"),
            PackOptions {
                output: Some(out.clone()),
                devtools: false,
                ..Default::default()
            },
            &launcher,
        )
        .unwrap();
        let payload = Payload::new(&out, &packed.footer);
        let files = payload.load_app_files(1024).unwrap().unwrap();
        let mut keys: Vec<_> = files.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["assets/app.js", "index.html"]);
        assert_eq!(files["index.html"], b"<h1>hi</h1>");
        // Over the limit: the caller falls back to the cache.
        assert!(payload.load_app_files(5).unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    fn static_app(dir: &Path) -> PathBuf {
        fs::create_dir_all(dir.join("web")).unwrap();
        fs::write(dir.join("web/index.html"), "<h1>hi</h1>").unwrap();
        fs::write(dir.join("icon.png"), crate::icon::tests::sample_png(32, 32)).unwrap();
        let config = dir.join("app.toml");
        fs::write(
            &config,
            "[app]\nname = \"Site\"\nid = \"site\"\nicon = \"icon.png\"\n[static]\nroot = \"web\"\n",
        )
        .unwrap();
        config
    }

    #[test]
    fn windows_launchers_become_gui_programs() {
        let dir = temp_dir("windows");
        let config = static_app(&dir);
        let launcher = dir.join("porthole.exe");
        // A console launcher that was signed: the copy loses the signature.
        let mut pe = crate::exe::tests::fake_pe(3, 5);
        let unsigned_len = pe.len();
        crate::exe::tests::fake_sign(&mut pe);
        fs::write(&launcher, &pe).unwrap();

        let packed = pack_with(
            &config,
            PackOptions {
                target: Some("x86_64-pc-windows-msvc".into()),
                ..Default::default()
            },
            &launcher,
        );
        // No `-o`: the default name gets `.exe` (in the current folder).
        let packed = packed.unwrap();
        assert_eq!(packed.output.file_name().unwrap(), "Site.exe");
        let out = packed.output.clone();
        let mut file = File::open(&out).unwrap();
        let header = exe::pe_header(&mut file).unwrap().unwrap();
        assert_eq!(header.subsystem, 2, "GUI subsystem");
        assert_eq!(header.certificates, (0, 0));
        assert!(packed.footer.payload_offset <= unsigned_len as u64 + 8);
        assert_eq!(
            payload::read_footer(&out).unwrap(),
            Some(packed.footer.clone())
        );
        let _ = fs::remove_file(&out);

        // The wrong platform is refused.
        let err = pack_with(
            &config,
            PackOptions {
                target: Some("x86_64-unknown-linux-gnu".into()),
                output: Some(dir.join("x")),
                ..Default::default()
            },
            &launcher,
        )
        .err()
        .unwrap();
        assert!(format!("{err:#}").contains("not one for"), "{err:#}");
        let err = pack_with(
            &config,
            PackOptions {
                macos_app: true,
                output: Some(dir.join("x.app")),
                ..Default::default()
            },
            &launcher,
        )
        .err()
        .unwrap();
        assert!(format!("{err:#}").contains("macOS launcher"), "{err:#}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn macos_app_bundle() {
        let dir = temp_dir("macos");
        let config = static_app(&dir);
        let launcher = dir.join("porthole");
        let mut macho = vec![0u8; 64];
        macho[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        macho[4..8].copy_from_slice(&0x0100_000cu32.to_le_bytes());
        fs::write(&launcher, &macho).unwrap();

        let app = dir.join("Site.app");
        fs::create_dir_all(&app).unwrap();
        // An existing non-bundle folder is not replaced.
        let opts = || PackOptions {
            macos_app: true,
            output: Some(app.clone()),
            ..Default::default()
        };
        assert!(pack_with(&config, opts(), &launcher).is_err());
        fs::remove_dir_all(&app).unwrap();

        let packed = pack_with(&config, opts(), &launcher).unwrap();
        assert!(packed.bundle);
        // Again, over the previous bundle.
        let packed = pack_with(&config, opts(), &launcher).unwrap();
        assert_eq!(fs::read(app.join("Contents/MacOS/Site")).unwrap(), macho);
        let payload_file = app.join("Contents/Resources/payload.tar.zst");
        let footer = payload::read_footer(&payload_file).unwrap().unwrap();
        assert_eq!(footer, packed.footer);
        assert_eq!(footer.payload_offset, 0);
        assert!(app.join("Contents/Resources/AppIcon.icns").is_file());
        let plist = fs::read_to_string(app.join("Contents/Info.plist")).unwrap();
        assert!(plist.contains("<string>Site</string>"));
        let files = Payload::new(&payload_file, &footer)
            .load_app_files(1 << 20)
            .unwrap()
            .unwrap();
        assert!(files.contains_key("index.html"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sizes_and_modes() {
        assert_eq!(size(10), "10 bytes");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(mode_string(0o755), "rwxr-xr-x");
        assert_eq!(mode_string(0o644), "rw-r--r--");
    }
}
