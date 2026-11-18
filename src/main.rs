//! Porthole: one binary that is both the developer CLI and the runtime
//! launcher for "backend + localhost web UI" desktop apps.
//!
//! Module map (and where later milestones plug in):
//! - [`cli`]: argument parsing.
//! - [`target`]: turning user input into the URL the window loads, plus the
//!   loopback-only policy (`run --url`).
//! - [`ready`]: waiting until a server accepts connections (`run --url`).
//! - [`app`]: `run app.toml` and packed exes, tying the pieces below
//!   together.
//! - [`payload`]: the packed format (footer and tar.zst payload), and
//!   self-detection: an exe with a footer runs its app instead of the CLI.
//! - [`pack`]: `pack` and `inspect`, including cross-packing from
//!   downloaded launchers ([`launcher`]) and macOS `.app` bundles
//!   ([`macos_app`]). [`exe`]: the PE / Mach-O / ELF details packing needs.
//! - [`cache`]: the extract-once cache for packed apps.
//! - [`static_site`]: static mode's `porthole://` file server.
//! - [`config`]: parsing and validating `app.toml`.
//! - [`sidecar`]: spawning, readiness, and OS-level kill-on-exit.
//! - [`instance`]: single-instance lock and focus hand-over.
//! - [`logs`]: rotating log files and `--console`.
//! - [`dialog`]: native error dialogs.
//! - [`window`] and [`window_state`]: the tao window and wry webview, and
//!   remembering its bounds. [`window_api`]: the script injected into app
//!   pages and the window controls for custom title bars. [`icon`]: PNG
//!   decoding for `app.icon`. Every mode ends by handing a URL and
//!   [`window::WindowOptions`] to [`window::open`].

mod app;
mod cache;
mod cli;
mod config;
mod dialog;
mod exe;
mod icon;
mod instance;
mod launcher;
mod logs;
mod macos_app;
mod pack;
mod payload;
mod ready;
mod sidecar;
mod static_site;
mod target;
mod window;
mod window_api;
mod window_state;

use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command, PackedArgs, RunArgs};
use crate::dialog::ErrorReport;

fn main() -> ExitCode {
    logs::mark_start();
    // A packed exe runs its app; a plain one is the developer CLI.
    match detect_payload() {
        Ok(None) => {}
        Ok(Some((exe, footer))) => return run_packed(exe, footer),
        Err(err) => return packed_failure(err),
    }

    let cli = Cli::parse();
    let result = match cli.command {
        Command::Run(args) => match &args.config {
            Some(config) => app::run_dev(
                config,
                app::RunOptions {
                    devtools: args.devtools,
                    console: args.console,
                    verify: false,
                },
            ),
            None => run_url(args),
        },
        Command::Pack(args) => pack::pack(
            &args.config,
            pack::PackOptions {
                output: args.output,
                devtools: args.devtools,
                target: args.target,
                launcher: args.launcher,
                macos_app: args.macos_app,
            },
        ),
        Command::Inspect(args) => pack::inspect(&args.exe),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("porthole: error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// The file holding this app's payload, and its footer: this executable,
/// or the payload file of the `.app` bundle it runs in. `None` for the
/// plain CLI.
fn detect_payload() -> Result<Option<(std::path::PathBuf, payload::Footer)>> {
    let exe = std::env::current_exe()?;
    if let Some(footer) = payload::read_footer(&exe)? {
        return Ok(Some((exe, footer)));
    }
    if let Some(file) = macos_app::payload_for_exe(&exe) {
        return Ok(payload::read_footer(&file)?.map(|f| (file, f)));
    }
    Ok(None)
}

fn run_packed(exe: std::path::PathBuf, footer: payload::Footer) -> ExitCode {
    let args = PackedArgs::parse(std::env::args_os().skip(1));
    let opts = app::RunOptions {
        devtools: false,
        console: args.console,
        verify: args.verify,
    };
    match app::run_packed(exe, footer, opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => packed_failure(err),
    }
}

/// Packed apps are usually started without a terminal, so errors go to a
/// dialog (unless one was already shown) as well as stderr.
fn packed_failure(err: anyhow::Error) -> ExitCode {
    eprintln!("porthole: error: {err:#}");
    if err.downcast_ref::<app::Reported>().is_none() {
        let title = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "Porthole".to_owned());
        dialog::show_error(&ErrorReport {
            title,
            message: format!("{err:#}"),
            log_tail: vec![],
            logs_dir: None,
        });
    }
    ExitCode::FAILURE
}

/// `porthole run --url <url>`: open a window on a server that is already running.
fn run_url(args: RunArgs) -> Result<()> {
    let input = args.url.as_deref().unwrap_or_default();
    let url = target::parse_url(input)?;
    target::check_allowed(&url, args.allow_remote)?;

    if !args.no_wait {
        let addr = target::socket_addr_string(&url)?;
        ready::wait_for_tcp(&addr, args.wait_timeout())?;
    }

    window::open(
        url,
        window::WindowOptions {
            title: args.title,
            width: args.width,
            height: args.height,
            resizable: !args.fixed_size,
            devtools: args.devtools,
            theme: None,
            state_file: None,
            webview_data_dir: None,
            protocol: None,
            title_bar: config::TitleBar::Native,
            bar_color: None,
            icon: None,
            app_page: false,
            startup_wait: std::time::Duration::ZERO,
        },
        window::Hooks::default(),
    )
}
