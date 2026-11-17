# Porthole

Porthole turns any "backend + localhost web UI" app into a desktop app. It
opens the app's local URL in the operating system's own webview (WebView2,
WKWebView or WebKitGTK), so the launcher stays around 1–3 MB instead of
shipping a browser.

> **Status: v0.2.** Porthole can start your backend as a sidecar and open a
> window on it, or open a window on a server you have already started.
> Packing everything into a single executable (v0.3) is next. See the
> [roadmap](#roadmap).

## Run your backend as a sidecar

Describe the app in an `app.toml` and run it:

```sh
porthole run app.toml
porthole run app.toml --console    # also stream the logs to this terminal
```

Porthole picks a free port and a random token, starts your backend with
them, waits until it answers, and opens a window. Closing the window stops
the backend. A complete example lives in
[`examples/hello-python`](examples/hello-python).

```toml
[app]
name = "MyApp"
id = "com.example.myapp"    # names the data folder and the single-instance lock

[window]
title = "My App"            # default: app.name
width = 1200
height = 800
resizable = true
theme = "system"            # light | dark | system
single_instance = true      # a second launch focuses the running window
remember_window = true      # restore size, position and maximised state

[sidecar]
path = "dist/"              # file or folder, relative to app.toml
command = "server"          # relative to path; a bare name like "python3" falls back to PATH
args = ["--mode", "desktop"]
ready_path = "/healthz"     # optional; plain TCP connect if omitted
ready_timeout_secs = 15
shutdown_grace_secs = 3

[sidecar.windows]           # optional per-OS overrides of command and args
command = "server.exe"      # also [sidecar.macos] and [sidecar.linux]

[auth]
enabled = true              # false = no APP_TOKEN, for trusted dev setups
```

If `path` is a single executable, `command` can be left out.

### What your backend gets

| Variable | Example | Meaning |
| --- | --- | --- |
| `APP_PORT` | `51837` | Port to listen on, on `127.0.0.1` only |
| `APP_TOKEN` | 64 hex characters | Per-launch secret (absent when `auth.enabled = false`) |
| `APP_DATA_DIR` | see below | Writable per-user folder for the app |

The working directory is the sidecar's folder. The window's first request
is `http://127.0.0.1:<port>/?token=<APP_TOKEN>`. The backend should check
the token in constant time, set the cookie
`app_session=<APP_TOKEN>; HttpOnly; SameSite=Strict; Path=/`, redirect to
the same path without the query string, and answer 401 to any other
request without that cookie. It should also reject requests whose `Host`
header isn't `127.0.0.1:<port>` or `localhost:<port>`. The example server
shows all of this in about 30 lines of standard-library Python.

### Startup, shutdown and crashes

- **Readiness.** Porthole polls every 50 ms until the port accepts a
  connection, or, with `ready_path`, until `GET <ready_path>` answers with
  a 2xx, 3xx, 401 or 403 (the probe carries no session cookie, so a 401
  still proves the server is up). If the backend exits first or
  `ready_timeout_secs` passes, Porthole shows an error dialog with the last
  20 lines of `sidecar.log` and offers to open the logs folder.
- **Shutdown.** Closing the window asks the backend to stop (`SIGTERM` to
  its process group on macOS and Linux, `Ctrl+Break` on Windows), waits up
  to `shutdown_grace_secs`, then kills it and anything it started.
- **The backend never outlives Porthole**, even if Porthole crashes or is
  killed: a Job Object on Windows, `PR_SET_PDEATHSIG` on Linux, and a tiny
  reaper process on macOS. The backend runs in its own process group.
- **If the backend exits** while the window is open, Porthole shows the
  error dialog and closes. There is no automatic restart.

### Data, logs and window state

`APP_DATA_DIR` is `<data dir>/<app.id>`: `%APPDATA%\<id>` on Windows,
`~/Library/Application Support/<id>` on macOS and
`$XDG_DATA_HOME/<id>` (usually `~/.local/share/<id>`) on Linux. Porthole
keeps its own files there too:

| Path | Contents |
| --- | --- |
| `logs/porthole.log` | The launcher's log |
| `logs/sidecar.log` | Your backend's stdout and stderr |
| `porthole-window.json` | Saved window size, position and maximised state |
| `webview/` | The webview's cookies, storage and caches |
| `porthole.lock`, `porthole-instance` | Single-instance lock and hand-over |

Logs rotate at 5 MB, keeping the last 3 (`sidecar.log.1` to `.3`). A saved
position that is no longer on any screen falls back to the centre of the
primary display.

Set `PORTHOLE_NO_DIALOGS=1` to skip error dialogs on headless machines
such as CI; errors still go to stderr and the log.

## Open a window on a running server

Start your server, then point Porthole at it:

```sh
npm run dev                                   # e.g. Vite on http://localhost:5173
porthole run --url http://localhost:5173
```

`--url` also accepts `localhost:5173` or just `5173`. This is the workflow
for frontend hot reload during development.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--url <URL>` | required | The server to open |
| `--title <TEXT>` | page `<title>` | Fixed window title |
| `--width <PX>`, `--height <PX>` | 1200 × 800 | Initial window size |
| `--fixed-size` | off | Make the window non-resizable |
| `--devtools` | off | Allow opening the web inspector |
| `--wait <SECS>` | 10 | How long to wait for the server to accept connections |
| `--no-wait` | off | Open the window without checking the server first |
| `--allow-remote` | off | Allow URLs that are not on this machine |

Behaviour worth knowing:

- **Loopback only by default.** URLs must point at `localhost`,
  `*.localhost`, `127.0.0.0/8` or `::1` unless you pass `--allow-remote`.
- **One window.** Links with `target="_blank"` and `window.open` load in the
  same window when they stay on the app's origin. Links to other sites open
  in your default browser.
- **Waits for the server.** Porthole retries the connection every 50 ms
  until `--wait` runs out, so you can start the server and Porthole together.
- **macOS menus.** Porthole installs the standard App, Edit and Window menus
  so copy, paste and other shortcuts work in the webview.

## Building

Requires Rust 1.90 or newer.

```sh
cargo build --release      # binary at target/release/porthole
```

Platform requirements, at build time and on the user's machine:

| Platform | Webview | Needs |
| --- | --- | --- |
| Windows 10/11 | WebView2 | The WebView2 runtime (preinstalled on Windows 11 and most updated Windows 10 machines) |
| macOS 11+ | WKWebView | Nothing extra |
| Linux | WebKitGTK | `libwebkit2gtk-4.1` and GTK 3. To build on Debian/Ubuntu: `sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev` |

CI builds and tests on Ubuntu 24.04, macOS (arm64) and Windows, including
a test that kills a stand-in launcher outright and checks its sidecar dies
with it, and runs window smoke tests on Linux under Xvfb. The sidecar tests
use Python, and skip themselves where it is not installed.

## Roadmap

1. **v0.1 — Window.** `run --url` opens a window on an existing localhost server.
2. **v0.2 — Sidecar.** `run app.toml` starts your backend with `APP_PORT`,
   `APP_TOKEN` and `APP_DATA_DIR`, waits for it, and stops it with the window.
3. **v0.3 — Pack.** `pack` and `inspect`: one executable with the backend
   (or a static site) inside.
4. **v0.4 — Auth kit.** The token handshake spec and reference middlewares.
5. **v0.5 — Release polish.** Title bar modes, downloads, signing,
   cross-packing, prebuilt launchers.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
