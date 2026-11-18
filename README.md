# Porthole

Porthole turns any "backend + localhost web UI" app into a desktop app. It
opens the app's local URL in the operating system's own webview (WebView2,
WKWebView or WebKitGTK), so the launcher stays around 1–3 MB instead of
shipping a browser.

> **Status: v0.4.** Porthole can start your backend as a sidecar, serve a
> static site itself, or open a window on a server you have already
> started, and `pack` turns any of the first two into a single executable.
> The [auth kit](auth-kit) protects a sidecar in one line for Express and
> FastAPI. Release polish (v0.5) is next. See the [roadmap](#roadmap).

## Pack an app into one executable

```sh
porthole pack app.toml -o MyApp          # MyApp.exe on Windows
porthole inspect MyApp                   # what's inside
./MyApp                                  # runs the app
```

`pack` copies the `porthole` binary, appends your app (the sidecar or the
static site, plus `app.toml`) as a zstd-compressed tar payload, and ends
the file with a small footer. The operating system ignores the trailing
bytes, so the result is a normal executable. When it starts, it finds its
own payload and runs the app; a `porthole` binary without a payload is the
command-line tool.

| Flag | Meaning |
| --- | --- |
| `-o`, `--output <FILE>` | Where to write the executable. Default: `<app.name>` in the current folder |
| `--devtools` | Allow the web inspector in the packed app (off by default) |

`pack` bundles `sidecar.path` (one file or a whole folder, for runtimes
such as Python, Node or .NET) or `static.root`, plus `app.icon` if set.
Unix permission bits and symlinks are kept, so executables still run after
unpacking. It packs for the platform it runs on: build Windows apps on
Windows, and so on. Cross-packing, code signing and macOS `.app` bundles
arrive in v0.5; sign *after* packing.

**At launch**, a packed sidecar app unpacks once per version into
`<cache dir>/porthole/<first 16 hex digits of the payload's sha256>/`
(`%LOCALAPPDATA%`, `~/Library/Caches` or `$XDG_CACHE_HOME`), verifying the
sha256 on the way, and runs from there on every later launch. Unpacking
goes to a temporary folder that is renamed into place, so a crash or a
second instance never sees a half-written copy. Cache folders of versions
not launched for 30 days are deleted. Static sites skip the cache and are
served from memory (see below).

A packed app accepts two flags of its own; anything else on its command
line is ignored:

| Flag | Meaning |
| --- | --- |
| `--console` | Stream the logs to the terminal (as `run --console` does) |
| `--verify` | Throw away the cached copy and unpack afresh from the verified payload |

Set `PORTHOLE_CACHE_DIR` to use another cache folder.

### Packed file format

```
[ launcher binary ][ payload: tar.zst ][ footer: 58 bytes ]
```

The footer is little-endian: `magic` (8 bytes, `PRTHOLE\0`), `version`
(2, currently 1), `payload_offset` (8), `payload_len` (8) and `sha256` (32,
of the compressed payload). The payload holds `app.toml` (with its paths
rewritten to point into the payload), `pack.toml` (the Porthole version
and `--devtools`), the optional icon, and `app/`.

## Serve a static site

A frontend-only app needs no backend at all. Use `[static]` instead of
`[sidecar]`:

```toml
[app]
name = "MyApp"
id = "com.example.myapp"

[static]
root = "web/"               # folder of HTML/CSS/JS/WASM, relative to app.toml
spa_fallback = true         # unknown extensionless paths serve index.html (client-side routers)
# csp = "default-src 'self'"   # replaces the default Content-Security-Policy
# in_memory_limit_mb = 64      # packed sites larger than this are served from the cache folder
```

Porthole answers the webview's requests itself through a custom protocol,
`porthole://localhost/` (`http://porthole.localhost/` on Windows), so there
is no port and nothing else on the machine can reach the files. `porthole
run app.toml` serves the folder from disk, which is handy while
developing; a packed app holds the whole site in memory. See
[`examples/hello-static`](examples/hello-static).

- `Content-Type` comes from the file extension, including
  `application/wasm` for streaming WebAssembly compilation.
- `/` and folders serve their `index.html`. Missing files get a 404 page.
- The default Content-Security-Policy only lets scripts load from the app
  itself (no remote or inline scripts), and allows remote images, styles,
  fonts and HTTPS API calls. Set `static.csp` to replace it.
- The origin is the same on every launch, so `localStorage`, IndexedDB and
  cookies persist.
- A static app can't also run a sidecar in v1; put the frontend in the
  sidecar instead.

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

The working directory is the sidecar's folder. The backend must enforce
the [auth handshake](#auth-handshake): the [auth kit](auth-kit) does it in
one line for Express and FastAPI, and
[`examples/hello-python`](examples/hello-python) shows it in about 30
lines of standard-library Python.

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

## Auth handshake

A sidecar listens on a TCP port, so anything on the machine could talk to
it: another local process, or a web page in the user's browser calling
`http://127.0.0.1:<port>`. The handshake makes sure only Porthole's window
gets in. Porthole generates the secret; your backend enforces it. This
section is the specification; the [auth kit](auth-kit) middlewares are
conveniences that follow it.

**What Porthole does.** It generates a token of 32 random bytes, passes it
to the backend as `APP_TOKEN` (64 lowercase hex characters) along with
`APP_PORT`, and loads `http://127.0.0.1:<APP_PORT>/?token=<APP_TOKEN>` in
the window. That is the only URL that ever carries the token. With
`auth.enabled = false` in `app.toml`, it sets no `APP_TOKEN` and loads
`http://127.0.0.1:<APP_PORT>/`.

**What the backend must do**, for every request, in this order:

1. **Listen on `127.0.0.1:<APP_PORT>` only**, never `0.0.0.0` or `::`.
2. **Check the `Host` header.** It must be `127.0.0.1:<port>` or
   `localhost:<port>` (hostname compared case-insensitively), where
   `<port>` is the port the backend listens on. Anything else, including a
   missing `Host`, gets **403**. This stops DNS rebinding, where an
   attacker's domain resolves to `127.0.0.1` and the browser treats your
   backend as part of the attacker's site.
3. **Handshake.** If the query string has a `token` parameter (URL-decoded)
   equal to `APP_TOKEN`, compared in constant time, answer **302** with
   - `Set-Cookie: app_session=<APP_TOKEN>; HttpOnly; SameSite=Strict; Path=/`
   - `Location:` the same path with the `token` parameter removed. Other
     query parameters should be kept as they were. The location must be a
     path on the same origin (`//host/...` would leave it).
   - `Cache-Control: no-store`
4. **Session.** Otherwise, if the request has an `app_session` cookie
   equal to `APP_TOKEN`, compared in constant time, serve it normally.
   A `token` parameter that doesn't match is not an error by itself: a
   request with a valid cookie is served, so the app can use `?token=` for
   its own purposes.
5. **Everything else gets 401**, with no cookie set. That covers every
   method and path, including the API, static files and WebSocket
   upgrades. A WebSocket upgrade needs the cookie: it can't follow the
   handshake redirect. There is no need to exempt a health check: the
   readiness probe (`ready_path`) treats 401 and 403 as "up".

The webview stores the cookie and sends it with every later request,
including `fetch()` and WebSocket connections from the page.
`SameSite=Strict` keeps a page from another site from sending it, and
`HttpOnly` keeps it out of reach of scripts. The cookie is not `Secure`,
because the connection is plain HTTP on loopback.

**Standalone mode.** When a developer runs the backend directly, without
Porthole, there is no `APP_TOKEN`. A backend may then skip steps 3 to 5,
but only when the developer opts in explicitly, such as with a
`--standalone` flag; a missing token must not silently turn auth off.
Keep the `Host` check. The auth kit raises an error at startup unless
standalone mode was asked for, and prints a warning when it is on. The
same applies under `auth.enabled = false`.

**Logging.** The first request's query string holds the token, so log
paths, not full URLs.

### What it protects against

| Threat | Stopped by |
| --- | --- |
| A website in the user's browser calls `http://127.0.0.1:<port>` | The random port, and the token cookie: the user's browser doesn't have it (it lives in the webview's own profile), and `SameSite=Strict` would keep it off cross-site requests anyway |
| DNS rebinding (an attacker's domain resolves to `127.0.0.1`) | The `Host` check |
| Another local process calls the API | The token, which it doesn't know |
| The token leaking through the URL | It is in the URL for the first request only, then swapped for an `HttpOnly` cookie and redirected away |
| Same-user malware reading `APP_TOKEN` from the process environment | Not covered. Same-user code can already read the app's files and memory |

### Testing your own backend

[`auth-kit/conformance.py`](auth-kit/conformance.py) starts a backend the
way Porthole does and runs 27 checks against it, covering every rule
above. It needs only Python 3:

```sh
python3 auth-kit/conformance.py -- node server.js
python3 auth-kit/conformance.py --path /index.html -- ./my-server
```

`--path` names a page the backend serves with a 2xx once authenticated
(default `/`). CI runs it against both middlewares and all three examples.

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
with it, and runs window smoke tests on Linux under Xvfb, including one
that checks a real webview completes the auth handshake. The sidecar tests
use Python, and skip themselves where it is not installed.

## Roadmap

1. **v0.1 — Window.** `run --url` opens a window on an existing localhost server.
2. **v0.2 — Sidecar.** `run app.toml` starts your backend with `APP_PORT`,
   `APP_TOKEN` and `APP_DATA_DIR`, waits for it, and stops it with the window.
3. **v0.3 — Pack.** `pack` and `inspect`: one executable with the backend
   (or a static site, served from memory) inside.
4. **v0.4 — Auth kit.** The [handshake spec](#auth-handshake), reference
   middlewares for Express and FastAPI/Starlette, and a conformance test.
5. **v0.5 — Release polish.** Title bar modes, downloads, signing,
   cross-packing, prebuilt launchers.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
