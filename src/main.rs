//! Porthole: one binary that is both the developer CLI and the runtime
//! launcher for "backend + localhost web UI" desktop apps.
//!
//! Module map (and where later milestones plug in):
//! - [`cli`]: argument parsing. `run app.toml` (v0.2), `pack` and `inspect`
//!   (v0.3) become new variants of [`cli::Command`].
//! - [`target`]: turning user input into the URL the window loads, plus the
//!   loopback-only policy.
//! - [`ready`]: waiting until a server accepts connections. v0.2's sidecar
//!   readiness wait reuses it.
//! - [`window`]: the tao window and wry webview. Every mode (`--url`,
//!   sidecar, static) ends by handing a URL and [`window::WindowOptions`] to
//!   [`window::open`].

mod cli;
mod ready;
mod target;
mod window;

use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command, RunArgs};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Run(args) => run(args),
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
fn run(args: RunArgs) -> Result<()> {
    let url = target::parse_url(&args.url)?;
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
        },
    )
}
