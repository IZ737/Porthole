//! Single instance: one running copy per `app.id`.
//!
//! The first instance holds an exclusive lock on `<data>/porthole.lock` and
//! listens on a loopback port written to `<data>/porthole-instance`, next to
//! a random nonce. A second launch finds the lock taken, sends the nonce to
//! that port so the first instance brings its window to the front, and
//! exits. The OS drops the lock when the first instance exits, even if it
//! crashes.

use std::fs::{self, File, TryLockError};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::logs;
use crate::sidecar::random_hex;

const LOCK_FILE: &str = "porthole.lock";
const INFO_FILE: &str = "porthole-instance";
/// How long a second instance keeps trying to reach the first, which may
/// still be starting up.
const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(3);

/// Result of [`acquire`].
pub enum Instance {
    /// We are the only instance. Keep this alive for as long as the app runs.
    Primary(Primary),
    /// Another instance is running and has been asked to show its window.
    Secondary,
}

pub struct Primary {
    _lock: File,
    listener: TcpListener,
    nonce: String,
}

/// Take the single-instance lock in `data_dir`, or hand over to the
/// instance that holds it.
pub fn acquire(data_dir: &Path) -> Result<Instance> {
    let lock_path = data_dir.join(LOCK_FILE);
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("could not open {}", lock_path.display()))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            match activate_existing(data_dir) {
                Ok(()) => {
                    logs::info!("already running; asked the other window to come to the front")
                }
                Err(err) => logs::warning!(
                    "already running, but could not reach the other instance: {err:#}"
                ),
            }
            return Ok(Instance::Secondary);
        }
        Err(TryLockError::Error(err)) => {
            // Some file systems don't support locks. Run anyway rather
            // than refuse to start.
            logs::warning!("single-instance lock unavailable ({err}); continuing without it");
        }
    }

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let nonce = random_hex(16)?;
    let port = listener.local_addr()?.port();
    write_atomic(
        &data_dir.join(INFO_FILE),
        format!("{port} {nonce}\n").as_bytes(),
    )?;
    Ok(Instance::Primary(Primary {
        _lock: lock,
        listener,
        nonce,
    }))
}

impl Primary {
    /// Call `activate` (on a background thread) whenever another launch
    /// asks this instance to show itself. Keep the returned value alive:
    /// dropping it releases the lock.
    pub fn listen(self, activate: impl Fn() + Send + 'static) -> Result<PrimaryGuard> {
        let listener = self.listener.try_clone()?;
        let nonce = self.nonce.clone();
        thread::Builder::new()
            .name("single-instance".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    if read_request(stream).is_some_and(|got| constant_time_eq(&got, &nonce)) {
                        activate();
                    }
                }
            })?;
        Ok(PrimaryGuard { _primary: self })
    }
}

/// Holds the lock until dropped (or the process exits).
pub struct PrimaryGuard {
    _primary: Primary,
}

fn read_request(stream: TcpStream) -> Option<String> {
    stream.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
    let mut line = String::new();
    BufReader::new(stream.take(256)).read_line(&mut line).ok()?;
    line.strip_prefix("activate ").map(|n| n.trim().to_owned())
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Second instance: tell the first to show its window. Retries for a
/// while, since the first may hold the lock but not be listening yet.
fn activate_existing(data_dir: &Path) -> Result<()> {
    let info_path = data_dir.join(INFO_FILE);
    let deadline = Instant::now() + ACTIVATE_TIMEOUT;
    loop {
        let attempt = (|| -> Result<()> {
            let info = fs::read_to_string(&info_path)?;
            let (port, nonce) = info
                .trim()
                .split_once(' ')
                .context("malformed instance file")?;
            let port: u16 = port.parse()?;
            let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
            let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(500))?;
            writeln!(s, "activate {nonce}")?;
            Ok(())
        })();
        match attempt {
            Ok(()) => return Ok(()),
            Err(err) if Instant::now() >= deadline => return Err(err),
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// Write via a temporary file and rename, so a reader never sees half.
fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let tmp = tmp_path(path);
    fs::write(&tmp, contents)?;
    // Windows can't rename over an existing file.
    let _ = fs::remove_file(path);
    fs::rename(&tmp, path)
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".tmp-{}", std::process::id()));
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn second_instance_activates_first() {
        let dir = std::env::temp_dir().join(format!("porthole-instance-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let Instance::Primary(primary) = acquire(&dir).unwrap() else {
            panic!("first acquire should be primary");
        };
        let (tx, rx) = mpsc::channel();
        let guard = primary.listen(move || tx.send(()).unwrap()).unwrap();

        // File locks are per open file, so a second acquire in the same
        // process behaves like a second instance.
        assert!(matches!(acquire(&dir).unwrap(), Instance::Secondary));
        rx.recv_timeout(Duration::from_secs(5)).unwrap();

        // A wrong nonce is ignored.
        let info = fs::read_to_string(dir.join(INFO_FILE)).unwrap();
        let port: u16 = info.split(' ').next().unwrap().parse().unwrap();
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        writeln!(s, "activate wrong").unwrap();
        drop(s);
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());

        // Once the first instance goes away, the next launch is primary.
        drop(guard);
        assert!(matches!(acquire(&dir).unwrap(), Instance::Primary(_)));
        let _ = fs::remove_dir_all(&dir);
    }
}
