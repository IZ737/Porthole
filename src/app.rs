//! `porthole run app.toml`: start the backend as a sidecar and open a
//! window on it.
//!
//! Order of events:
//! 1. Read `app.toml`, create `APP_DATA_DIR`, start logging.
//! 2. Take the single-instance lock (a second launch focuses the first
//!    window and exits).
//! 3. Pick a port and token, spawn the sidecar.
//! 4. Wait for readiness; on failure show an error dialog with the log tail.
//! 5. Open the window at `http://127.0.0.1:<port>/?token=<token>`.
//! 6. On close (or if the sidecar dies), stop the sidecar and exit.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use anyhow::{Context, Result};
use url::Url;

use crate::config::{self, Config};
use crate::dialog::{self, ErrorReport};
use crate::instance::{self, Instance};
use crate::logs;
use crate::sidecar::{self, Sidecar};
use crate::window::{self, Hooks, WindowOptions};
use crate::window_state;

/// Options from the command line.
pub struct RunOptions {
    pub devtools: bool,
    pub console: bool,
}

/// The per-user data folder for `app.id`, passed to the backend as
/// `APP_DATA_DIR` (e.g. `%APPDATA%\com.example.myapp`,
/// `~/Library/Application Support/com.example.myapp`,
/// `~/.local/share/com.example.myapp`).
pub fn data_dir(app_id: &str) -> Result<PathBuf> {
    let base = directories::BaseDirs::new().context("could not find the user's home folder")?;
    Ok(base.data_dir().join(app_id))
}

pub fn run(config_path: &Path, opts: RunOptions) -> Result<()> {
    let config = Config::load(config_path)?;

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
        config_path.display()
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
        anyhow::anyhow!(message)
    };

    // Spawn on the main thread: see `Sidecar::spawn` about Linux's
    // parent-death signal.
    let (spec, port, token) = match sidecar_spec(&config, &data_dir, &logs_dir) {
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

    if let Err(err) = sidecar.wait_ready(
        port,
        config.sidecar.ready_path.as_deref(),
        config.sidecar.ready_timeout,
    ) {
        sidecar.shutdown();
        return Err(fail(format!("{err:#}"), sidecar.log_tail()));
    }
    logs::info!(
        "backend ready after {:.2}s",
        started.elapsed().as_secs_f64()
    );

    let url = app_url(port, token.as_deref())?;
    let sidecar = Rc::new(RefCell::new(sidecar));
    let on_open_sidecar = sidecar.clone();
    let app_name = config.app.name.clone();
    let hooks = Hooks {
        on_open: Some(Box::new(move |handle| {
            if let Some(primary) = instance {
                let focus = handle.clone();
                match primary.listen(move || focus.focus()) {
                    // The guard holds the lock for the life of the process.
                    Ok(guard) => std::mem::forget(guard),
                    Err(err) => logs::warning!("single-instance listener failed: {err:#}"),
                }
            }
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

    window::open(
        url,
        WindowOptions {
            title: Some(config.title().to_owned()),
            width: config.window.width,
            height: config.window.height,
            resizable: config.window.resizable,
            devtools: opts.devtools,
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
        },
        hooks,
    )
}

/// The sidecar's command line and environment, plus the port and token.
fn sidecar_spec(
    config: &Config,
    data_dir: &Path,
    logs_dir: &Path,
) -> Result<(sidecar::Spec, u16, Option<String>)> {
    let (program, cwd) =
        sidecar::resolve_program(&config.sidecar.path, config.sidecar.command.as_deref())?;
    let port = sidecar::pick_port().context("could not find a free port")?;
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
        args: config.sidecar.args.clone(),
        cwd,
        env,
        log_file: Some(logs_dir.join("sidecar.log")),
        shutdown_grace: config.sidecar.shutdown_grace,
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

    #[test]
    fn spec_sets_env() {
        let dir = std::env::temp_dir().join(format!("porthole-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("server.py"), "").unwrap();
        let text = "[app]\nname = \"A\"\nid = \"a\"\n[sidecar]\npath = \".\"\ncommand = \"python3\"\nargs = [\"server.py\"]\n";
        let config = Config::parse(text, &dir).unwrap();
        let (spec, port, token) =
            sidecar_spec(&config, Path::new("/data"), Path::new("/logs")).unwrap();
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
        let (spec, _, token) =
            sidecar_spec(&config, Path::new("/data"), Path::new("/logs")).unwrap();
        assert!(token.is_none());
        assert!(spec.env.iter().all(|(k, _)| k != "APP_TOKEN"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
