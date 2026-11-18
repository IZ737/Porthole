//! Running an app: `porthole run app.toml` in development, or a packed exe.
//!
//! Order of events:
//! 1. Read `app.toml` (from disk, or from the front of the payload), create
//!    `APP_DATA_DIR`, start logging.
//! 2. Take the single-instance lock (a second launch focuses the first
//!    window and exits) before unpacking or spawning anything.
//! 3. Packed apps: unpack the payload into the extract-once cache (sidecar,
//!    or a static site over the in-memory limit), or into memory (static).
//! 4. Sidecar: pick a port and token, spawn the sidecar, wait for
//!    readiness (on failure show an error dialog with the log tail), and
//!    open the window at `http://127.0.0.1:<port>/?token=<token>`.
//!    Static: open the window at the `porthole://` custom protocol.
//! 5. On close (or if the sidecar dies), stop the sidecar and exit.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use url::Url;

use crate::cache;
use crate::config::{self, Config, Mode, SidecarSection, StaticSection};
use crate::dialog::{self, ErrorReport};
use crate::icon;
use crate::instance::{self, Instance, Primary};
use crate::logs;
use crate::payload::{Footer, PackInfo, Payload};
use crate::sidecar::{self, Sidecar};
use crate::static_site::{self, Files, StaticSite};
use crate::window::{self, CustomProtocol, Hooks, WindowOptions};
use crate::window_state;

/// Options from the command line.
pub struct RunOptions {
    pub devtools: bool,
    pub console: bool,
    /// Packed apps: unpack afresh instead of trusting the cache.
    pub verify: bool,
}

/// An error that has already been shown to the user in a dialog.
#[derive(Debug)]
pub struct Reported(pub String);

impl std::fmt::Display for Reported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Reported {}

/// A packed exe's payload.
struct Bundle {
    exe: PathBuf,
    footer: Footer,
    info: PackInfo,
    cache_root: PathBuf,
    icon: Option<Vec<u8>>,
}

impl Bundle {
    /// Unpack into the cache (once per version), and prune old versions in
    /// the background.
    fn unpack(&self, fresh: bool) -> Result<PathBuf> {
        cache::ensure_extracted(&self.cache_root, &self.exe, &self.footer, fresh)
    }

    /// Delete cache folders of other versions untouched for 30 days. Runs
    /// in the background so it never delays the window.
    fn prune_in_background(&self) {
        let root = self.cache_root.clone();
        let keep = self.footer.cache_key();
        std::thread::spawn(move || {
            for path in cache::prune(&root, &keep, SystemTime::now()) {
                logs::info!("pruned the unused cache folder {}", path.display());
            }
        });
    }

    /// The static site's files: in memory if they fit under the limit,
    /// otherwise unpacked to the cache.
    fn static_files(&self, st: &StaticSection, fresh: bool) -> Result<Files> {
        if self.info.unpacked_bytes <= st.in_memory_limit {
            let started = Instant::now();
            let payload = Payload::new(&self.exe, &self.footer);
            if let Some(files) = payload.load_app_files(st.in_memory_limit)? {
                logs::info!(
                    "loaded {} files into memory in {:.2}s",
                    files.len(),
                    started.elapsed().as_secs_f64()
                );
                return Ok(Files::memory(files));
            }
        }
        logs::info!(
            "the site is larger than `static.in_memory_limit_mb`; serving it from the cache folder"
        );
        self.unpack(fresh)?;
        Ok(Files::Disk(st.root.clone()))
    }
}

/// `porthole run app.toml`.
pub fn run_dev(config_path: &Path, opts: RunOptions) -> Result<()> {
    let config = Config::load(config_path)?;
    launch(config, &config_path.display().to_string(), None, opts)
}

