//! `app.toml`: the app's description, read by `porthole run app.toml`.
//!
//! Parsing is strict (unknown keys are errors) so typos fail loudly instead
//! of being silently ignored. Keys that belong to later milestones (title
//! bar modes, `[static]`) are accepted where the format is final, and
//! [`Config::warnings`] says what this version ignores.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// A parsed and validated `app.toml`.
#[derive(Debug, Clone)]
pub struct Config {
    pub app: AppSection,
    pub window: WindowSection,
    pub sidecar: SidecarSection,
    pub auth: AuthSection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSection {
    pub name: String,
    /// Reverse-DNS style id, e.g. `com.example.myapp`. Names the data folder
    /// and the single-instance lock.
    pub id: String,
    /// Window and app icon. Bundled by `pack` (v0.3); not used by `run` yet.
    #[allow(dead_code)]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WindowSection {
    /// Fixed window title. Defaults to `app.name`.
    pub title: Option<String>,
    pub width: u32,
    pub height: u32,
    pub resizable: bool,
    pub title_bar: TitleBar,
    pub theme: Theme,
    pub bar_color: Option<String>,
    pub single_instance: bool,
    pub remember_window: bool,
}

impl Default for WindowSection {
    fn default() -> Self {
        Self {
            title: None,
            width: 1200,
            height: 800,
            resizable: true,
            title_bar: TitleBar::Native,
            theme: Theme::System,
            bar_color: None,
            single_instance: true,
            remember_window: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleBar {
    Native,
    Custom,
    Overlay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
    System,
}

#[derive(Debug, Clone)]
pub struct SidecarSection {
    /// File or folder holding the backend, resolved against `base_dir`.
    pub path: PathBuf,
    /// Program to run, after applying the per-OS override.
    pub command: Option<String>,
    pub args: Vec<String>,
    pub ready_path: Option<String>,
    pub ready_timeout: Duration,
    pub shutdown_grace: Duration,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AuthSection {
    /// `false` = no token, for trusted dev setups.
    pub enabled: bool,
}

impl Default for AuthSection {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// `[sidecar]` as written, before the per-OS override is applied.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSidecar {
    path: PathBuf,
    command: Option<String>,
    args: Option<Vec<String>>,
    ready_path: Option<String>,
    #[serde(default = "default_ready_timeout")]
    ready_timeout_secs: f64,
    #[serde(default = "default_shutdown_grace")]
    shutdown_grace_secs: f64,
    windows: Option<OsOverride>,
    macos: Option<OsOverride>,
    linux: Option<OsOverride>,
}

/// `[sidecar.windows]`, `[sidecar.macos]`, `[sidecar.linux]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OsOverride {
    command: Option<String>,
    args: Option<Vec<String>>,
}

fn default_ready_timeout() -> f64 {
    15.0
}

fn default_shutdown_grace() -> f64 {
    3.0
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    app: AppSection,
    #[serde(default)]
    window: WindowSection,
    sidecar: Option<RawSidecar>,
    #[serde(rename = "static")]
    static_: Option<toml::Table>,
    #[serde(default)]
    auth: AuthSection,
}

impl Config {
    /// Read and validate `app.toml` at `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?;
        let base_dir = path
            .parent()
            .map(|p| {
                if p.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    p
                }
            })
            .unwrap_or(Path::new("."))
            .to_path_buf();
        Self::parse(&text, &base_dir).with_context(|| format!("invalid {}", path.display()))
    }

    /// Parse `app.toml` text; relative paths resolve against `base_dir`.
    pub fn parse(text: &str, base_dir: &Path) -> Result<Self> {
        let raw: RawConfig = toml::from_str(text)?;

        validate_id(&raw.app.id)?;
        if raw.app.name.trim().is_empty() {
            bail!("`app.name` must not be empty");
        }
        if raw.window.width < 100 || raw.window.height < 100 {
            bail!("`window.width` and `window.height` must be at least 100");
        }

        let sidecar = match (raw.sidecar, raw.static_) {
            (Some(_), Some(_)) => bail!("use either [sidecar] or [static], not both"),
            (None, Some(_)) => bail!(
                "[static] mode is not supported yet (it arrives in Porthole v0.3); \
                 use [sidecar] for now"
            ),
            (None, None) => bail!("missing [sidecar] section"),
            (Some(s), None) => s,
        };

        let os = match std::env::consts::OS {
            "windows" => sidecar.windows,
            "macos" => sidecar.macos,
            "linux" => sidecar.linux,
            _ => None,
        };
        let (os_command, os_args) = os.map_or((None, None), |o| (o.command, o.args));
        let command = os_command.or(sidecar.command);
        let args = os_args.or(sidecar.args).unwrap_or_default();

        if command.as_deref().is_some_and(|c| c.trim().is_empty()) {
            bail!("`sidecar.command` must not be empty");
        }
        if let Some(p) = &sidecar.ready_path
            && !p.starts_with('/')
        {
            bail!("`sidecar.ready_path` must start with `/`, e.g. \"/healthz\"");
        }

        Ok(Config {
            app: raw.app,
            window: raw.window,
            sidecar: SidecarSection {
                path: base_dir.join(&sidecar.path),
                command,
                args,
                ready_path: sidecar.ready_path,
                ready_timeout: secs("sidecar.ready_timeout_secs", sidecar.ready_timeout_secs)?,
                shutdown_grace: secs("sidecar.shutdown_grace_secs", sidecar.shutdown_grace_secs)?,
            },
            auth: raw.auth,
        })
    }

    /// The window title: `window.title`, else `app.name`.
    pub fn title(&self) -> &str {
        self.window.title.as_deref().unwrap_or(&self.app.name)
    }

    /// Settings this version accepts but does not act on yet.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.window.title_bar != TitleBar::Native {
            out.push(
                "`window.title_bar` modes other than \"native\" arrive in v0.5; \
                 using the native title bar"
                    .to_owned(),
            );
        }
        if self.window.bar_color.is_some() {
            out.push("`window.bar_color` arrives in v0.5; ignoring it".to_owned());
        }
        out
    }
}

