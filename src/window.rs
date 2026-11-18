//! The desktop window: a tao window hosting a wry webview.
//!
//! This is the one place that touches the windowing and webview crates.
//! Every launch mode ends here with a URL and a [`WindowOptions`].

use std::borrow::Cow;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tao::dpi::{LogicalSize, PhysicalPosition};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder, EventLoopProxy};
use tao::window::{Icon, Theme, Window, WindowBuilder};
use url::Url;
use wry::http::{Request, Response};
use wry::{NewWindowFeatures, NewWindowResponse, WebContext, WebView, WebViewBuilder};

use crate::config::TitleBar;
use crate::dialog::{self, ErrorReport};
use crate::logs;
use crate::window_api::{self, Command};
use crate::window_state::{self, WindowState};

/// Title shown until the page sets its own, when `--title` is not given.
const DEFAULT_TITLE: &str = "Porthole";

/// How the window looks. Filled from `[window]` in `app.toml`, or from
/// command-line flags for `run --url`.
#[derive(Debug, Clone)]
pub struct WindowOptions {
    /// Fixed title. `None` follows the page's `<title>`.
    pub title: Option<String>,
    pub width: u32,
    pub height: u32,
    pub resizable: bool,
    pub devtools: bool,
    /// `None` follows the system theme.
    pub theme: Option<Theme>,
    /// Where to remember size, position and maximised state; `None` to
    /// always start at the default size.
    pub state_file: Option<PathBuf>,
    /// Where the webview keeps cookies, storage and caches. `None` uses the
    /// platform default.
    pub webview_data_dir: Option<PathBuf>,
    /// A custom URL scheme answered in-process (static mode).
    pub protocol: Option<CustomProtocol>,
    /// The OS title bar, or one the page draws itself.
    pub title_bar: TitleBar,
    /// The native title bar's colour (Windows 11, macOS).
    pub bar_color: Option<(u8, u8, u8)>,
    pub icon: Option<Icon>,
    /// The page is the app itself (`app.toml`), not a server the developer
    /// pointed `run --url` at: inject the script that sends links to other
    /// sites to the browser.
    pub app_page: bool,
    /// How long the launcher waited for the backend before opening the
    /// window, for the startup timing in the log.
    pub startup_wait: Duration,
}

impl WindowOptions {
    /// Whether the page gets the window-control API.
    fn controls(&self) -> bool {
        self.title_bar != TitleBar::Native
    }
}

/// Turns a request for a custom scheme into a response.
pub type ProtocolHandler =
    Arc<dyn Fn(&Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> + Send + Sync>;

/// Answers requests for one URL scheme without a network socket.
#[derive(Clone)]
pub struct CustomProtocol {
    pub scheme: &'static str,
    pub handler: ProtocolHandler,
}

impl std::fmt::Debug for CustomProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomProtocol")
            .field("scheme", &self.scheme)
            .finish_non_exhaustive()
    }
}

/// Lets other threads reach the window while it is open.
#[derive(Clone)]
pub struct WindowHandle(EventLoopProxy<UserEvent>);

impl WindowHandle {
    /// Bring the window to the front (a second launch of a single-instance app).
    pub fn focus(&self) {
        let _ = self.0.send_event(UserEvent::Focus);
    }

    /// Show `report` in an error dialog, then close the app.
    pub fn fail(&self, report: ErrorReport) {
        let _ = self.0.send_event(UserEvent::Fatal(Box::new(report)));
    }
}

/// Code the caller runs at points in the window's life.
#[derive(Default)]
pub struct Hooks {
    /// Runs before the event loop starts, with a handle for other threads.
    pub on_open: Option<Box<dyn FnOnce(WindowHandle)>>,
    /// Runs once when the app is about to exit (window closed, or after a
    /// fatal error), after the window is hidden. The process exits when it
    /// returns.
    pub on_exit: Option<Box<dyn FnOnce()>>,
}