/// A packed exe: everything comes from the payload at the end of `exe`.
pub fn run_packed(exe: PathBuf, footer: Footer, opts: RunOptions) -> Result<()> {
    let payload = Payload::new(&exe, &footer);
    let (app_toml, info, icon) = payload.read_meta_and_icon()?;
    let cache_root = cache::root()?;
    // Paths in the packed app.toml point into the cache folder.
    let config = Config::parse(&app_toml, &cache::folder(&cache_root, &footer))
        .context("the packed app.toml is invalid")?;
    let label = format!("{} (packed, sha256 {})", exe.display(), footer.cache_key());
    let opts = RunOptions {
        // Only `pack --devtools` enables the inspector in a packed app.
        devtools: info.devtools,
        ..opts
    };
    let bundle = Bundle {
        exe,
        footer,
        info,
        cache_root,
        icon,
    };
    launch(config, &label, Some(bundle), opts)
}

/// The per-user data folder for `app.id`, passed to the backend as
/// `APP_DATA_DIR` (e.g. `%APPDATA%\com.example.myapp`,
/// `~/Library/Application Support/com.example.myapp`,
/// `~/.local/share/com.example.myapp`).
pub fn data_dir(app_id: &str) -> Result<PathBuf> {
    let base = directories::BaseDirs::new().context("could not find the user's home folder")?;
    Ok(base.data_dir().join(app_id))
}

fn launch(config: Config, label: &str, bundle: Option<Bundle>, opts: RunOptions) -> Result<()> {
    let data_dir = data_dir(&config.app.id)?;
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("could not create {}", data_dir.display()))?;
    let logs_dir = data_dir.join("logs");
    if opts.console {
        logs::attach_console();
    }
    logs::init(&logs_dir, opts.console)
        .with_context(|| format!("could not open the log in {}", logs_dir.display()))?;
    logs::info!(
        "porthole {} starting {} ({})",
        env!("CARGO_PKG_VERSION"),
        config.app.name,
        label
    );
    for warning in config.warnings() {
        logs::warning!("{warning}");
    }
    if !opts.console {
        eprintln!("porthole: logs in {}", logs_dir.display());
    }

    let instance = if config.window.single_instance {
        match instance::acquire(&data_dir)? {
            Instance::Primary(p) => Some(p),
            Instance::Secondary => return Ok(()),
        }
    } else {
        None
    };

    let fail = |message: String, tail: Vec<String>| -> anyhow::Error {
        dialog::show_error(&ErrorReport {
            title: config.app.name.clone(),
            message: message.clone(),
            log_tail: tail,
            logs_dir: Some(logs_dir.clone()),
        });
        anyhow::Error::new(Reported(message))
    };

    if let Some(b) = &bundle {
        b.prune_in_background();
    }
    let mut window_opts = window_options(&config, &data_dir, opts.devtools);
    window_opts.icon = window_icon(&config, bundle.as_ref());
    match &config.mode {
        Mode::Sidecar(sc) => {
            if let Some(b) = &bundle
                && let Err(err) = b.unpack(opts.verify)
            {
                return Err(fail(format!("{err:#}"), vec![]));
            }
            run_sidecar(
                &config,
                sc,
                &data_dir,
                &logs_dir,
                instance,
                window_opts,
                &fail,
            )
        }
        Mode::Static(st) => {
            let files = match &bundle {
                Some(b) => b.static_files(st, opts.verify),
                None if st.root.is_dir() => Ok(Files::Disk(st.root.clone())),
                None => Err(anyhow::anyhow!(
                    "`static.root` {} is not a folder",
                    st.root.display()
                )),
            };
            let files = files.map_err(|err| fail(format!("{err:#}"), vec![]))?;
            let site = StaticSite {
                files,
                spa_fallback: st.spa_fallback,
                csp: st
                    .csp
                    .clone()
                    .unwrap_or_else(|| static_site::DEFAULT_CSP.to_owned()),
            };
            run_static(site, instance, window_opts)
        }
    }
}

