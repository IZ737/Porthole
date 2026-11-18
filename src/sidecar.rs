//! Running the app's backend: spawning it with `APP_PORT`, `APP_TOKEN` and
//! `APP_DATA_DIR`, capturing its output, waiting until it answers, and making
//! sure it never outlives the launcher.
//!
//! How the sidecar is tied to the launcher, per OS:
//!
//! | OS      | If the launcher dies                                   | Graceful stop                          |
//! |---------|--------------------------------------------------------|----------------------------------------|
//! | Windows | Job Object with `KILL_ON_JOB_CLOSE`, assigned before the child runs | `CTRL_BREAK_EVENT`, then `TerminateJobObject` |
//! | Linux   | `PR_SET_PDEATHSIG` = `SIGTERM` in the child             | `SIGTERM` to its group, then `SIGKILL` |
//! | macOS   | A forked reaper waits on the launcher with kqueue and kills the group | `SIGTERM` to its group, then `SIGKILL` |
//!
//! The sidecar always runs in its own process group so that its own
//! children are stopped with it.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::logs::{self, RotatingFile};

/// How often readiness is polled, as in the design.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Lines of sidecar output kept in memory for error dialogs.
pub const TAIL_LINES: usize = 20;
/// After a hard kill, how long to wait for the OS to report the exit.
const KILL_WAIT: Duration = Duration::from_secs(2);

/// Everything needed to start the sidecar.
#[derive(Debug, Clone)]
pub struct Spec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, OsString)>,
    /// `sidecar.log`; `None` keeps output in memory only (tests).
    pub log_file: Option<PathBuf>,
    pub shutdown_grace: Duration,
}

/// Work out which program to run and from which folder, from
/// `sidecar.path` and `sidecar.command`.
///
/// - `path` is a file and there is no `command`: run that file.
/// - Otherwise `command` is looked up in the folder (`path` itself, or the
///   file's folder). A bare name not found there, such as `python` or
///   `node`, is looked up on `PATH`, which is handy in dev mode.
///
/// The working directory is the folder in both cases.
pub fn resolve_program(path: &Path, command: Option<&str>) -> Result<(PathBuf, PathBuf)> {
    let meta = std::fs::metadata(path)
        .with_context(|| format!("`sidecar.path` {} does not exist", path.display()))?;
    let path = std::path::absolute(path)?;
    let (dir, command) = if meta.is_dir() {
        let command = command.with_context(|| {
            format!(
                "`sidecar.command` is required because `sidecar.path` ({}) is a folder",
                path.display()
            )
        })?;
        (path, command)
    } else {
        let dir = path
            .parent()
            .context("`sidecar.path` has no parent")?
            .to_path_buf();
        match command {
            None => return Ok((path, dir)),
            Some(c) => (dir, c),
        }
    };

    let candidate = dir.join(command);
    if candidate.is_file() || (cfg!(windows) && candidate.with_extension("exe").is_file()) {
        return Ok((candidate, dir));
    }
    let bare = !command.contains(['/', '\\']);
    if bare {
        // Let the OS search PATH when spawning.
        return Ok((PathBuf::from(command), dir));
    }
    bail!(
        "`sidecar.command` {command:?} not found in {}",
        dir.display()
    )
}

/// A free loopback port. It is released before returning, so another
/// process could take it in the meantime; the backend then fails to bind and
/// the readiness wait reports it.
pub fn pick_port() -> io::Result<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    Ok(listener.local_addr()?.port())
}

/// Remembers the app's port in `APP_DATA_DIR`.
pub const PORT_FILE: &str = "porthole-port";

/// The app's port: the one it had last time if it is still free, so the
/// page keeps its origin (`http://127.0.0.1:<port>`) and with it its
/// localStorage, IndexedDB and other per-origin storage across launches.
/// The first launch picks a random free port and remembers it.
///
/// If the remembered port is taken (by another program, or by a second
/// window of an app with `single_instance = false`), this launch uses a
/// random one, but the remembered port is kept for next time.
pub fn pick_port_for(data_dir: &Path) -> io::Result<u16> {
    let file = data_dir.join(PORT_FILE);
    let saved = std::fs::read_to_string(&file)
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .filter(|p| *p >= 1024);
    if let Some(port) = saved {
        if TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok() {
            return Ok(port);
        }
        logs::warning!(
            "port {port}, which this app used last time, is taken; using another one, \
             so the page's saved browser storage is not available this time"
        );
        return pick_port();
    }
    let port = pick_port()?;
    if let Err(err) = std::fs::write(&file, format!("{port}\n")) {
        logs::warning!("could not save the port to {}: {err}", file.display());
    }
    Ok(port)
}