/// Messages from webview callbacks (which may run off the main thread) to
/// the event loop.
#[derive(Debug)]
pub enum UserEvent {
    /// The page's `<title>` changed.
    TitleChanged(String),
    /// A same-origin `target="_blank"` link or `window.open`: Porthole is
    /// single-window, so load it in the existing window.
    Navigate(String),
    /// Bring the window to the front.
    Focus,
    /// Something the app depends on failed: show it and exit.
    Fatal(Box<ErrorReport>),
    /// The page asked to close the window (custom title bar).
    Close,
    /// The page asked whether the window is maximised.
    ReportMaximized,
}

/// Open the window on `url` and run the event loop until it is closed.
///
/// The process exits when the window closes, so this only returns on a
/// setup error.
pub fn open(url: Url, opts: WindowOptions, hooks: Hooks) -> Result<()> {
    // WebView2 ships with Windows 11 but not with every Windows 10 install.
    #[cfg(windows)]
    if let Err(err) = wry::webview_version() {
        let app = opts.title.as_deref().unwrap_or(DEFAULT_TITLE);
        dialog::webview_missing(app, &err.to_string());
        return Err(anyhow::Error::new(crate::app::Reported(
            "the WebView2 Runtime is not installed".to_owned(),
        )));
    }
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let saved = opts.state_file.as_deref().and_then(WindowState::load);
    let mut builder = WindowBuilder::new()
        .with_title(opts.title.as_deref().unwrap_or(DEFAULT_TITLE))
        .with_inner_size(LogicalSize::new(opts.width, opts.height))
        .with_resizable(opts.resizable)
        .with_theme(opts.theme)
        .with_window_icon(opts.icon.clone());
    builder = title_bar::configure(builder, opts.title_bar, opts.bar_color);
    if let Some(state) = &saved {
        builder = restore(builder, &event_loop, state, opts.resizable);
    }
    let window = Rc::new(
        builder
            .build(&event_loop)
            .context("could not create the window")?,
    );
    title_bar::after_build(&window, opts.title_bar, opts.bar_color);

    let mut web_context = WebContext::new(opts.webview_data_dir.clone());
    let webview = build_webview(&window, &url, &opts, &mut web_context, proxy)
        .context("could not create the webview (is the OS webview installed?)")?;
    let startup = logs::since_start();
    logs::info!(
        "startup: window shown {} ms after launch, {} ms of it waiting for the backend",
        startup.as_millis(),
        opts.startup_wait.as_millis()
    );

    // Without an Edit menu, Cmd+C / Cmd+V do nothing in WKWebView. The
    // closure below owns the menu, which keeps it alive.
    #[cfg(target_os = "macos")]
    let menu = macos_menu::build();

    if let Some(on_open) = hooks.on_open {
        on_open(WindowHandle(event_loop.create_proxy()));
    }
    let mut on_exit = hooks.on_exit;
    let controls = opts.controls();
    let state_file = opts.state_file;
    // The last size and position the window had while neither maximised
    // nor minimised: what to restore next time.
    let mut normal = match saved {
        // Started maximised: the current size is the maximised one.
        Some(s) if s.maximized => WindowState {
            maximized: false,
            ..s
        },
        _ => current_state(&window, saved.as_ref()),
    };

    let follow_page_title = opts.title.is_none();
    let mut was_maximized = window.is_maximized();
    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        // The webview needs its context for as long as it lives.
        let _ = &web_context;
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
                    logs::warning!("could not open {target}: {err}");
                }
            }
            Event::UserEvent(UserEvent::Focus) => {
                window.set_minimized(false);
                window.set_visible(true);
                window.set_focus();
            }
            Event::UserEvent(UserEvent::ReportMaximized) => {
                was_maximized = window.is_maximized();
                let _ = webview.evaluate_script(&window_api::report_maximized(was_maximized));
            }
            Event::UserEvent(UserEvent::Fatal(report)) => {
                window.set_visible(false);
                if let Some(f) = on_exit.take() {
                    f();
                }
                dialog::show_error(&report);
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::Moved(_) | WindowEvent::Resized(_),
                ..
            } => {
                let maximized = window.is_maximized();
                if !maximized && !window.is_minimized() {
                    normal = current_state(&window, Some(&normal));
                }
                if controls && maximized != was_maximized {
                    was_maximized = maximized;
                    let _ = webview.evaluate_script(&window_api::report_maximized(maximized));
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            }
            | Event::UserEvent(UserEvent::Close) => {
                if let Some(path) = &state_file {
                    let state = WindowState {
                        maximized: window.is_maximized(),
                        ..normal
                    };
                    if let Err(err) = state.save(path) {
                        logs::warning!(
                            "could not save the window state to {}: {err}",
                            path.display()
                        );
                    }
                }
                // Hide first so closing feels instant while the backend
                // gets its grace period.
                window.set_visible(false);
                if let Some(f) = on_exit.take() {
                    f();
                }
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    })
}

