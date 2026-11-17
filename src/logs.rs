//! Log files: `porthole.log` (the launcher) and `sidecar.log` (the backend's
//! stdout and stderr), both in `<APP_DATA_DIR>/logs/`, rotated at 5 MB with
//! the last 3 kept.
//!
//! `--console` also copies every line to stderr so both logs can be watched
//! live from a terminal.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// Rotate a log once it grows past this size.
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
/// Rotated files kept besides the live one: `x.log.1` (newest) to `x.log.3`.
pub const KEEP_ROTATED: u32 = 3;

/// An append-only log file that rotates itself by size.
#[derive(Debug)]
pub struct RotatingFile {
    path: PathBuf,
    file: File,
    len: u64,
    max_bytes: u64,
    keep: u32,
}

impl RotatingFile {
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        Self::with_limits(path, MAX_LOG_BYTES, KEEP_ROTATED)
    }

    pub fn with_limits(path: impl Into<PathBuf>, max_bytes: u64, keep: u32) -> io::Result<Self> {
        let path = path.into();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            path,
            file,
            len,
            max_bytes,
            keep,
        })
    }

    /// Append `line` plus a newline, rotating first if the file is full.
    /// A line is never split across files.
    pub fn write_line(&mut self, line: &str) -> io::Result<()> {
        if self.len > 0 && self.len + line.len() as u64 + 1 > self.max_bytes {
            self.rotate()?;
        }
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.len += line.len() as u64 + 1;
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        // x.log.2 -> x.log.3, x.log.1 -> x.log.2, x.log -> x.log.1. Windows
        // can't rename over an existing file, so remove the target first.
        for i in (1..=self.keep).rev() {
            let to = rotated(&self.path, i);
            let from = if i == 1 {
                self.path.clone()
            } else {
                rotated(&self.path, i - 1)
            };
            if from.exists() {
                let _ = fs::remove_file(&to);
                fs::rename(&from, &to)?;
            }
        }
        if self.keep == 0 {
            let _ = fs::remove_file(&self.path);
        }
        self.file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)?;
        self.len = 0;
        Ok(())
    }
}

fn rotated(path: &Path, i: u32) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{i}"));
    PathBuf::from(s)
}

/// `2026-11-17T18:36:54Z`, the prefix of every launcher log line.
pub fn timestamp() -> String {
    humantime::format_rfc3339_seconds(SystemTime::now()).to_string()
}

static LAUNCHER_LOG: OnceLock<Mutex<RotatingFile>> = OnceLock::new();
static CONSOLE: AtomicBool = AtomicBool::new(false);

/// Send launcher messages to `<logs_dir>/porthole.log` from now on, and
/// also to stderr if `console` is set.
pub fn init(logs_dir: &Path, console: bool) -> io::Result<()> {
    CONSOLE.store(console, Ordering::Relaxed);
    let file = RotatingFile::open(logs_dir.join("porthole.log"))?;
    let _ = LAUNCHER_LOG.set(Mutex::new(file));
    Ok(())
}

/// Whether `--console` is on.
pub fn console() -> bool {
    CONSOLE.load(Ordering::Relaxed)
}

/// Write one launcher message. Before [`init`] (and in `run --url`, which
/// has no data folder) it goes to stderr only.
pub fn write(level: &str, msg: &str) {
    let line = format!("{} {level} {msg}", timestamp());
    match LAUNCHER_LOG.get() {
        Some(log) => {
            if let Ok(mut f) = log.lock() {
                let _ = f.write_line(&line);
            }
            if console() {
                eprintln!("[porthole] {line}");
            }
        }
        None => eprintln!("porthole: {msg}"),
    }
}

/// Log an informational launcher message.
macro_rules! info {
    ($($arg:tt)*) => { $crate::logs::write("INFO", &format!($($arg)*)) };
}

/// Log a launcher warning. Warnings also reach stderr without `--console`,
/// so they show up when running from a terminal.
macro_rules! warning {
    ($($arg:tt)*) => {{
        let msg = format!($($arg)*);
        $crate::logs::write("WARN", &msg);
        if !$crate::logs::console() && $crate::logs::initialized() {
            eprintln!("porthole: warning: {msg}");
        }
    }};
}

pub(crate) use {info, warning};

/// Whether [`init`] has run.
pub fn initialized() -> bool {
    LAUNCHER_LOG.get().is_some()
}

/// On Windows, make sure there is a console to stream to: attach to the
/// parent's (the terminal that started us) or open a new one. Elsewhere
/// stderr is already wherever the user launched from.
pub fn attach_console() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, GetConsoleWindow,
        };
        // SAFETY: plain Win32 calls with no pointers.
        unsafe {
            if GetConsoleWindow().is_null() && AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
                AllocConsole();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "porthole-test-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rotates_and_keeps_last_n() {
        let dir = temp_dir("rotate");
        let path = dir.join("x.log");
        // 10-byte lines ("line N...\n") into 25-byte files: two lines each.
        let mut log = RotatingFile::with_limits(&path, 25, 2).unwrap();
        for i in 0..8 {
            log.write_line(&format!("line {i}xxx")).unwrap();
        }
        let read = |p: &Path| fs::read_to_string(p).unwrap();
        assert_eq!(read(&path), "line 6xxx\nline 7xxx\n");
        assert_eq!(read(&rotated(&path, 1)), "line 4xxx\nline 5xxx\n");
        assert_eq!(read(&rotated(&path, 2)), "line 2xxx\nline 3xxx\n");
        assert!(!rotated(&path, 3).exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn appends_across_reopen() {
        let dir = temp_dir("reopen");
        let path = dir.join("x.log");
        RotatingFile::open(&path).unwrap().write_line("a").unwrap();
        RotatingFile::open(&path).unwrap().write_line("b").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\nb\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn oversized_line_still_written() {
        let dir = temp_dir("big");
        let path = dir.join("x.log");
        let mut log = RotatingFile::with_limits(&path, 4, 1).unwrap();
        log.write_line("0123456789").unwrap();
        log.write_line("ab").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "ab\n");
        assert_eq!(
            fs::read_to_string(rotated(&path, 1)).unwrap(),
            "0123456789\n"
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