/// `bytes` random bytes from the OS, as lowercase hex.
pub fn random_hex(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| anyhow::anyhow!("no OS randomness: {e}"))?;
    let mut out = String::with_capacity(bytes * 2);
    for b in buf {
        let _ = write!(out, "{b:02x}");
    }
    Ok(out)
}

/// The sidecar's recent output and its log file.
struct Output {
    tail: Mutex<VecDeque<String>>,
    file: Option<Mutex<RotatingFile>>,
}

impl Output {
    fn push(&self, line: String) {
        if logs::console() {
            eprintln!("[sidecar] {line}");
        }
        if let Some(f) = &self.file
            && let Ok(mut f) = f.lock()
        {
            let _ = f.write_line(&line);
        }
        if let Ok(mut tail) = self.tail.lock() {
            if tail.len() == TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    }
}

type ExitCallback = Box<dyn FnOnce(ExitStatus) + Send>;

/// Shared between the watcher thread (which owns the `Child`) and the
/// [`Sidecar`] handle.
struct ExitState {
    status: Mutex<Option<ExitStatus>>,
    changed: Condvar,
    on_exit: Mutex<Option<ExitCallback>>,
}

/// A running sidecar. Call [`Sidecar::shutdown`] before the process exits;
/// dropping it also shuts it down.
pub struct Sidecar {
    pid: u32,
    exit: Arc<ExitState>,
    output: Arc<Output>,
    stopping: Arc<AtomicBool>,
    shutdown_grace: Duration,
    os: os::Tie,
    stopped: bool,
}

impl Sidecar {
    /// Start the sidecar.
    ///
    /// On Linux the parent-death signal fires when the *thread* that spawned
    /// the child exits, so call this from the main thread.
    pub fn spawn(spec: &Spec) -> Result<Self> {
        let file = match &spec.log_file {
            Some(p) => {
                Some(Mutex::new(RotatingFile::open(p).with_context(|| {
                    format!("could not open {}", p.display())
                })?))
            }
            None => None,
        };
        let output = Arc::new(Output {
            tail: Mutex::new(VecDeque::with_capacity(TAIL_LINES)),
            file,
        });
        if let Some(f) = &output.file
            && let Ok(mut f) = f.lock()
        {
            let _ = f.write_line(&format!(
                "--- {} porthole: starting {} ---",
                logs::timestamp(),
                spec.program.display()
            ));
        }

        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        let (mut child, tie) = os::spawn(&mut cmd, spec.shutdown_grace)
            .with_context(|| format!("could not start {}", spec.program.display()))?;
        let pid = child.id();

        // Copy stdout and stderr into the log line by line. Each reader
        // reports EOF so the watcher can wait for the last lines to land.
        let (eof_tx, eof_rx) = mpsc::channel::<()>();
        let stdout = child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>);
        let stderr = child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>);
        let mut readers = 0;
        for pipe in [stdout, stderr].into_iter().flatten() {
            readers += 1;
            let output = output.clone();
            let eof_tx = eof_tx.clone();
            thread::Builder::new()
                .name("sidecar-output".into())
                .spawn(move || {
                    copy_lines(pipe, &output);
                    let _ = eof_tx.send(());
                })?;
        }
        drop(eof_tx);

        let exit = Arc::new(ExitState {
            status: Mutex::new(None),
            changed: Condvar::new(),
            on_exit: Mutex::new(None),
        });
        let watcher_exit = exit.clone();
        let watcher_output = output.clone();
        thread::Builder::new()
            .name("sidecar-watch".into())
            .spawn(move || watch(child, readers, eof_rx, &watcher_exit, &watcher_output))?;

        Ok(Self {
            pid,
            exit,
            output,
            stopping: Arc::new(AtomicBool::new(false)),
            shutdown_grace: spec.shutdown_grace,
            os: tie,
            stopped: false,
        })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The exit status, if the sidecar has exited.
    pub fn exit_status(&self) -> Option<ExitStatus> {
        *self.exit.status.lock().unwrap()
    }

    /// Wait up to `timeout` for the sidecar to exit.
    fn wait_exit(&self, timeout: Duration) -> Option<ExitStatus> {
        let guard = self.exit.status.lock().unwrap();
        let (guard, _) = self
            .exit
            .changed
            .wait_timeout_while(guard, timeout, |s| s.is_none())
            .unwrap();
        *guard
    }

    /// The last [`TAIL_LINES`] lines the sidecar printed.
    pub fn log_tail(&self) -> Vec<String> {
        tail_of(&self.output)
    }