/// Apply saved state to the builder. A saved position that is no longer on
/// any screen (a monitor was unplugged) is replaced by the primary
/// monitor's centre.
fn restore(
    mut builder: WindowBuilder,
    event_loop: &EventLoop<UserEvent>,
    state: &WindowState,
    resizable: bool,
) -> WindowBuilder {
    if resizable {
        builder = builder.with_inner_size(LogicalSize::new(state.width, state.height));
    }
    builder = builder.with_maximized(state.maximized && resizable);

    let (Some(x), Some(y)) = (state.x, state.y) else {
        return builder;
    };
    let monitors: Vec<window_state::Rect> = event_loop
        .available_monitors()
        .map(|m| {
            let (p, s) = (m.position(), m.size());
            (p.x, p.y, s.width, s.height)
        })
        .collect();
    let scale = event_loop
        .primary_monitor()
        .map_or(1.0, |m| m.scale_factor());
    let physical: tao::dpi::PhysicalSize<u32> =
        LogicalSize::new(state.width, state.height).to_physical(scale);
    let size = (physical.width, physical.height);

    if window_state::is_on_screen((x, y), size, &monitors) {
        builder.with_position(PhysicalPosition::new(x, y))
    } else if let Some(primary) = event_loop.primary_monitor() {
        let (p, s) = (primary.position(), primary.size());
        let (cx, cy) = window_state::centered(size, (p.x, p.y, s.width, s.height));
        builder.with_position(PhysicalPosition::new(cx, cy))
    } else {
        builder
    }
}

/// The window's current bounds. Platforms that can't report a position
/// keep the previous one.
fn current_state(window: &Window, previous: Option<&WindowState>) -> WindowState {
    let size = window.inner_size().to_logical::<f64>(window.scale_factor());
    let pos = window.outer_position().ok();
    WindowState {
        x: pos.map(|p| p.x).or(previous.and_then(|s| s.x)),
        y: pos.map(|p| p.y).or(previous.and_then(|s| s.y)),
        width: size.width,
        height: size.height,
        maximized: false,
    }
}

