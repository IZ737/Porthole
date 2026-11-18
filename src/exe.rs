//! Just enough of the Windows (PE), macOS (Mach-O) and Linux (ELF)
//! executable formats for packing:
//!
//! - telling which platform a launcher was built for, so `pack --target`
//!   and `pack --launcher` can refuse a mismatch;
//! - switching a Windows launcher to the GUI subsystem, so a packed app
//!   starts without a console window (the plain `porthole.exe` stays a
//!   console program, so the CLI works in a terminal);
//! - finding the Authenticode certificate table that `signtool` appends to
//!   a signed exe, after the payload footer.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};

/// Launchers that releases publish, and `pack --target` accepts.
pub const TARGETS: &[&str] = &[
    "x86_64-pc-windows-msvc",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-unknown-linux-gnu",
];

/// The target this binary was built for, in the same naming.
pub fn host_target() -> &'static str {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        "aarch64-pc-windows-msvc"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else {
        "unknown"
    }
}

/// Operating system part of a target triple.
pub fn target_os(target: &str) -> &'static str {
    if target.contains("windows") {
        "windows"
    } else if target.contains("apple") {
        "macos"
    } else if target.contains("linux") {
        "linux"
    } else {
        "unknown"
    }
}

/// What kind of executable a file is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// Windows. `gui` is false for console programs.
    Pe {
        arch: &'static str,
        gui: bool,
    },
    /// macOS; a universal binary lists every architecture.
    MachO {
        arches: Vec<&'static str>,
    },
    Elf {
        arch: &'static str,
    },
    Unknown,
}

impl Kind {
    pub fn os(&self) -> &'static str {
        match self {
            Kind::Pe { .. } => "windows",
            Kind::MachO { .. } => "macos",
            Kind::Elf { .. } => "linux",
            Kind::Unknown => "unknown",
        }
    }

    /// Whether this executable runs on `target`.
    pub fn runs_on(&self, target: &str) -> bool {
        let arch = target.split('-').next().unwrap_or_default();
        self.os() == target_os(target)
            && match self {
                Kind::Pe { arch: a, .. } | Kind::Elf { arch: a } => *a == arch,
                Kind::MachO { arches } => arches.contains(&arch),
                Kind::Unknown => false,
            }
    }

    pub fn describe(&self) -> String {
        match self {
            Kind::Pe { arch, gui } => format!(
                "Windows {arch}, {} subsystem",
                if *gui { "GUI" } else { "console" }
            ),
            Kind::MachO { arches } => format!("macOS {}", arches.join(" + ")),
            Kind::Elf { arch } => format!("Linux {arch}"),
            Kind::Unknown => "unknown format".to_owned(),
        }
    }
}

/// Identify the executable at `path`.
pub fn kind(path: &Path) -> Result<Kind> {
    let mut file =
        File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    kind_of(&mut file)
}

pub fn kind_of(file: &mut (impl Read + Seek)) -> Result<Kind> {
    let mut head = [0u8; 64];
    file.seek(SeekFrom::Start(0))?;
    let n = read_up_to(file, &mut head)?;
    let head = &head[..n];
    if head.starts_with(b"MZ") {
        return Ok(match pe_header(file)? {
            Some(pe) => Kind::Pe {
                arch: pe.arch,
                gui: pe.subsystem == SUBSYSTEM_GUI,
            },
            None => Kind::Unknown,
        });
    }
    if head.len() >= 8 && head[..4] == [0xcf, 0xfa, 0xed, 0xfe] {
        let cpu = u32::from_le_bytes(head[4..8].try_into().unwrap());
        return Ok(Kind::MachO {
            arches: vec![macho_arch(cpu)],
        });
    }
    if head.len() >= 8 && head[..4] == [0xca, 0xfe, 0xba, 0xbe] {
        // Universal binary: big-endian count, then 20-byte entries.
        let count = u32::from_be_bytes(head[4..8].try_into().unwrap()) as usize;
        let mut arches = Vec::new();
        let mut entry = [0u8; 20];
        for i in 0..count.min(8) {
            file.seek(SeekFrom::Start(8 + 20 * i as u64))?;
            file.read_exact(&mut entry)?;
            arches.push(macho_arch(u32::from_be_bytes(
                entry[..4].try_into().unwrap(),
            )));
        }
        return Ok(Kind::MachO { arches });
    }
    if head.len() >= 20 && head.starts_with(b"\x7fELF") {
        let arch = match u16::from_le_bytes([head[18], head[19]]) {
            0x3e => "x86_64",
            0xb7 => "aarch64",
            _ => "other",
        };
        return Ok(Kind::Elf { arch });
    }
    Ok(Kind::Unknown)
}