    /// Call `f` (on another thread) if the sidecar exits on its own, that
    /// is, not because of [`Sidecar::shutdown`]. It receives the exit status
    /// and the log tail. If it has already exited, `f` runs straight away.
    pub fn on_unexpected_exit(&self, f: impl FnOnce(ExitStatus, Vec<String>) + Send + 'static) {
        let stopping = self.stopping.clone();
        let output = self.output.clone();
        let callback: ExitCallback = Box::new(move |status| {
            if !stopping.load(Ordering::SeqCst) {
                f(status, tail_of(&output));
            }
        });
        // Hold the status lock so the watcher can't finish in between the
        // check and the store.
        let status = self.exit.status.lock().unwrap();
        match *status {
            Some(s) => {
                drop(status);
                callback(s);
            }
            None => *self.exit.on_exit.lock().unwrap() = Some(callback),
        }
    }

    /// Wait until the backend answers on `127.0.0.1:port`: a TCP connection,
    /// or with `ready_path` an HTTP response that is not an error.
    ///
    /// Fails if the sidecar exits first or `timeout` passes.
    pub fn wait_ready(&self, port: u16, ready_path: Option<&str>, timeout: Duration) -> Result<()> {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let start = Instant::now();
        loop {
            if let Some(status) = self.exit_status() {
                bail!(
                    "the backend exited before it was ready ({})",
                    describe(status)
                );
            }
            let Err(last_problem) = probe(addr, ready_path) else {
                return Ok(());
            };
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                bail!(
                    "the backend did not become ready within {:.0}s: {last_problem} on {addr}",
                    timeout.as_secs_f64()
                );
            }
            // Sleep, but wake early if the sidecar exits.
            let _ = self.wait_exit(POLL_INTERVAL.min(timeout - elapsed));
        }
    }

    /// Stop the sidecar: ask it to exit, wait up to the grace period, then
    /// kill it and anything it started. Safe to call more than once.
    pub fn shutdown(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.stopping.store(true, Ordering::SeqCst);

        if self.exit_status().is_none() {
            logs::info!(
                "stopping the backend (pid {}, grace {:.1}s)",
                self.pid,
                self.shutdown_grace.as_secs_f64()
            );
            let asked = self.os.request_stop(self.pid);
            let exited = asked && self.wait_exit(self.shutdown_grace).is_some();
            if !exited {
                logs::info!("killing the backend (pid {})", self.pid);
                self.os.kill(self.pid);
                if self.wait_exit(KILL_WAIT).is_none() {
                    logs::warning!(
                        "the backend (pid {}) did not exit after being killed",
                        self.pid
                    );
                }
            }
        }
        // Whatever it started may still be running.
        self.os.sweep(self.pid);
        if let Some(status) = self.exit_status() {
            logs::info!("backend stopped ({})", describe(status));
        }
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn tail_of(output: &Output) -> Vec<String> {
    output
        .tail
        .lock()
        .map(|t| t.iter().cloned().collect())
        .unwrap_or_default()
}

/// The watcher thread: wait for the child, let the output readers finish
/// (briefly: a grandchild may hold the pipes open), then publish the status.
fn watch(
    mut child: Child,
    readers: usize,
    eof_rx: mpsc::Receiver<()>,
    exit: &ExitState,
    output: &Output,
) {
    let status = match child.wait() {
        Ok(s) => s,
        Err(err) => {
            logs::warning!("lost track of the backend: {err}");
            return;
        }
    };
    let deadline = Instant::now() + Duration::from_millis(500);
    for _ in 0..readers {
        let left = deadline.saturating_duration_since(Instant::now());
        if eof_rx.recv_timeout(left).is_err() {
            break;
        }
    }
    output.push(format!(
        "--- {} porthole: backend exited ({}) ---",
        logs::timestamp(),
        describe(status)
    ));

    let callback = {
        let mut s = exit.status.lock().unwrap();
        *s = Some(status);
        exit.changed.notify_all();
        exit.on_exit.lock().unwrap().take()
    };
    if let Some(cb) = callback {
        cb(status);
    }
}

fn copy_lines(pipe: Box<dyn Read + Send>, output: &Output) {
    let mut reader = BufReader::new(pipe);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                while buf.last().is_some_and(|b| matches!(b, b'\n' | b'\r')) {
                    buf.pop();
                }
                output.push(String::from_utf8_lossy(&buf).into_owned());
            }
        }
    }
}

/// `exit code 1`, `killed by signal 9`, ...
pub fn describe(status: ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("exit code {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return format!("killed by signal {sig}");
        }
    }
    status.to_string()
}