fn build_webview(
    window: &Rc<Window>,
    url: &Url,
    opts: &WindowOptions,
    web_context: &mut WebContext,
    proxy: EventLoopProxy<UserEvent>,
) -> wry::Result<WebView> {
    let title_proxy = proxy.clone();
    let app_url = url.clone();

    let mut builder = WebViewBuilder::new_with_web_context(web_context);
    if opts.app_page {
        builder = builder
            .with_initialization_script_for_main_only(window_api::script(opts.title_bar), true);
    }
    if opts.app_page && opts.controls() {
        let (window, proxy, app_url) = (window.clone(), proxy.clone(), url.clone());
        builder = builder.with_ipc_handler(move |request| {
            on_window_command(&window, &proxy, &app_url, request);
        });
    }
    if let Some(protocol) = &opts.protocol {
        let handler = protocol.handler.clone();
        builder = builder.with_custom_protocol(protocol.scheme.to_owned(), move |_id, request| {
            handler(&request)
        });
    }
    let builder = builder
        .with_url(url.as_str())
        .with_devtools(opts.devtools)
        .with_document_title_changed_handler(move |title| {
            let _ = title_proxy.send_event(UserEvent::TitleChanged(title));
        })
        .with_new_window_req_handler(move |target: String, _features: NewWindowFeatures| {
            match new_window_action(&app_url, &target) {
                NewWindowAction::LoadHere => {
                    let _ = proxy.send_event(UserEvent::Navigate(target));
                }
                NewWindowAction::OpenInBrowser => {
                    if let Err(err) = open::that_detached(&target) {
                        logs::warning!("could not open {target} in the browser: {err}");
                    }
                }
                NewWindowAction::Ignore => {}
            }
            NewWindowResponse::Deny
        })
        .with_on_page_load_handler(|event, url| {
            if let wry::PageLoadEvent::Finished = event {
                // Never log the query: the first URL carries the token.
                let url = url.split(['?', '#']).next().unwrap_or_default();
                logs::info!("page loaded: {url}");
            }
        })
        .with_download_started_handler(downloads::started)
        .with_download_completed_handler(downloads::completed);

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

/// A message from the page's window-control API (custom title bars).
fn on_window_command(
    window: &Window,
    proxy: &EventLoopProxy<UserEvent>,
    app_url: &Url,
    request: Request<String>,
) {
    let page = request.uri().to_string();
    if !window_api::allowed(app_url, &page) {
        logs::warning!("ignored a window-control message from {page}");
        return;
    }
    let Some(command) = Command::parse(request.body()) else {
        logs::warning!("ignored an unknown window-control message");
        return;
    };
    let result = match command {
        Command::Minimize => {
            window.set_minimized(true);
            Ok(())
        }
        Command::ToggleMaximize => {
            window.set_maximized(!window.is_maximized());
            Ok(())
        }
        Command::Close => {
            let _ = proxy.send_event(UserEvent::Close);
            Ok(())
        }
        Command::IsMaximized => {
            let _ = proxy.send_event(UserEvent::ReportMaximized);
            Ok(())
        }
        Command::Drag => window.drag_window(),
        Command::Resize(direction) => window.drag_resize_window(direction),
    };
    if !matches!(command, Command::Drag | Command::Resize(_)) {
        logs::info!("window control: {}", request.body());
    }
    if let Err(err) = result {
        logs::warning!("window control {} failed: {err}", request.body());
    }
}

/// Downloads: ask where to save with the native "Save as" dialog, starting
/// in the user's Downloads folder. (WKWebView drops downloads that nobody
/// handles.)
mod downloads {
    use std::path::PathBuf;

    use crate::dialog;
    use crate::logs;

    /// `path` arrives as `<Downloads>/<suggested name>`, where wry falls
    /// back to the current folder if it can't find a Downloads folder.
    pub fn started(url: String, path: &mut PathBuf) -> bool {
        let shown = url.split(['?', '#']).next().unwrap_or_default().to_owned();
        let name = path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| "download".into());
        let suggested = unique(downloads_dir().join(&name));
        if std::env::var_os(dialog::NO_DIALOGS_ENV).is_some() {
            logs::info!("download of {shown} to {}", suggested.display());
            *path = suggested;
            return true;
        }
        let mut dialog = rfd::FileDialog::new().set_title("Save as");
        if let Some(dir) = suggested.parent() {
            dialog = dialog.set_directory(dir);
        }
        if let Some(name) = suggested.file_name() {
            dialog = dialog.set_file_name(name.to_string_lossy());
        }
        match dialog.save_file() {
            Some(chosen) => {
                logs::info!("download of {shown} to {}", chosen.display());
                *path = chosen;
                true
            }
            None => {
                logs::info!("download of {shown} cancelled");
                false
            }
        }
    }

    /// The user's Downloads folder, else their home folder.
    fn downloads_dir() -> PathBuf {
        if let Some(dir) =
            directories::UserDirs::new().and_then(|u| u.download_dir().map(PathBuf::from))
        {
            return dir;
        }
        match directories::BaseDirs::new() {
            Some(b) if b.home_dir().join("Downloads").is_dir() => b.home_dir().join("Downloads"),
            Some(b) => b.home_dir().to_path_buf(),
            None => PathBuf::from("."),
        }
    }

    /// `path`, or `name (1).ext`, `name (2).ext`... if it exists.
    fn unique(path: PathBuf) -> PathBuf {
        if !path.exists() {
            return path;
        }
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let ext = path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        (1..)
            .map(|i| path.with_file_name(format!("{stem} ({i}){ext}")))
            .find(|p| !p.exists())
            .expect("some name is free")
    }

    pub fn completed(url: String, path: Option<PathBuf>, ok: bool) {
        let url = url.split(['?', '#']).next().unwrap_or_default();
        let to = path
            .map(|p| format!(" to {}", p.display()))
            .unwrap_or_default();
        if ok {
            logs::info!("downloaded {url}{to}");
        } else {
            logs::warning!("the download of {url}{to} failed");
        }
    }
}