fn macho_arch(cpu: u32) -> &'static str {
    match cpu {
        0x0100_0007 => "x86_64",
        0x0100_000c => "aarch64",
        _ => "other",
    }
}

fn read_up_to(file: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

const SUBSYSTEM_GUI: u16 = 2;
const SUBSYSTEM_CONSOLE: u16 = 3;
/// Index of the certificate table in the optional header's data
/// directories.
const SECURITY_DIRECTORY: u64 = 4;

/// The parts of a PE header that packing needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeHeader {
    pub arch: &'static str,
    pub subsystem: u16,
    /// File offset of the subsystem field.
    subsystem_at: u64,
    /// File offset of the certificate table's directory entry.
    security_entry_at: u64,
    /// The Authenticode certificate table: file offset and length. Zero
    /// length when unsigned.
    pub certificates: (u64, u64),
}

/// Parse the PE header, or `None` if the file is not a PE image.
pub fn pe_header(file: &mut (impl Read + Seek)) -> Result<Option<PeHeader>> {
    let mut buf = [0u8; 4];
    let mut read_at = |file: &mut dyn ReadSeek, at: u64, len: usize| -> Result<u64> {
        file.seek(SeekFrom::Start(at))?;
        file.read_exact(&mut buf[..len])?;
        Ok(match len {
            2 => u16::from_le_bytes([buf[0], buf[1]]) as u64,
            _ => u32::from_le_bytes(buf) as u64,
        })
    };
    let file: &mut dyn ReadSeek = file;
    let len = file.seek(SeekFrom::End(0))?;
    if len < 0x40 || read_at(file, 0, 2)? != u64::from(u16::from_le_bytes(*b"MZ")) {
        return Ok(None);
    }
    let pe = read_at(file, 0x3c, 4)?;
    if pe + 24 + 112 + 40 > len
        || read_at(file, pe, 4)? != u64::from(u32::from_le_bytes(*b"PE\0\0"))
    {
        return Ok(None);
    }
    let arch = match read_at(file, pe + 4, 2)? {
        0x8664 => "x86_64",
        0xaa64 => "aarch64",
        0x014c => "i686",
        _ => "other",
    };
    let opt = pe + 24;
    let dirs = match read_at(file, opt, 2)? {
        0x10b => opt + 96,
        0x20b => opt + 112,
        _ => bail!("unknown PE optional header"),
    };
    let count = read_at(file, dirs - 4, 4)?;
    let security_entry_at = dirs + 8 * SECURITY_DIRECTORY;
    let certificates = if count > SECURITY_DIRECTORY {
        (
            read_at(file, security_entry_at, 4)?,
            read_at(file, security_entry_at + 4, 4)?,
        )
    } else {
        (0, 0)
    };
    Ok(Some(PeHeader {
        arch,
        subsystem: read_at(file, opt + 68, 2)? as u16,
        subsystem_at: opt + 68,
        security_entry_at,
        certificates,
    }))
}

trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

impl PeHeader {
    /// Where the signed part of the file ends: the start of a certificate
    /// table at the end of the file, if there is one.
    pub fn signed_end(&self, file_len: u64) -> Option<u64> {
        let (offset, size) = self.certificates;
        (size > 0 && offset > 0 && offset.checked_add(size) == Some(file_len)).then_some(offset)
    }
}