/// One readiness check. `Err` says what is wrong, for the timeout message.
fn probe(addr: SocketAddr, ready_path: Option<&str>) -> Result<(), String> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(250))
        .map_err(|_| "nothing accepted a connection".to_owned())?;
    let Some(path) = ready_path else {
        return Ok(());
    };
    let status =
        http_status(&mut stream, addr, path).map_err(|e| format!("GET {path} failed ({e})"))?;
    if is_ready_status(status) {
        Ok(())
    } else {
        Err(format!("GET {path} returned HTTP {status}"))
    }
}

/// Statuses that mean "the backend is up". The probe carries no session
/// cookie, so a backend enforcing the auth handshake answers 401 or 403,
/// which still proves it is serving.
pub fn is_ready_status(status: u16) -> bool {
    (200..400).contains(&status) || status == 401 || status == 403
}

/// A minimal HTTP/1.1 GET; returns the response status code.
fn http_status(stream: &mut TcpStream, addr: SocketAddr, path: &str) -> io::Result<u16> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    // One write: `write!` on a raw socket would send each piece of the
    // format string as its own segment.
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nUser-Agent: porthole\r\n\
         Accept: */*\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes())?;
    let mut line = String::new();
    BufReader::new(stream.take(1024)).read_line(&mut line)?;
    // "HTTP/1.1 200 OK"
    let mut parts = line.split_whitespace();
    match (parts.next(), parts.next().and_then(|c| c.parse().ok())) {
        (Some(v), Some(code)) if v.starts_with("HTTP/") => Ok(code),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not an HTTP response",
        )),
    }
}

#[cfg(target_os = "linux")]
mod os {
    use super::*;
    use std::os::unix::process::CommandExt;

    pub struct Tie;

    pub fn spawn(cmd: &mut Command, _grace: Duration) -> io::Result<(Child, Tie)> {
        let launcher = std::process::id() as libc::pid_t;
        cmd.process_group(0);
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(io::Error::last_os_error());
                }
                // The launcher may have died before prctl took effect.
                if libc::getppid() != launcher {
                    return Err(io::Error::other("launcher exited during spawn"));
                }
                Ok(())
            });
        }
        Ok((cmd.spawn()?, Tie))
    }

    impl Tie {
        pub fn request_stop(&self, pid: u32) -> bool {
            super::unix::signal_group(pid, libc::SIGTERM)
        }
        pub fn kill(&self, pid: u32) {
            super::unix::signal_group(pid, libc::SIGKILL);
        }
        pub fn sweep(&self, pid: u32) {
            super::unix::signal_group(pid, libc::SIGKILL);
        }
    }
}

#[cfg(target_os = "macos")]
mod os {
    use super::*;

    /// The reaper process's pid, if it started.
    pub struct Tie {
        reaper: Option<libc::pid_t>,
    }

    pub fn spawn(cmd: &mut Command, grace: Duration) -> io::Result<(Child, Tie)> {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        let child = cmd.spawn()?;
        let reaper = start_reaper(child.id() as libc::pid_t, grace);
        if reaper.is_none() {
            logs::warning!(
                "could not start the reaper process; the backend may outlive Porthole if it crashes"
            );
        }
        Ok((child, Tie { reaper }))
    }

    /// Fork a tiny process that waits for the launcher to exit (kqueue
    /// `NOTE_EXIT`) and then stops the sidecar's process group. macOS has no
    /// parent-death signal, so this is what covers a crashed or killed
    /// launcher.
    fn start_reaper(pgid: libc::pid_t, grace: Duration) -> Option<libc::pid_t> {
        let launcher = std::process::id() as libc::pid_t;
        let grace = libc::timespec {
            tv_sec: grace.as_secs().min(3600) as libc::time_t,
            tv_nsec: grace.subsec_nanos() as libc::c_long,
        };
        // Highest fd to close in the child, computed before forking.
        // SAFETY: plain libc call.
        let max_fd = unsafe { libc::getdtablesize() }.clamp(256, 65536);
        // SAFETY: the child below only makes async-signal-safe syscalls and
        // leaves with `_exit`; it never returns into Rust code.
        match unsafe { libc::fork() } {
            -1 => None,
            0 => unsafe { reaper_main(launcher, pgid, grace, max_fd) },
            pid => Some(pid),
        }
    }

    /// Runs in the forked reaper. Async-signal-safe calls only: the
    /// launcher is multi-threaded, so locks and the allocator may be in any
    /// state here.
    unsafe fn reaper_main(
        launcher: libc::pid_t,
        pgid: libc::pid_t,
        grace: libc::timespec,
        max_fd: libc::c_int,
    ) -> ! {
        unsafe {
            // Leave the launcher's process group so a Ctrl+C aimed at the
            // launcher in a terminal doesn't take the reaper down too.
            libc::setpgid(0, 0);
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            libc::signal(libc::SIGINT, libc::SIG_IGN);
            // Drop inherited descriptors, above all the single-instance
            // lock, which would otherwise stay held after the launcher dies.
            for fd in 3..max_fd {
                libc::close(fd);
            }
            // And the launcher's stdio, so whoever reads its output sees EOF
            // when the launcher exits, not when the reaper does.
            let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
            if null >= 0 {
                for fd in 0..3 {
                    libc::dup2(null, fd);
                }
                if null > 2 {
                    libc::close(null);
                }
            }

            let kq = libc::kqueue();
            let mut watch = libc::kevent {
                ident: launcher as libc::uintptr_t,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_ADD | libc::EV_ONESHOT,
                fflags: libc::NOTE_EXIT,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            let registered = kq >= 0
                && libc::kevent(kq, &watch, 1, std::ptr::null_mut(), 0, std::ptr::null()) == 0;
            // Not registered means the launcher is already gone (ESRCH).
            if registered {
                loop {
                    let n = libc::kevent(kq, std::ptr::null(), 0, &mut watch, 1, std::ptr::null());
                    if n > 0 || (n < 0 && *libc::__error() != libc::EINTR) {
                        break;
                    }
                }
            }
            libc::kill(-pgid, libc::SIGTERM);
            libc::nanosleep(&grace, std::ptr::null_mut());
            libc::kill(-pgid, libc::SIGKILL);
            libc::_exit(0)
        }
    }

    impl Tie {
        pub fn request_stop(&self, pid: u32) -> bool {
            super::unix::signal_group(pid, libc::SIGTERM)
        }
        pub fn kill(&self, pid: u32) {
            super::unix::signal_group(pid, libc::SIGKILL);
        }
        pub fn sweep(&mut self, pid: u32) {
            super::unix::signal_group(pid, libc::SIGKILL);
            // The launcher has done the reaper's job; stop it before it can
            // signal a process group id that gets reused later.
            if let Some(reaper) = self.reaper.take() {
                // SAFETY: plain libc calls on a pid we own.
                unsafe {
                    libc::kill(reaper, libc::SIGKILL);
                    libc::waitpid(reaper, std::ptr::null_mut(), 0);
                }
            }
        }
    }
}

#[cfg(unix)]
mod unix {
    /// Signal the sidecar's process group (its pgid is its pid). Returns
    /// whether the signal was delivered.
    pub fn signal_group(pid: u32, sig: libc::c_int) -> bool {
        // SAFETY: plain libc call.
        unsafe { libc::kill(-(pid as libc::pid_t), sig) == 0 }
    }
}

#[cfg(windows)]
mod os {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED, OpenThread, ResumeThread,
        THREAD_SUSPEND_RESUME,
    };

    /// The job object holding the sidecar and everything it starts. When
    /// the launcher exits for any reason, the OS closes this handle and the
    /// job's processes are killed.
    pub struct Tie {
        job: Option<HANDLE>,
    }

    // SAFETY: a job handle can be used from any thread.
    unsafe impl Send for Tie {}

    fn create_job() -> io::Result<HANDLE> {
        // SAFETY: Win32 calls with valid pointers; the struct is plain data.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                let err = io::Error::last_os_error();
                CloseHandle(job);
                return Err(err);
            }
            Ok(job)
        }
    }

    pub fn spawn(cmd: &mut Command, _grace: Duration) -> io::Result<(Child, Tie)> {
        let job = match create_job() {
            Ok(j) => Some(j),
            Err(err) => {
                logs::warning!(
                    "could not create a job object ({err}); the backend may outlive Porthole if it crashes"
                );
                None
            }
        };
        // Start suspended so the child is in the job before it can start
        // processes of its own, then resume it.
        // CREATE_NO_WINDOW: a packed app is a GUI program with no console,
        // so a console backend (python.exe, node.exe) would otherwise get
        // a console window of its own. Its output is piped to the log.
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED | CREATE_NO_WINDOW);
        let mut child = cmd.spawn()?;
        if let Some(job) = job {
            // SAFETY: both handles are valid for the duration of the call.
            if unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) } == 0 {
                logs::warning!(
                    "could not add the backend to a job object ({}); it may outlive Porthole if it crashes",
                    io::Error::last_os_error()
                );
            }
        }
        if let Err(err) = resume_process(child.id()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err);
        }
        Ok((child, Tie { job }))
    }

    /// Resume every thread of a process started with `CREATE_SUSPENDED`.
    fn resume_process(pid: u32) -> io::Result<()> {
        // SAFETY: Win32 calls with valid pointers; handles are closed.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snap == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let mut entry: THREADENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            let mut resumed = 0;
            let mut more = Thread32First(snap, &mut entry) != 0;
            while more {
                if entry.th32OwnerProcessID == pid {
                    let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                    if !thread.is_null() {
                        if ResumeThread(thread) != u32::MAX {
                            resumed += 1;
                        }
                        CloseHandle(thread);
                    }
                }
                more = Thread32Next(snap, &mut entry) != 0;
            }
            CloseHandle(snap);
            if resumed == 0 {
                return Err(io::Error::other(
                    "could not resume the backend's main thread",
                ));
            }
            Ok(())
        }
    }

    impl Tie {
        /// Ctrl+Break to the sidecar's process group. This only works when
        /// it shares the launcher's console; otherwise go straight to kill.
        pub fn request_stop(&self, pid: u32) -> bool {
            // SAFETY: plain Win32 call.
            unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) != 0 }
        }
        pub fn kill(&self, pid: u32) {
            match self.job {
                // SAFETY: valid job handle.
                Some(job) => unsafe {
                    TerminateJobObject(job, 1);
                },
                None => {
                    let _ = Command::new("taskkill")
                        .args(["/F", "/T", "/PID", &pid.to_string()])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
            }
        }
        pub fn sweep(&self, _pid: u32) {
            if let Some(job) = self.job {
                // SAFETY: valid job handle.
                unsafe {
                    TerminateJobObject(job, 1);
                }
            }
        }
    }

    impl Drop for Tie {
        fn drop(&mut self) {
            if let Some(job) = self.job.take() {
                // SAFETY: we own the handle. Closing it kills what is left.
                unsafe {
                    CloseHandle(job);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_statuses() {
        for s in [200, 204, 302, 401, 403] {
            assert!(is_ready_status(s), "{s}");
        }
        for s in [404, 500, 502, 503] {
            assert!(!is_ready_status(s), "{s}");
        }
    }

    #[test]
    fn port_is_remembered() {
        // Other tests bind ephemeral ports at the same time, so the OS can
        // hand a port this test has just released to one of them before
        // the next call checks it. When that happens, start again with a
        // new data folder (and so a new port) instead of failing.
        for attempt in 0..5 {
            let dir = std::env::temp_dir()
                .join(format!("porthole-port-{}-{attempt}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let result = check_port_memory(&dir);
            let _ = std::fs::remove_dir_all(&dir);
            if result {
                return;
            }
        }
        panic!("another test took the remembered port on every attempt");
    }

    /// The checks behind `port_is_remembered`. Returns false when a port
    /// that should have been free was taken by someone else.
    fn check_port_memory(dir: &Path) -> bool {
        let first = pick_port_for(dir).unwrap();
        if pick_port_for(dir).unwrap() != first {
            return false;
        }
        assert_eq!(
            std::fs::read_to_string(dir.join(PORT_FILE)).unwrap().trim(),
            first.to_string()
        );
        // Taken: use another port this time, but keep the saved one.
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, first)).unwrap();
        assert_ne!(pick_port_for(dir).unwrap(), first);
        drop(held);
        if pick_port_for(dir).unwrap() != first {
            return false;
        }
        // Garbage or a privileged port: start over.
        std::fs::write(dir.join(PORT_FILE), "80").unwrap();
        let fresh = pick_port_for(dir).unwrap();
        assert!(fresh >= 1024);
        pick_port_for(dir).unwrap() == fresh
    }

    #[test]
    fn random_hex_is_hex() {
        let t = random_hex(32).unwrap();
        assert_eq!(t.len(), 64);
        assert!(t.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(t, random_hex(32).unwrap());
    }

    #[test]
    fn resolves_programs() {
        let dir = std::env::temp_dir().join(format!("porthole-resolve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("server.bin");
        std::fs::write(&exe, b"").unwrap();

        // A file with no command runs itself, from its folder.
        let (prog, cwd) = resolve_program(&exe, None).unwrap();
        assert_eq!(
            (prog.as_path(), cwd.as_path()),
            (exe.as_path(), dir.as_path())
        );
        // A folder plus a command found in it.
        let (prog, cwd) = resolve_program(&dir, Some("server.bin")).unwrap();
        assert_eq!(
            (prog.as_path(), cwd.as_path()),
            (exe.as_path(), dir.as_path())
        );
        // A bare name not in the folder falls back to PATH.
        let (prog, _) = resolve_program(&dir, Some("python3")).unwrap();
        assert_eq!(prog, Path::new("python3"));
        // Errors.
        assert!(resolve_program(&dir, None).is_err());
        assert!(resolve_program(&dir, Some("bin/missing")).is_err());
        assert!(resolve_program(&dir.join("nope"), None).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn http_probe_reads_status() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut statuses = ["503 Service Unavailable", "401 Unauthorized"].into_iter();
            let mut status = statuses.next();
            while let Some(current) = status {
                let (mut s, _) = listener.accept().unwrap();
                let mut req = [0u8; 1024];
                let n = s.read(&mut req).unwrap_or(0);
                // Other tests run in parallel and probe ports they picked
                // and released; one may land on this listener with a bare
                // TCP connect. Only answer our own probe's request.
                if !String::from_utf8_lossy(&req[..n]).starts_with("GET /healthz HTTP/1.1") {
                    continue;
                }
                write!(s, "HTTP/1.1 {current}\r\nContent-Length: 0\r\n\r\n").unwrap();
                status = statuses.next();
            }
        });
        assert!(probe(addr, Some("/healthz")).unwrap_err().contains("503"));
        let second = probe(addr, Some("/healthz"));
        assert!(second.is_ok(), "{second:?}");
        server.join().unwrap();
    }

    // End-to-end tests with a real backend. They use Python, which every CI
    // runner has; they are skipped where it is missing.

    fn python() -> Option<&'static str> {
        ["python3", "python"].into_iter().find(|p| {
            Command::new(p)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
    }

    fn spec(script: &str, env: Vec<(String, OsString)>) -> Option<Spec> {
        let Some(py) = python() else {
            eprintln!("skipping: python not found");
            return None;
        };
        Some(Spec {
            program: PathBuf::from(py),
            args: vec!["-u".into(), "-c".into(), script.into()],
            cwd: std::env::temp_dir(),
            env,
            log_file: None,
            shutdown_grace: Duration::from_secs(2),
        })
    }

    const SERVER: &str = r#"
import os, socket, sys, signal, time
signal.signal(signal.SIGTERM, lambda *a: (print("got SIGTERM"), sys.exit(0)))
if hasattr(signal, "SIGBREAK"):
    signal.signal(signal.SIGBREAK, lambda *a: (print("got SIGBREAK"), sys.exit(0)))
print("token=" + os.environ.get("APP_TOKEN", "-"))
print("data=" + os.environ.get("APP_DATA_DIR", "-"))
time.sleep(0.2)
s = socket.socket()
s.bind(("127.0.0.1", int(os.environ["APP_PORT"])))
s.listen()
print("listening")
s.settimeout(0.2)
while True:
    try:
        c, _ = s.accept()
        c.close()
    except socket.timeout:
        pass
"#;

    #[test]
    fn spawns_waits_and_stops_gracefully() {
        let port = pick_port().unwrap();
        let env = vec![
            ("APP_PORT".into(), port.to_string().into()),
            ("APP_TOKEN".into(), "secret".into()),
            ("APP_DATA_DIR".into(), "/data".into()),
        ];
        let Some(spec) = spec(SERVER, env) else {
            return;
        };
        let mut sidecar = Sidecar::spawn(&spec).unwrap();
        sidecar
            .wait_ready(port, None, Duration::from_secs(20))
            .unwrap();
        let unexpected = Arc::new(AtomicBool::new(false));
        let flag = unexpected.clone();
        sidecar.on_unexpected_exit(move |_, _| flag.store(true, Ordering::SeqCst));

        let start = Instant::now();
        sidecar.shutdown();
        assert!(sidecar.exit_status().is_some());
        let tail = sidecar.log_tail().join("\n");
        assert!(tail.contains("token=secret"), "{tail}");
        assert!(tail.contains("data=/data"), "{tail}");
        assert!(tail.contains("listening"), "{tail}");
        // Python handles the stop request (SIGTERM, or Ctrl+Break on
        // Windows when a console is shared) and exits within the grace.
        if cfg!(unix) {
            assert!(tail.contains("got SIGTERM"), "{tail}");
            assert!(start.elapsed() < Duration::from_secs(2));
        }
        assert!(!unexpected.load(Ordering::SeqCst));
    }

    #[test]
    fn kills_backend_that_ignores_stop() {
        let script = r#"
import signal, time
for name in ("SIGTERM", "SIGBREAK"):
    if hasattr(signal, name):
        signal.signal(getattr(signal, name), signal.SIG_IGN)
print("ready")
time.sleep(60)
"#;
        let Some(mut spec) = spec(script, vec![]) else {
            return;
        };
        spec.shutdown_grace = Duration::from_millis(300);
        let mut sidecar = Sidecar::spawn(&spec).unwrap();
        // Give Python time to install the handlers.
        let start = Instant::now();
        while !sidecar.log_tail().iter().any(|l| l == "ready") {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "python never started"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let start = Instant::now();
        sidecar.shutdown();
        assert!(sidecar.exit_status().is_some());
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn reports_early_exit() {
        let script = "import sys; print('boom: missing config'); sys.exit(3)";
        let Some(spec) = spec(script, vec![]) else {
            return;
        };
        let sidecar = Sidecar::spawn(&spec).unwrap();
        let port = pick_port().unwrap();
        let err = sidecar
            .wait_ready(port, None, Duration::from_secs(20))
            .unwrap_err()
            .to_string();
        assert!(err.contains("exited before it was ready"), "{err}");
        assert!(err.contains("exit code 3"), "{err}");
        assert!(
            sidecar
                .log_tail()
                .iter()
                .any(|l| l == "boom: missing config")
        );

        // Registering after the exit still fires.
        let (tx, rx) = mpsc::channel();
        sidecar.on_unexpected_exit(move |status, tail| {
            let _ = tx.send((status.code(), tail));
        });
        let (code, tail) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(code, Some(3));
        assert!(tail.iter().any(|l| l == "boom: missing config"));
    }

    #[test]
    fn times_out_when_backend_never_listens() {
        let Some(spec) = spec("import time; time.sleep(60)", vec![]) else {
            return;
        };
        let mut sidecar = Sidecar::spawn(&spec).unwrap();
        let port = pick_port().unwrap();
        let err = sidecar
            .wait_ready(port, None, Duration::from_millis(300))
            .unwrap_err()
            .to_string();
        assert!(err.contains("did not become ready"), "{err}");
        sidecar.shutdown();
    }

    #[test]
    fn missing_program_is_an_error() {
        let spec = Spec {
            program: PathBuf::from("porthole-definitely-not-a-program"),
            args: vec![],
            cwd: std::env::temp_dir(),
            env: vec![],
            log_file: None,
            shutdown_grace: Duration::from_secs(1),
        };
        assert!(Sidecar::spawn(&spec).is_err());
    }

    /// The point of the OS-level tie: kill the launcher outright (no
    /// cleanup code runs) and the sidecar must go too. Runs the test binary
    /// itself as a stand-in launcher.
    #[test]
    fn sidecar_dies_with_launcher() {
        if std::env::var_os("PORTHOLE_TEST_LAUNCHER").is_some() {
            return;
        }
        if python().is_none() {
            eprintln!("skipping: python not found");
            return;
        }
        let pid_file = std::env::temp_dir().join(format!("porthole-orphan-{}", std::process::id()));
        let _ = std::fs::remove_file(&pid_file);
        let mut launcher = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sidecar::tests::fake_launcher",
                "--nocapture",
                "--include-ignored",
            ])
            .env("PORTHOLE_TEST_LAUNCHER", &pid_file)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let start = Instant::now();
        let sidecar_pid: u32 = loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|s| s.trim().parse().ok())
            {
                break pid;
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "fake launcher never started"
            );
            thread::sleep(Duration::from_millis(50));
        };
        assert!(process_alive(sidecar_pid));
        launcher.kill().unwrap(); // SIGKILL / TerminateProcess
        launcher.wait().unwrap();
        let start = Instant::now();
        while process_alive(sidecar_pid) {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "sidecar {sidecar_pid} outlived its launcher"
            );
            thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_file(&pid_file);
    }

    /// Helper for `sidecar_dies_with_launcher`; does nothing in a normal run.
    #[test]
    #[ignore = "helper process for sidecar_dies_with_launcher"]
    fn fake_launcher() {
        let Some(pid_file) = std::env::var_os("PORTHOLE_TEST_LAUNCHER") else {
            return;
        };
        let spec = spec("import time; print('up'); time.sleep(120)", vec![]).unwrap();
        let sidecar = Sidecar::spawn(&spec).unwrap();
        std::fs::write(&pid_file, sidecar.pid().to_string()).unwrap();
        std::mem::forget(sidecar);
        thread::sleep(Duration::from_secs(120));
    }

    fn process_alive(pid: u32) -> bool {
        #[cfg(unix)]
        {
            // Signal 0 checks existence. A zombie still "exists", so also
            // ask ps (which reports state Z) on unix.
            // SAFETY: plain libc call.
            if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
                return false;
            }
            let out = Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            let stat = String::from_utf8_lossy(&out.stdout);
            !stat.trim().is_empty() && !stat.trim_start().starts_with('Z')
        }
        #[cfg(windows)]
        {
            let out = Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
        }
    }
}