fn secs(key: &str, value: f64) -> Result<Duration> {
    if !value.is_finite() || value < 0.0 {
        bail!("`{key}` must be a non-negative number of seconds");
    }
    Duration::try_from_secs_f64(value).with_context(|| format!("`{key}` is too large"))
}

/// `app.id` becomes a folder name, so keep it to a portable character set.
fn validate_id(id: &str) -> Result<()> {
    let ok_chars = id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if id.is_empty() || !ok_chars || id.starts_with('.') || id.len() > 128 {
        bail!(
            "`app.id` must be 1-128 characters of letters, digits, `.`, `-` or `_`, \
             not starting with `.` (e.g. \"com.example.myapp\"); got {id:?}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
        [app]
        name = "MyApp"
        id = "com.example.myapp"
        icon = "icon.png"

        [window]
        title = "My App"
        width = 1000
        height = 700
        resizable = false
        title_bar = "native"
        theme = "dark"
        single_instance = false
        remember_window = false

        [sidecar]
        path = "dist/"
        command = "server"
        args = ["--mode", "desktop"]
        ready_path = "/healthz"
        ready_timeout_secs = 5
        shutdown_grace_secs = 1.5

        [sidecar.windows]
        command = "server.exe"

        [sidecar.macos]
        command = "server-mac"
        args = ["--mac"]

        [sidecar.linux]
        command = "server-linux"
        args = ["--linux"]

        [auth]
        enabled = false
    "#;

    fn parse(text: &str) -> Result<Config> {
        Config::parse(text, Path::new("/base"))
    }

    #[test]
    fn parses_full_example() {
        let c = parse(FULL).unwrap();
        assert_eq!(c.app.id, "com.example.myapp");
        assert_eq!(c.title(), "My App");
        assert_eq!((c.window.width, c.window.height), (1000, 700));
        assert!(!c.window.resizable && !c.window.single_instance && !c.window.remember_window);
        assert_eq!(c.window.theme, Theme::Dark);
        assert_eq!(c.sidecar.path, Path::new("/base").join("dist/"));
        assert_eq!(c.sidecar.ready_path.as_deref(), Some("/healthz"));
        assert_eq!(c.sidecar.ready_timeout, Duration::from_secs(5));
        assert_eq!(c.sidecar.shutdown_grace, Duration::from_millis(1500));
        assert!(!c.auth.enabled);
        assert!(c.warnings().is_empty());

        // The per-OS table wins; Windows overrides only the command, so it
        // keeps the shared args.
        let (cmd, args): (&str, &[&str]) = match std::env::consts::OS {
            "windows" => ("server.exe", &["--mode", "desktop"]),
            "macos" => ("server-mac", &["--mac"]),
            "linux" => ("server-linux", &["--linux"]),
            _ => ("server", &["--mode", "desktop"]),
        };
        assert_eq!(c.sidecar.command.as_deref(), Some(cmd));
        assert_eq!(c.sidecar.args, args);
    }

    #[test]
    fn minimal_uses_defaults() {
        let c = parse(
            r#"
            [app]
            name = "Mini"
            id = "mini"
            [sidecar]
            path = "server.py"
            "#,
        )
        .unwrap();
        assert_eq!(c.title(), "Mini");
        assert_eq!((c.window.width, c.window.height), (1200, 800));
        assert!(c.window.resizable && c.window.single_instance && c.window.remember_window);
        assert_eq!(c.window.theme, Theme::System);
        assert!(c.auth.enabled);
        assert_eq!(c.sidecar.command, None);
        assert!(c.sidecar.args.is_empty());
        assert_eq!(c.sidecar.ready_timeout, Duration::from_secs(15));
        assert_eq!(c.sidecar.shutdown_grace, Duration::from_secs(3));
    }

    #[test]
    fn rejects_mistakes() {
        let base = "[app]\nname = \"A\"\nid = \"a\"\n";
        let cases = [
            ("", "missing [sidecar]"),
            ("[static]\nroot = \"web/\"\n", "v0.3"),
            (
                "[static]\nroot = \"web/\"\n[sidecar]\npath = \"x\"\n",
                "not both",
            ),
            ("[sidecar]\npath = \"x\"\ncomand = \"typo\"\n", "comand"),
            ("[sidecar]\npath = \"x\"\nready_path = \"healthz\"\n", "/"),
            (
                "[sidecar]\npath = \"x\"\nready_timeout_secs = -1\n",
                "non-negative",
            ),
            (
                "[window]\ntheme = \"blue\"\n[sidecar]\npath = \"x\"\n",
                "blue",
            ),
            (
                "[window]\nwidth = 10\n[sidecar]\npath = \"x\"\n",
                "at least 100",
            ),
        ];
        for (extra, needle) in cases {
            let err = format!("{:#}", parse(&format!("{base}{extra}")).unwrap_err());
            assert!(err.contains(needle), "{extra:?}: {err}");
        }
    }

    #[test]
    fn validates_id() {
        for ok in ["a", "com.example.my-app_2"] {
            assert!(validate_id(ok).is_ok(), "{ok}");
        }
        for bad in ["", ".", "..", "../x", "a/b", "a\\b", "my app", "ü"] {
            assert!(validate_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn warns_about_later_features() {
        let c = parse(
            r##"
            [app]
            name = "A"
            id = "a"
            [window]
            title_bar = "custom"
            bar_color = "#000000"
            [sidecar]
            path = "x"
            "##,
        )
        .unwrap();
        assert_eq!(c.warnings().len(), 2);
    }
}