/// Title bar modes and colours, per platform.
mod title_bar {
    use tao::window::{Window, WindowBuilder};

    use crate::config::TitleBar;

    pub fn configure(
        builder: WindowBuilder,
        mode: TitleBar,
        color: Option<(u8, u8, u8)>,
    ) -> WindowBuilder {
        #[cfg(target_os = "macos")]
        {
            use tao::platform::macos::WindowBuilderExtMacOS;
            match mode {
                TitleBar::Native => match color {
                    // A transparent bar shows the window's background
                    // colour through it.
                    Some((r, g, b)) => builder
                        .with_titlebar_transparent(true)
                        .with_background_color((r, g, b, 255)),
                    None => builder,
                },
                TitleBar::Overlay => builder
                    .with_titlebar_transparent(true)
                    .with_title_hidden(true)
                    .with_fullsize_content_view(true),
                TitleBar::Custom => builder.with_decorations(false),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = color;
            match mode {
                TitleBar::Native => builder,
                // Overlay is a macOS look; elsewhere it is a custom bar.
                TitleBar::Custom | TitleBar::Overlay => builder.with_decorations(false),
            }
        }
    }

    /// Settings that need the native window.
    pub fn after_build(window: &Window, mode: TitleBar, color: Option<(u8, u8, u8)>) {
        #[cfg(windows)]
        if let (TitleBar::Native, Some((r, g, b))) = (mode, color) {
            use tao::platform::windows::WindowExtWindows;
            use windows_sys::Win32::Graphics::Dwm::{DWMWA_CAPTION_COLOR, DwmSetWindowAttribute};
            // COLORREF is 0x00BBGGRR. Windows 10 ignores this attribute.
            let colorref: u32 = u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16);
            // SAFETY: a valid window handle and a pointer to a u32 that
            // outlives the call.
            unsafe {
                DwmSetWindowAttribute(
                    window.hwnd() as _,
                    DWMWA_CAPTION_COLOR as u32,
                    (&colorref as *const u32).cast(),
                    std::mem::size_of::<u32>() as u32,
                );
            }
        }
        #[cfg(not(windows))]
        let _ = (window, mode, color);
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
fn new_window_action(app_url: &Url, target: &str) -> NewWindowAction {
    let Ok(target_url) = Url::parse(target) else {
        return NewWindowAction::Ignore;
    };
    if same_origin(app_url, &target_url) {
        return NewWindowAction::LoadHere;
    }
    match target_url.scheme() {
        "http" | "https" | "mailto" => NewWindowAction::OpenInBrowser,
        // `javascript:`, `file:`, `data:` and custom schemes: do nothing
        // rather than hand them to the OS.
        _ => NewWindowAction::Ignore,
    }
}

/// Scheme, host and port match. Unlike [`Url::origin`], this also works
/// for custom schemes such as `porthole://`, whose origins `url` treats as
/// opaque (never equal to anything).
pub fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
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
                crate::logs::warning!("could not build the menu bar: {err}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(target: &str) -> NewWindowAction {
        let app = Url::parse("http://localhost:3000/").unwrap();
        new_window_action(&app, target)
    }

    #[test]
    fn custom_scheme_counts_as_the_app() {
        let app = Url::parse("porthole://localhost/").unwrap();
        assert_eq!(
            new_window_action(&app, "porthole://localhost/help.html"),
            NewWindowAction::LoadHere
        );
        assert_eq!(
            new_window_action(&app, "porthole://elsewhere/"),
            NewWindowAction::Ignore
        );
        assert_eq!(
            new_window_action(&app, "https://example.com/"),
            NewWindowAction::OpenInBrowser
        );
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
