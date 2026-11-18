use std::path::PathBuf;
use std::time::Duration;

use clap::{ArgGroup, Args, Parser, Subcommand};

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
    /// `porthole run app.toml` starts the backend described in `app.toml`
    /// and opens a window on it. `porthole run --url <URL>` opens a window
    /// on a server you have already started (for example a Vite dev server).
    Run(RunArgs),

    /// Pack an app into a single executable.
    ///
    /// Copies this launcher and appends the app described in `app.toml`
    /// (the sidecar or the static site, plus the config) as a compressed
    /// payload. The result runs the app when started.
    Pack(PackArgs),

    /// Show what a packed executable (or `.app` bundle) contains.
    Inspect(InspectArgs),
}

#[derive(Debug, Args)]
pub struct PackArgs {
    /// The app's `app.toml`.
    #[arg(value_name = "APP_TOML")]
    pub config: PathBuf,

    /// Where to write the executable. Defaults to `<app.name>` (plus `.exe`
    /// on Windows) in the current folder.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Allow the web inspector in the packed app. Off by default.
    #[arg(long)]
    pub devtools: bool,

    /// Pack for another platform, using the prebuilt launcher from this
    /// version's GitHub release (downloaded once, then cached). One of
    /// x86_64-pc-windows-msvc, x86_64-apple-darwin, aarch64-apple-darwin,
    /// x86_64-unknown-linux-gnu.
    #[arg(long, value_name = "TRIPLE")]
    pub target: Option<String>,

    /// Pack onto this launcher binary instead (a `porthole` built for the
    /// platform the app is for). With `--target`, it must match it.
    #[arg(long, value_name = "FILE")]
    pub launcher: Option<PathBuf>,

    /// Write a macOS `.app` bundle (needed to sign and notarize), with the
    /// payload in `Contents/Resources`. Needs a macOS launcher.
    #[arg(long)]
    pub macos_app: bool,
}

#[derive(Debug, Args)]
pub struct InspectArgs {
    /// A packed executable, or a `.app` bundle.
    #[arg(value_name = "EXE")]
    pub exe: PathBuf,
}

/// Flags a packed app accepts. Anything else on its command line is
/// ignored (it belongs to the app, or to the OS).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PackedArgs {
    /// Stream the logs to a terminal.
    pub console: bool,
    /// Unpack afresh instead of trusting the cache folder.
    pub verify: bool,
}

impl PackedArgs {
    pub fn parse(args: impl IntoIterator<Item = std::ffi::OsString>) -> Self {
        let mut out = Self::default();
        for arg in args {
            match arg.to_str() {
                Some("--console") => out.console = true,
                Some("--verify") => out.verify = true,
                _ => {}
            }
        }
        out
    }
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("source").required(true).args(["config", "url"])))]
pub struct RunArgs {
    /// The app's `app.toml`: start its backend and open a window on it.
    #[arg(value_name = "APP_TOML")]
    pub config: Option<PathBuf>,

    /// Stream the launcher and backend logs to this terminal as well as to
    /// the log files (with `app.toml`).
    #[arg(long, conflicts_with = "url")]
    pub console: bool,

    /// URL of an already-running server, e.g. `http://localhost:3000`.
    ///
    /// `localhost:3000` and a bare port such as `3000` are accepted too and
    /// mean `http://localhost:<port>`.
    #[arg(long, value_name = "URL")]
    pub url: Option<String>,

    /// Window title. Defaults to the page's `<title>`, updated as it changes
    /// (with `--url`; `app.toml` sets its own).
    #[arg(long, conflicts_with = "config")]
    pub title: Option<String>,

    /// Initial window width in logical pixels (with `--url`).
    #[arg(long, default_value_t = 1200, value_parser = clap::value_parser!(u32).range(100..), conflicts_with = "config")]
    pub width: u32,

    /// Initial window height in logical pixels (with `--url`).
    #[arg(long, default_value_t = 800, value_parser = clap::value_parser!(u32).range(100..), conflicts_with = "config")]
    pub height: u32,

    /// Keep the window at its initial size (with `--url`).
    #[arg(long, conflicts_with = "config")]
    pub fixed_size: bool,

    /// Allow opening the web inspector (right-click > Inspect, or the
    /// platform shortcut).
    #[arg(long)]
    pub devtools: bool,

    /// Allow URLs that are not on this machine (anything other than
    /// localhost or a loopback address).
    #[arg(long, conflicts_with = "config")]
    pub allow_remote: bool,

    /// Seconds to wait for the server to accept connections before giving
    /// up (with `--url`; `app.toml` uses `sidecar.ready_timeout_secs`).
    #[arg(
        long,
        value_name = "SECS",
        default_value_t = 10.0,
        conflicts_with = "config"
    )]
    pub wait: f64,

    /// Open the window straight away without checking that the server is up.
    #[arg(long, conflicts_with_all = ["wait", "config"])]
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
        let Command::Run(args) = cli.command else {
            panic!("not run")
        };
        assert_eq!(args.url.as_deref(), Some("3000"));
        assert_eq!(args.config, None);
        assert_eq!((args.width, args.height), (1200, 800));
        assert!(!args.allow_remote && !args.devtools && !args.no_wait);
        assert_eq!(args.wait_timeout(), Duration::from_secs(10));
    }

    #[test]
    fn pack_and_inspect() {
        let cli = Cli::try_parse_from(["porthole", "pack", "app.toml", "-o", "My.exe"]).unwrap();
        let Command::Pack(args) = cli.command else {
            panic!("not pack")
        };
        assert_eq!(args.output.as_deref(), Some(std::path::Path::new("My.exe")));
        assert!(!args.devtools);
        assert!(Cli::try_parse_from(["porthole", "pack"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["porthole", "inspect", "My.exe"])
                .unwrap()
                .command,
            Command::Inspect(_)
        ));
    }

    #[test]
    fn packed_args() {
        let args = |a: &[&str]| PackedArgs::parse(a.iter().map(std::ffi::OsString::from));
        assert_eq!(args(&[]), PackedArgs::default());
        assert_eq!(
            args(&["--console", "file.txt", "--verify"]),
            PackedArgs {
                console: true,
                verify: true
            }
        );
    }

    #[test]
    fn run_requires_url_or_config() {
        assert!(Cli::try_parse_from(["porthole", "run"]).is_err());
    }

    #[test]
    fn run_with_config() {
        let cli = Cli::try_parse_from(["porthole", "run", "app.toml", "--console", "--devtools"])
            .unwrap();
        let Command::Run(args) = cli.command else {
            panic!("not run")
        };
        assert_eq!(
            args.config.as_deref(),
            Some(std::path::Path::new("app.toml"))
        );
        assert!(args.console && args.devtools);
    }

    #[test]
    fn config_and_url_flags_dont_mix() {
        for bad in [
            &["porthole", "run", "app.toml", "--url", "3000"][..],
            &["porthole", "run", "app.toml", "--width", "900"],
            &["porthole", "run", "app.toml", "--no-wait"],
            &["porthole", "run", "--url", "3000", "--console"],
        ] {
            assert!(Cli::try_parse_from(bad).is_err(), "{bad:?}");
        }
    }
}
