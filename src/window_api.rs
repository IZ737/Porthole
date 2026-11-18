//! The script Porthole injects into the app's pages, and the fixed set of
//! window-control messages it sends back over wry's IPC channel.
//!
//! In every app mode the script sends plain links to other sites to the
//! system browser. With `title_bar = "custom"` or `"overlay"` it also
//! provides `window.porthole.window` (minimise, maximise, close, drag) for
//! the page's own title bar. This is deliberately not a general bridge
//! between the page and the launcher: unknown messages, and messages from
//! any origin other than the app's own, are dropped.

use tao::window::ResizeDirection;
use url::Url;

use crate::config::TitleBar;

const SCRIPT: &str = include_str!("window_api.js");

/// The initialization script for `title_bar` on this platform. Custom
/// and overlay bars get the window-control API; on Linux the page's outer
/// 5 px also resize the frameless window (the webview covers its edges).
pub fn script(title_bar: TitleBar) -> String {
    let mode = match title_bar {
        TitleBar::Native => "native",
        TitleBar::Overlay if cfg!(target_os = "macos") => "overlay",
        TitleBar::Custom | TitleBar::Overlay => "custom",
    };
    let controls = title_bar != TitleBar::Native;
    let resize_edges = controls && cfg!(not(any(target_os = "windows", target_os = "macos")));
    SCRIPT
        .replace("__PORTHOLE_CONTROLS__", bool_js(controls))
        .replace("__PORTHOLE_TITLE_BAR__", &format!("{mode:?}"))
        .replace("__PORTHOLE_RESIZE__", bool_js(resize_edges))
}

fn bool_js(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

/// JavaScript that tells the page whether the window is maximised.
pub fn report_maximized(maximized: bool) -> String {
    format!("window.__portholeMaximized && window.__portholeMaximized({maximized})")
}

/// A message from the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Minimize,
    ToggleMaximize,
    Close,
    IsMaximized,
    /// Start moving the window (mouse down on a `data-porthole-drag` region).
    Drag,
    /// Start resizing the window from an edge.
    Resize(ResizeDirection),
}

impl Command {
    pub fn parse(message: &str) -> Option<Self> {
        Some(match message {
            "minimize" => Self::Minimize,
            "toggle-maximize" => Self::ToggleMaximize,
            "close" => Self::Close,
            "is-maximized" => Self::IsMaximized,
            "drag" => Self::Drag,
            other => Self::Resize(match other.strip_prefix("resize-")? {
                "n" => ResizeDirection::North,
                "s" => ResizeDirection::South,
                "e" => ResizeDirection::East,
                "w" => ResizeDirection::West,
                "ne" => ResizeDirection::NorthEast,
                "nw" => ResizeDirection::NorthWest,
                "se" => ResizeDirection::SouthEast,
                "sw" => ResizeDirection::SouthWest,
                _ => return None,
            }),
        })
    }
}

/// Whether a message sent from the page at `page_url` may control the
/// window: only the app's own origin may.
pub fn allowed(app_url: &Url, page_url: &str) -> bool {
    Url::parse(page_url).is_ok_and(|page| crate::window::same_origin(app_url, &page))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(Command::parse("minimize"), Some(Command::Minimize));
        assert_eq!(Command::parse("close"), Some(Command::Close));
        assert_eq!(
            Command::parse("resize-se"),
            Some(Command::Resize(ResizeDirection::SouthEast))
        );
        for bad in ["", "resize-", "resize-x", "eval", "Minimize", "drag "] {
            assert_eq!(Command::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn only_the_app_may_send() {
        let app = Url::parse("http://127.0.0.1:5000/?token=x").unwrap();
        assert!(allowed(&app, "http://127.0.0.1:5000/settings"));
        assert!(!allowed(&app, "http://127.0.0.1:5001/"));
        assert!(!allowed(&app, "http://localhost:5000/"));
        assert!(!allowed(&app, "https://example.com/"));
        assert!(!allowed(&app, "garbage"));
        let app = Url::parse("porthole://localhost/").unwrap();
        assert!(allowed(&app, "porthole://localhost/index.html"));
        assert!(!allowed(&app, "porthole://other/"));
    }

    #[test]
    fn script_placeholders_are_filled() {
        for mode in [TitleBar::Native, TitleBar::Custom, TitleBar::Overlay] {
            let s = script(mode);
            assert!(!s.contains("__PORTHOLE_"), "{s}");
        }
        assert!(script(TitleBar::Native).contains("const controls = false;"));
        assert!(script(TitleBar::Native).contains("const resizeEdges = false;"));
        let custom = script(TitleBar::Custom);
        assert!(custom.contains("const titleBar = \"custom\";"));
        assert_eq!(
            custom.contains("const resizeEdges = true;"),
            cfg!(target_os = "linux")
        );
    }
}