/// Prepare a copy of a Windows launcher (`out`, already written) to carry
/// a payload: switch a console launcher to the GUI subsystem so the app
/// opens no console window, and clear a signature the launcher came with
/// (it no longer covers the file; sign the packed exe instead).
pub fn prepare_windows_copy(out: &mut File, header: &PeHeader) -> Result<()> {
    if header.subsystem == SUBSYSTEM_CONSOLE {
        out.seek(SeekFrom::Start(header.subsystem_at))?;
        out.write_all(&SUBSYSTEM_GUI.to_le_bytes())?;
    }
    if header.certificates != (0, 0) {
        out.seek(SeekFrom::Start(header.security_entry_at))?;
        out.write_all(&[0u8; 8])?;
    }
    out.seek(SeekFrom::End(0))?;
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::io::Cursor;

    /// A minimal PE32+ header (not a runnable program), `extra` bytes long
    /// after the headers.
    pub fn fake_pe(subsystem: u16, extra: usize) -> Vec<u8> {
        let mut b = vec![0u8; 0x80 + 24 + 240 + extra];
        b[..2].copy_from_slice(b"MZ");
        b[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        b[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        let opt = 0x80 + 24;
        b[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        b[opt + 68..opt + 70].copy_from_slice(&subsystem.to_le_bytes());
        b[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());
        b
    }

    /// Append a fake certificate table the way `signtool` does: pad to 8
    /// bytes, then the table, recorded in the security directory.
    pub fn fake_sign(b: &mut Vec<u8>) {
        while !b.len().is_multiple_of(8) {
            b.push(0);
        }
        let offset = b.len() as u32;
        b.extend_from_slice(&[0xab; 40]);
        let entry = 0x80 + 24 + 112 + 32;
        b[entry..entry + 4].copy_from_slice(&offset.to_le_bytes());
        b[entry + 4..entry + 8].copy_from_slice(&40u32.to_le_bytes());
    }

    #[test]
    fn reads_and_patches_pe() {
        let mut pe = fake_pe(SUBSYSTEM_CONSOLE, 10);
        let header = pe_header(&mut Cursor::new(&pe)).unwrap().unwrap();
        assert_eq!(header.arch, "x86_64");
        assert_eq!(header.subsystem, SUBSYSTEM_CONSOLE);
        assert_eq!(header.certificates, (0, 0));
        assert_eq!(header.signed_end(pe.len() as u64), None);
        assert_eq!(
            kind_of(&mut Cursor::new(&pe)).unwrap(),
            Kind::Pe {
                arch: "x86_64",
                gui: false
            }
        );

        fake_sign(&mut pe);
        let header = pe_header(&mut Cursor::new(&pe)).unwrap().unwrap();
        let signed_at = header.signed_end(pe.len() as u64).unwrap();
        assert_eq!(signed_at, pe.len() as u64 - 40);

        let path = std::env::temp_dir().join(format!("porthole-pe-{}", std::process::id()));
        std::fs::write(&path, &pe[..signed_at as usize]).unwrap();
        let mut out = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        prepare_windows_copy(&mut out, &header).unwrap();
        drop(out);
        let patched = std::fs::read(&path).unwrap();
        let header = pe_header(&mut Cursor::new(&patched)).unwrap().unwrap();
        assert_eq!(header.subsystem, SUBSYSTEM_GUI);
        assert_eq!(header.certificates, (0, 0));
        assert!(
            Kind::Pe {
                arch: "x86_64",
                gui: true
            }
            .runs_on("x86_64-pc-windows-msvc")
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn identifies_other_formats() {
        let mut elf = vec![0u8; 64];
        elf[..4].copy_from_slice(b"\x7fELF");
        elf[18] = 0x3e;
        let k = kind_of(&mut Cursor::new(&elf)).unwrap();
        assert!(k.runs_on("x86_64-unknown-linux-gnu"));
        assert!(!k.runs_on("aarch64-apple-darwin"));

        let mut macho = vec![0u8; 64];
        macho[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        macho[4..8].copy_from_slice(&0x0100_000cu32.to_le_bytes());
        let k = kind_of(&mut Cursor::new(&macho)).unwrap();
        assert!(k.runs_on("aarch64-apple-darwin"));
        assert!(!k.runs_on("x86_64-apple-darwin"));

        let mut fat = vec![0u8; 64];
        fat[..4].copy_from_slice(&[0xca, 0xfe, 0xba, 0xbe]);
        fat[4..8].copy_from_slice(&2u32.to_be_bytes());
        fat[8..12].copy_from_slice(&0x0100_0007u32.to_be_bytes());
        fat[28..32].copy_from_slice(&0x0100_000cu32.to_be_bytes());
        let k = kind_of(&mut Cursor::new(&fat)).unwrap();
        assert!(k.runs_on("x86_64-apple-darwin") && k.runs_on("aarch64-apple-darwin"));

        assert_eq!(
            kind_of(&mut Cursor::new(b"#!/bin/sh\n")).unwrap(),
            Kind::Unknown
        );
        let host = kind(&std::env::current_exe().unwrap()).unwrap();
        assert!(host.runs_on(host_target()), "{host:?}");
    }
}
