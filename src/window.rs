//! The desktop window: a tao window hosting a wry webview.
//!
//! This is the one place that touches the windowing and webview crates.
//! Every launch mode ends here with a URL and a [`WindowOptions`].

use anyhow::{Context, Result};
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::{Window, WindowBuilder};
use url::Url;
use wry::{NewWindowFeatures, NewWindowResponse, WebView, WebViewBuilder};

/// Title shown until the page sets its own, when `--title` is not given.
const DEFAULT_TITLE: &str = "Porthole";

/// How the window looks. Later milestones fill this from `[window]` in
/// `app.toml`; v0.1 fills it from command-line flags.
#[derive(Debug, Clone)]
pub struct WindowOptions {
    /// Fixed title. `None` follows the page's `<title>`.
    pub title: Option<String>,
    pub width: u32,
    pub height: u32,
    pub resizable: bool,
    pub devtools: bool,
}

/// Messages from webview callbacks (which may run off the main thread) to
/// the event loop.
#[derive(Debug)]
enum UserEvent {
    /// The page's `<title>` changed.
    TitleChanged(String),
    /// A same-origin `target="_blank"` link or `window.open`: Porthole is
    /// single-window, so load it in the existing window.
    Navigate(String),
}

/// Open the window on `url` and run the event loop until it is closed.
///
/// The process exits when the window closes, so this only returns on a
/// setup error.
pub fn open(url: Url, opts: WindowOptions) -> Result<()> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title(opts.title.as_deref().unwrap_or(DEFAULT_TITLE))
        .with_inner_size(LogicalSize::new(opts.width, opts.height))
        .with_resizable(opts.resizable)
        .build(&event_loop)
        .context("could not create the window")?;

    let webview = build_webview(&window, &url, &opts, proxy)
        .context("could not create the webview (is the OS webview installed?)")?;

    // Without an Edit menu, Cmd+C / Cmd+V do nothing in WKWebView. The
    // closure below owns the menu, which keeps it alive.
    #[cfg(target_os = "macos")]
    let menu = macos_menu::build();

    let follow_page_title = opts.title.is_none();
    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) =>
            {
                #[cfg(target_os = "macos")]
                if let Some(menu) = &menu {
                    menu.init_for_nsapp();
                }
            }
            Event::UserEvent(UserEvent::TitleChanged(title)) if follow_page_title => {
                let title = title.trim();
                window.set_title(if title.is_empty() {
                    DEFAULT_TITLE
                } else {
                    title
                });
            }
            Event::UserEvent(UserEvent::Navigate(target)) => {
                if let Err(err) = webview.load_url(&target) {
                    eprintln!("porthole: could not open {target}: {err}");
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                // v0.2: stop the sidecar here before exiting.
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    })
}

fn build_webview(
    window: &Window,
    url: &Url,
    opts: &WindowOptions,
    proxy: EventLoopProxy<UserEvent>,
) -> wry::Result<WebView> {
    let title_proxy = proxy.clone();
    let app_origin = url.origin();

    let builder = WebViewBuilder::new()
        .with_url(url.as_str())
        .with_devtools(opts.devtools)
        .with_document_title_changed_handler(move |title| {
            let _ = title_proxy.send_event(UserEvent::TitleChanged(title));
        })
        .with_new_window_req_handler(move |target: String, _features: NewWindowFeatures| {
            match new_window_action(&app_origin, &target) {
                NewWindowAction::LoadHere => {
                    let _ = proxy.send_event(UserEvent::Navigate(target));
                }
                NewWindowAction::OpenInBrowser => {
                    if let Err(err) = open::that_detached(&target) {
                        eprintln!("porthole: could not open {target} in the browser: {err}");
                    }
                }
                NewWindowAction::Ignore => {}
            }
            NewWindowResponse::Deny
        });

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        builder.build(window)
    }

    // GTK platforms: attach to the window's GTK container, which works on
    // both X11 and Wayland (`build` needs an X11 window handle).
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window
            .default_vbox()
            .expect("tao windows on GTK platforms have a default vbox");
        builder.build_gtk(vbox)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum NewWindowAction {
    LoadHere,
    OpenInBrowser,
    Ignore,
}

/// Decide what to do with a request for a new window (`target="_blank"`,
/// `window.open`). Porthole never opens a second window.
fn new_window_action(app_origin: &url::Origin, target: &str) -> NewWindowAction {
    let Ok(target_url) = Url::parse(target) else {
        return NewWindowAction::Ignore;
    };
    if &target_url.origin() == app_origin {
        return NewWindowAction::LoadHere;
    }
    match target_url.scheme() {
        "http" | "https" | "mailto" => NewWindowAction::OpenInBrowser,
        // `javascript:`, `file:`, `data:` and custom schemes: do nothing
        // rather than hand them to the OS.
        _ => NewWindowAction::Ignore,
    }
}

#[cfg(target_os = "macos")]
mod macos_menu {
    use muda::{Menu, PredefinedMenuItem, Submenu};

    /// Build the standard App / Edit / Window menus. Install it with
    /// `init_for_nsapp` once the event loop starts, and keep it alive for as
    /// long as the app runs.
    pub fn build() -> Option<Menu> {
        let menu = Menu::new();
        let app = Submenu::with_items(
            "Porthole",
            true,
            &[
                &PredefinedMenuItem::about(None, None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::hide(None),
                &PredefinedMenuItem::hide_others(None),
                &PredefinedMenuItem::show_all(None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::quit(None),
            ],
        );
        let edit = Submenu::with_items(
            "Edit",
            true,
            &[
                &PredefinedMenuItem::undo(None),
                &PredefinedMenuItem::redo(None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::cut(None),
                &PredefinedMenuItem::copy(None),
                &PredefinedMenuItem::paste(None),
                &PredefinedMenuItem::select_all(None),
            ],
        );
        let window = Submenu::with_items(
            "Window",
            true,
            &[
                &PredefinedMenuItem::minimize(None),
                &PredefinedMenuItem::fullscreen(None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::close_window(None),
            ],
        );
        let built = (|| -> muda::Result<()> {
            let (app, edit, window) = (app?, edit?, window?);
            menu.append_items(&[&app, &edit, &window])?;
            Ok(())
        })();
        match built {
            Ok(()) => Some(menu),
            Err(err) => {
                eprintln!("porthole: could not build the menu bar: {err}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(target: &str) -> NewWindowAction {
        let origin = Url::parse("http://localhost:3000/").unwrap().origin();
        new_window_action(&origin, target)
    }

    #[test]
    fn new_window_routing() {
        assert_eq!(
            action("http://localhost:3000/other"),
            NewWindowAction::LoadHere
        );
        assert_eq!(
            action("http://localhost:4000/"),
            NewWindowAction::OpenInBrowser
        );
        assert_eq!(
            action("https://example.com/"),
            NewWindowAction::OpenInBrowser
        );
        assert_eq!(
            action("mailto:a@example.com"),
            NewWindowAction::OpenInBrowser
        );
        assert_eq!(action("file:///etc/passwd"), NewWindowAction::Ignore);
        assert_eq!(action("javascript:alert(1)"), NewWindowAction::Ignore);
        assert_eq!(action("not a url"), NewWindowAction::Ignore);
    }
}
