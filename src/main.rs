//! Porthole: one binary that is both the developer CLI and the runtime
//! launcher for "backend + localhost web UI" desktop apps.
//!
//! Module map (and where later milestones plug in):
//! - [`cli`]: argument parsing. `pack` and `inspect` (v0.3) become new
//!   variants of [`cli::Command`].
//! - [`target`]: turning user input into the URL the window loads, plus the
//!   loopback-only policy (`run --url`).
//! - [`ready`]: waiting until a server accepts connections (`run --url`).
//! - [`app`]: `run app.toml`, tying the pieces below together. A packed
//!   exe (v0.3) runs the same flow from its extracted payload.
//! - [`config`]: parsing and validating `app.toml`.
//! - [`sidecar`]: spawning, readiness, and OS-level kill-on-exit.
//! - [`instance`]: single-instance lock and focus hand-over.
//! - [`logs`]: rotating log files and `--console`.
//! - [`dialog`]: native error dialogs.
//! - [`window`] and [`window_state`]: the tao window and wry webview, and
//!   remembering its bounds. Every mode ends by handing a URL and
//!   [`window::WindowOptions`] to [`window::open`].

mod app;
mod cli;
mod config;
mod dialog;
mod instance;
mod logs;
mod ready;
mod sidecar;
mod target;
mod window;
mod window_state;

use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command, RunArgs};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Run(args) => match &args.config {
            Some(config) => app::run(
                config,
                app::RunOptions {
                    devtools: args.devtools,
                    console: args.console,
                },
            ),
            None => run_url(args),
        },
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("porthole: error: {err:#}");
            ExitCode::FAILURE
        }
    }
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
        },
        window::Hooks::default(),
    )
}