fn window_options(config: &Config, data_dir: &Path, devtools: bool) -> WindowOptions {
    WindowOptions {
        title: Some(config.title().to_owned()),
        width: config.window.width,
        height: config.window.height,
        resizable: config.window.resizable,
        devtools,
        theme: match config.window.theme {
            config::Theme::Light => Some(tao::window::Theme::Light),
            config::Theme::Dark => Some(tao::window::Theme::Dark),
            config::Theme::System => None,
        },
        state_file: config
            .window
            .remember_window
            .then(|| data_dir.join(window_state::FILE_NAME)),
        webview_data_dir: Some(data_dir.join("webview")),
        protocol: None,
        title_bar: config.window.title_bar,
        bar_color: config.window.bar_rgb(),
        icon: None,
        app_page: true,
        startup_wait: Duration::ZERO,
    }
}

/// The window icon from `app.icon`: the payload's copy in a packed app,
/// the file next to `app.toml` in development. A bad icon is only a
/// warning.
fn window_icon(config: &Config, bundle: Option<&Bundle>) -> Option<tao::window::Icon> {
    let bytes = match bundle {
        Some(b) => b.icon.clone()?,
        None => {
            let path = config.icon_path.as_ref()?;
            match std::fs::read(path) {
                Ok(b) => b,
                Err(err) => {
                    logs::warning!("could not read `app.icon` {}: {err}", path.display());
                    return None;
                }
            }
        }
    };
    match icon::window_icon(&bytes) {
        Ok(i) => Some(i),
        Err(err) => {
            logs::warning!("`app.icon` can't be used as the window icon: {err:#}");
            None
        }
    }
}

/// Hand focus requests from later launches to the window.
fn listen_for_focus(instance: Option<Primary>, handle: &window::WindowHandle) {
    if let Some(primary) = instance {
        let focus = handle.clone();
        match primary.listen(move || focus.focus()) {
            // The guard holds the lock for the life of the process.
            Ok(guard) => std::mem::forget(guard),
            Err(err) => logs::warning!("single-instance listener failed: {err:#}"),
        }
    }
}

fn run_static(site: StaticSite, instance: Option<Primary>, mut opts: WindowOptions) -> Result<()> {
    let site = Arc::new(site);
    opts.protocol = Some(CustomProtocol {
        scheme: static_site::SCHEME,
        handler: Arc::new(move |request| site.handle(request)),
    });
    let hooks = Hooks {
        on_open: Some(Box::new(move |handle| listen_for_focus(instance, &handle))),
        on_exit: Some(Box::new(|| logs::info!("exiting"))),
    };
    window::open(static_site::start_url(), opts, hooks)
}

fn run_sidecar(
    config: &Config,
    sc: &SidecarSection,
    data_dir: &Path,
    logs_dir: &Path,
    instance: Option<Primary>,
    mut window_opts: WindowOptions,
    fail: &dyn Fn(String, Vec<String>) -> anyhow::Error,
) -> Result<()> {
    // Spawn on the main thread: see `Sidecar::spawn` about Linux's
    // parent-death signal.
    let (spec, port, token) = match sidecar_spec(config, sc, data_dir, logs_dir) {
        Ok(v) => v,
        Err(err) => return Err(fail(format!("{err:#}"), vec![])),
    };
    logs::info!(
        "starting the backend: {} {:?} in {} on port {port}",
        spec.program.display(),
        spec.args,
        spec.cwd.display()
    );
    let started = Instant::now();
    let mut sidecar = match Sidecar::spawn(&spec) {
        Ok(s) => s,
        Err(err) => return Err(fail(format!("{err:#}"), vec![])),
    };
    logs::info!("backend pid {}", sidecar.pid());

    if let Err(err) = sidecar.wait_ready(port, sc.ready_path.as_deref(), sc.ready_timeout) {
        sidecar.shutdown();
        return Err(fail(format!("{err:#}"), sidecar.log_tail()));
    }
    window_opts.startup_wait = started.elapsed();
    logs::info!(
        "backend ready after {:.2}s",
        window_opts.startup_wait.as_secs_f64()
    );

    let url = app_url(port, token.as_deref())?;
    let sidecar = Rc::new(RefCell::new(sidecar));
    let on_open_sidecar = sidecar.clone();
    let app_name = config.app.name.clone();
    let logs_dir = logs_dir.to_path_buf();
    let hooks = Hooks {
        on_open: Some(Box::new(move |handle| {
            listen_for_focus(instance, &handle);
            on_open_sidecar
                .borrow()
                .on_unexpected_exit(move |status, tail| {
                    handle.fail(ErrorReport {
                        title: app_name,
                        message: format!(
                            "The backend stopped unexpectedly ({}).",
                            sidecar::describe(status)
                        ),
                        log_tail: tail,
                        logs_dir: Some(logs_dir),
                    });
                });
        })),
        on_exit: Some(Box::new(move || {
            sidecar.borrow_mut().shutdown();
            logs::info!("exiting");
        })),
    };

    window::open(url, window_opts, hooks)
}

