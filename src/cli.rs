use std::time::Duration;

use clap::{Args, Parser, Subcommand};

/// Turn a backend + localhost web UI into a desktop app using the OS webview.
#[derive(Debug, Parser)]
#[command(name = "porthole", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Open a window on a web app.
    ///
    /// In v0.1 this only supports `--url`, which points the window at a
    /// server you have already started (for example a Vite dev server).
    Run(RunArgs),
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// URL of an already-running server, e.g. `http://localhost:3000`.
    ///
    /// `localhost:3000` and a bare port such as `3000` are accepted too and
    /// mean `http://localhost:<port>`.
    #[arg(long, value_name = "URL")]
    pub url: String,

    /// Window title. Defaults to the page's `<title>`, updated as it changes.
    #[arg(long)]
    pub title: Option<String>,

    /// Initial window width in logical pixels.
    #[arg(long, default_value_t = 1200, value_parser = clap::value_parser!(u32).range(100..))]
    pub width: u32,

    /// Initial window height in logical pixels.
    #[arg(long, default_value_t = 800, value_parser = clap::value_parser!(u32).range(100..))]
    pub height: u32,

    /// Keep the window at its initial size.
    #[arg(long)]
    pub fixed_size: bool,

    /// Allow opening the web inspector (right-click > Inspect, or the
    /// platform shortcut).
    #[arg(long)]
    pub devtools: bool,

    /// Allow URLs that are not on this machine (anything other than
    /// localhost or a loopback address).
    #[arg(long)]
    pub allow_remote: bool,

    /// Seconds to wait for the server to accept connections before giving up.
    #[arg(long, value_name = "SECS", default_value_t = 10.0)]
    pub wait: f64,

    /// Open the window straight away without checking that the server is up.
    #[arg(long, conflicts_with = "wait")]
    pub no_wait: bool,
}

impl RunArgs {
    pub fn wait_timeout(&self) -> Duration {
        Duration::try_from_secs_f64(self.wait.max(0.0)).unwrap_or(Duration::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn run_parses_defaults() {
        let cli = Cli::try_parse_from(["porthole", "run", "--url", "3000"]).unwrap();
        let Command::Run(args) = cli.command;
        assert_eq!(args.url, "3000");
        assert_eq!((args.width, args.height), (1200, 800));
        assert!(!args.allow_remote && !args.devtools && !args.no_wait);
        assert_eq!(args.wait_timeout(), Duration::from_secs(10));
    }

    #[test]
    fn run_requires_url() {
        assert!(Cli::try_parse_from(["porthole", "run"]).is_err());
    }
}