/// The sidecar's command line and environment, plus the port and token.
fn sidecar_spec(
    config: &Config,
    sc: &SidecarSection,
    data_dir: &Path,
    logs_dir: &Path,
) -> Result<(sidecar::Spec, u16, Option<String>)> {
    let (program, cwd) = sidecar::resolve_program(&sc.path, sc.command.as_deref())?;
    let port = sidecar::pick_port_for(data_dir).context("could not find a free port")?;
    let token = if config.auth.enabled {
        Some(sidecar::random_hex(32)?)
    } else {
        None
    };
    let mut env = vec![
        ("APP_PORT".to_owned(), port.to_string().into()),
        ("APP_DATA_DIR".to_owned(), data_dir.as_os_str().to_owned()),
    ];
    if let Some(t) = &token {
        env.push(("APP_TOKEN".to_owned(), t.into()));
    }
    let spec = sidecar::Spec {
        program,
        args: sc.args.clone(),
        cwd,
        env,
        log_file: Some(logs_dir.join("sidecar.log")),
        shutdown_grace: sc.shutdown_grace,
    };
    Ok((spec, port, token))
}

/// The first URL the window loads. The token is only ever in this one
/// request; the backend swaps it for a cookie and redirects.
fn app_url(port: u16, token: Option<&str>) -> Result<Url> {
    let mut url = Url::parse(&format!("http://127.0.0.1:{port}/"))?;
    if let Some(t) = token {
        url.query_pairs_mut().append_pair("token", t);
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_with_and_without_token() {
        assert_eq!(
            app_url(5000, Some("abc")).unwrap().as_str(),
            "http://127.0.0.1:5000/?token=abc"
        );
        assert_eq!(
            app_url(5000, None).unwrap().as_str(),
            "http://127.0.0.1:5000/"
        );
    }

    fn sidecar_of(config: &Config) -> &SidecarSection {
        match &config.mode {
            Mode::Sidecar(s) => s,
            Mode::Static(_) => panic!("not a sidecar app"),
        }
    }

    #[test]
    fn spec_sets_env() {
        let dir = std::env::temp_dir().join(format!("porthole-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("server.py"), "").unwrap();
        let text = "[app]\nname = \"A\"\nid = \"a\"\n[sidecar]\npath = \".\"\ncommand = \"python3\"\nargs = [\"server.py\"]\n";
        let config = Config::parse(text, &dir).unwrap();
        let (spec, port, token) = sidecar_spec(
            &config,
            sidecar_of(&config),
            Path::new("/data"),
            Path::new("/logs"),
        )
        .unwrap();
        let env: std::collections::HashMap<_, _> = spec.env.iter().cloned().collect();
        assert_eq!(env["APP_PORT"], port.to_string().as_str());
        assert_eq!(env["APP_DATA_DIR"], "/data");
        assert_eq!(env["APP_TOKEN"], token.unwrap().as_str());
        assert_eq!(spec.args, ["server.py"]);
        assert_eq!(
            spec.log_file.as_deref(),
            Some(Path::new("/logs/sidecar.log"))
        );

        let config = Config::parse(&format!("{text}[auth]\nenabled = false\n"), &dir).unwrap();
        let (spec, _, token) = sidecar_spec(
            &config,
            sidecar_of(&config),
            Path::new("/data"),
            Path::new("/logs"),
        )
        .unwrap();
        assert!(token.is_none());
        assert!(spec.env.iter().all(|(k, _)| k != "APP_TOKEN"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
