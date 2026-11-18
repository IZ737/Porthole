# Porthole

Porthole turns any "backend + localhost web UI" app into a desktop app. It
opens the app's local URL in the operating system's own webview (WebView2,
WKWebView or WebKitGTK), so the launcher stays around 1–3 MB instead of
shipping a browser.

> **Status: v1.0.** Porthole can start your backend as a sidecar, serve a
> static site itself, or open a window on a server you have already
> started, and `pack` turns any of the first two into a single executable,
> for Windows, macOS or Linux from any of them. Apps can draw their own
> title bar. The [auth kit](auth-kit) protects a sidecar in one line for
> Express and FastAPI, and the examples cover Python, Node and Go. The
> packed format is [frozen for 1.x](FORMAT.md); see
> [Stability](#stability) for what else 1.x promises.

## Install

macOS and Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/IZ737/Porthole/main/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/IZ737/Porthole/main/install.ps1 | iex
```

Both scripts download the `porthole` binary for your platform from the
latest [GitHub release](https://github.com/IZ737/Porthole/releases), check
it against the release's `SHA256SUMS`, and install it without admin
rights: to `~/.local/bin` on macOS and Linux (the script tells you if that
is not on your `PATH`), or to `%LOCALAPPDATA%\Programs\porthole` on
Windows, which it adds to your user `PATH`. Run the same line again to
update. Releases have builds for Windows x64 (which also runs on Windows 11
on Arm), macOS on Apple silicon and Intel, and Linux x64; on Linux you also
need the [WebKitGTK runtime](#building), and the script says so if it is
missing.

The scripts read a few optional environment variables:

| Variable | Meaning |
| --- | --- |
| `PORTHOLE_VERSION` | Install this release, such as `1.0.0`, instead of the latest |
| `PORTHOLE_INSTALL_DIR` | Install to this folder instead |
| `PORTHOLE_BASE_URL` | Download from a mirror of the releases page (same layout) |
| `PORTHOLE_NO_MODIFY_PATH` | Windows: set to `1` to leave your `PATH` alone |

```sh
curl -fsSL https://raw.githubusercontent.com/IZ737/Porthole/main/install.sh | PORTHOLE_VERSION=1.0.0 sh
```

```powershell
$env:PORTHOLE_VERSION = '1.0.0'; irm https://raw.githubusercontent.com/IZ737/Porthole/main/install.ps1 | iex
```

To uninstall, delete the binary (and, on Windows, its folder from your
user `PATH`). To build from source instead, see [Building](#building), or
run `cargo install --git https://github.com/IZ737/Porthole porthole-cli`.

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
| `-o`, `--output <FILE>` | Where to write the executable. Default: `<app.name>` in the current folder (`.exe` for Windows, `.app` with `--macos-app`) |
| `--devtools` | Allow the web inspector in the packed app (off by default) |
| `--target <TRIPLE>` | Pack for another platform (see [Cross-packing](#cross-packing)) |
| `--launcher <FILE>` | Pack onto this `porthole` binary instead of the running one |
| `--macos-app` | Write a macOS `.app` bundle, which can be signed and notarized (see [Signing](#signing)) |

`pack` bundles `sidecar.path` (one file or a whole folder, for runtimes
such as Python, Node or .NET) or `static.root`, plus `app.icon` if set.
Unix permission bits and symlinks are kept, so executables still run after
unpacking. On Windows, the packed exe is a GUI program, so it opens no
console window (the `porthole.exe` CLI itself stays a console program);
`--console` attaches one when you want to see the logs. Sign *after*
packing.

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

### Cross-packing

`pack` copies the `porthole` binary it runs as, so by default it packs for
its own platform. To pack for another one, name it:

```sh
porthole pack app.toml --target x86_64-pc-windows-msvc       # MyApp.exe
porthole pack app.toml --target aarch64-apple-darwin --macos-app
```

| Target | Platform |
| --- | --- |
| `x86_64-pc-windows-msvc` | Windows x64 |
| `x86_64-apple-darwin` | macOS on Intel |
| `aarch64-apple-darwin` | macOS on Apple silicon |
| `x86_64-unknown-linux-gnu` | Linux x64 |

The launcher for that target comes from the GitHub release of the same
Porthole version (`porthole-<version>-<target>[.exe]`), is checked against
the release's `SHA256SUMS`, and is cached in
`<cache dir>/porthole/launchers/`, so later packs work offline. Downloads
use `curl`, which Windows 10+, macOS and most Linux systems include.
`--launcher <FILE>` packs onto a `porthole` binary you already have (one
you built yourself, or for a platform without a release), and `pack`
refuses a launcher that doesn't match `--target`. Set
`PORTHOLE_RELEASES_URL` to download from a mirror laid out like
`<url>/v<version>/<asset>`.

A static site packs for any platform as is. A sidecar must itself be built
for the target platform: `pack` bundles whatever `sidecar.path` holds, so
cross-pack a Go or .NET backend you cross-compiled, not a Linux virtualenv
for Windows.

### Signing

Signatures cover the whole file, so the order is always pack, then sign,
then (on macOS) notarize. Signing `porthole` before packing achieves
nothing: `pack` drops a launcher's signature, since it would not cover the
packed file anyway.

**Windows.** Sign the packed exe with Authenticode as usual. Signing
appends a certificate table after Porthole's footer; the launcher reads
the PE header to find where the signed data ends and looks for its footer
there. Unsigned self-extracting exes are what antivirus heuristics distrust
most, so sign anything you distribute.

```powershell
signtool sign /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 /f cert.pfx /p $env:CERT_PASSWORD MyApp.exe
porthole inspect MyApp.exe        # shows the signature and checks the payload
```

**macOS.** `codesign` rejects a Mach-O file with data appended, so a plain
packed file can't be signed. Pack a `.app` bundle instead: the launcher
goes in `Contents/MacOS` untouched, and the payload in
`Contents/Resources/payload.tar.zst`. `app.icon` (a square PNG of 16 to
1024 pixels) becomes the Dock icon, and `app.version` the bundle version.
Then sign with a Developer ID, notarize, and staple:

```sh
porthole pack app.toml --macos-app -o MyApp.app
codesign --force --deep --options runtime --timestamp --sign "Developer ID Application: You (TEAMID)" MyApp.app
ditto -c -k --keepParent MyApp.app MyApp.zip
xcrun notarytool submit MyApp.zip --apple-id you@example.com --team-id TEAMID --password "$APP_PASSWORD" --wait
xcrun stapler staple MyApp.app
```

A sidecar that is itself a native program (a Go or .NET binary) is
unpacked to the cache folder at launch and is not covered by the bundle's
signature; with the hardened runtime, sign it before packing too
(`codesign --options runtime --sign ... bin/server`). Interpreters such as
`python3` from the user's system are not affected. For a quick local test
without a Developer ID, sign ad hoc: `codesign --force --deep --sign -
MyApp.app`.

**Linux** has no platform-wide signing; publish a checksum next to the
download.

### Packed file format

```
[ launcher binary ][ payload: tar.zst ][ footer: 58 bytes ]
```

[FORMAT.md](FORMAT.md) is the full specification, frozen for every 1.x
release. In short: the footer is little-endian `magic` (8 bytes,
`PRTHOLE\0`), `version` (2, always 1 in 1.x), `payload_offset` (8),
`payload_len` (8) and `sha256` (32, of the compressed payload). The payload holds `app.toml` (with its paths
rewritten to point into the payload), `pack.toml` (the Porthole version
and `--devtools`), the optional icon, and `app/`.

A signed Windows exe has the Authenticode certificate table after the
footer (plus up to 7 bytes of zero padding before it). A `--macos-app`
bundle keeps `[ payload ][ footer ]` in `Contents/Resources/payload.tar.zst`,
with `payload_offset` 0; a launcher in `Contents/MacOS` with no footer of
its own reads that file.

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
- Byte-range requests are answered (`206 Partial Content`), so `<video>`
  and `<audio>` can seek.
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
icon = "icon.png"           # optional: window and taskbar icon (PNG), Dock icon of a .app
version = "1.2.0"           # optional: the .app bundle's version

[window]
title = "My App"            # default: app.name
width = 1200
height = 800
resizable = true
title_bar = "native"        # native | custom | overlay (see "Title bar")
theme = "system"            # light | dark | system
bar_color = "#1e1e2e"       # optional: native title bar colour (Windows 11, macOS)
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
lines of standard-library Python, [`examples/hello-go`](examples/hello-go)
in Go's standard library.

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
- **The port stays the same across launches.** The first launch picks a
  random free port and remembers it in `APP_DATA_DIR/porthole-port`; later
  launches reuse it. The page's origin (`http://127.0.0.1:<port>`) is then
  stable, so `localStorage`, IndexedDB and other per-origin storage
  survive a restart. If that port is taken, this launch uses another one
  (and the page starts with empty storage) but keeps the remembered port
  for next time. The token is still new on every launch.

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
| `porthole-port` | The sidecar's port, reused on the next launch |
| `webview/` | The webview's cookies, storage and caches |
| `porthole.lock`, `porthole-instance` | Single-instance lock and hand-over |

Logs rotate at 5 MB, keeping the last 3 (`sidecar.log.1` to `.3`). A saved
position that is no longer on any screen falls back to the centre of the
primary display.

Set `PORTHOLE_NO_DIALOGS=1` to skip error dialogs on headless machines
such as CI; errors still go to stderr and the log.

## Title bar

Set `title_bar` in `[window]`; it works in sidecar and static apps alike.

| Mode | What you get |
| --- | --- |
| `native` (default) | The OS title bar. `bar_color` sets its colour on Windows 11 and macOS; `theme` picks light or dark |
| `custom` | No OS title bar: your HTML draws the whole bar, buttons included |
| `overlay` | macOS: the page runs under a transparent title bar that keeps the traffic-light buttons. Same as `custom` on Windows and Linux |

With `custom` or `overlay`, Porthole injects a small window-control API
before your page's scripts run:

```js
window.porthole.window.minimize()
window.porthole.window.toggleMaximize()
window.porthole.window.close()
window.porthole.window.isMaximized()                 // Promise<boolean>
window.porthole.window.on('maximized-change', cb)    // returns an unsubscribe function
window.porthole.window.titleBar                      // "custom" or "overlay" (macOS only)
```

Any element with a `data-porthole-drag` attribute moves the window when
dragged, and maximises it on double-click, like an OS title bar. Buttons,
links, inputs and anything marked `data-porthole-no-drag` inside it keep
their clicks. While the window is maximised, `<html>` has a
`data-porthole-maximized` attribute, handy for swapping the maximise icon.
[`examples/hello-titlebar`](examples/hello-titlebar) has a starter bar
(`titlebar.css` and `titlebar.js`) to copy.

This is a fixed set of window messages, not a bridge between your page and
native code: your backend's HTTP API stays the only interface, and the
launcher ignores messages from any origin other than the app's own.

- Frameless windows stay resizable from their edges: natively on Windows,
  and through the page's outer 5 pixels on Linux (where the webview covers
  the window's edges).
- On Windows 11, a custom bar loses the snap-layout flyout of the maximise
  button; Win+Z and dragging to the screen edges still work.
- `bar_color` has no effect on Linux, where the desktop theme draws the bar.

## Links, downloads and icons

- **Links to other sites** open in the default browser, whether they are
  plain links, `target="_blank"` or `window.open`; the window stays on the
  app. Same-origin links load in the window.
- **Downloads** (a link with `download`, or a response with
  `Content-Disposition: attachment`) show the native "Save as" dialog,
  starting in the user's Downloads folder.
- **`<input type="file">`** uses each webview's native file picker.
- **`app.icon`** (a PNG up to 1024 × 1024) is the window and taskbar icon
  on Windows and Linux, the Dock icon of a `--macos-app` bundle, and the
  icon Windows Explorer shows for a packed `.exe` (`pack` writes it into
  the exe's resources, also when cross-packing from macOS or Linux). Use a
  square PNG of at least 256 × 256 for sharp icons at every size.
- **WebView2** ships with Windows 11 but is missing on some Windows 10
  machines. Porthole checks for it at launch and, if it is missing, offers
  to open Microsoft's download page.

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

`--url` also accepts `localhost:5173` or just `5173`.

### Develop with hot reload

For frontend work, run your dev server as usual and open a Porthole window
on it, so you see the real webview (WebKit on macOS and Linux, Chromium on
Windows) with hot reload:

```sh
npx vite --port 5173 --strictPort      # in one terminal
porthole run --url 5173 --devtools      # in another
```

For a sidecar app, run the backend on its own in standalone mode (the
examples take `--standalone`, so no token is needed) and let the dev
server proxy API calls to it:

```js
// vite.config.js
export default {
  server: { proxy: { '/api': 'http://127.0.0.1:8000' } },
};
```

```sh
APP_PORT=8000 python3 server.py --standalone   # or: node server.js --standalone
```

When the frontend is ready, build it into the folder your backend serves
(or `static.root`), and check the real thing with `porthole run app.toml`
before packing. The window-control API for custom title bars only exists
in app windows (`run app.toml` and packed apps), not with `--url`.

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
  same window when they stay on the server's origin, and open in your
  default browser when they go elsewhere. (Plain links are left alone
  here, so a dev server can redirect or link wherever it likes; app windows
  send those to the browser too.)
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

On Linux, users need the WebKitGTK 4.1 runtime, which most desktop
distributions install with GNOME. If not: `sudo apt install
libwebkit2gtk-4.1-0` (Debian 12+, Ubuntu 22.04+), `sudo dnf install
webkit2gtk4.1` (Fedora 37+), or `sudo pacman -S webkit2gtk-4.1` (Arch).
The released Linux launcher is built on Ubuntu 22.04, so it runs on
distributions with glibc 2.35 or newer. CI tests on Ubuntu 22.04 (release
build) and 24.04.

CI builds and tests on Ubuntu 24.04, macOS (arm64) and Windows. On Linux it
runs real windows under Xvfb (on a private D-Bus session bus) and waits for
each page to load: `run --url`, sidecar and static apps, packed apps, the
custom title bar's window controls, and the auth handshake in a real
webview for the Express, FastAPI and Go examples. It also checks the
startup budget (window visible within 300 ms of launch on a cache hit, not
counting the backend's own start-up), cross-packs for Windows and macOS
from Linux, then runs the Windows exe after Authenticode-signing it and the
macOS bundle after codesigning it.

### Releases

Pushing a tag such as `v1.0.0` (matching `version` in `Cargo.toml`) runs
the release workflow: it builds the launchers for the four targets in
[Cross-packing](#cross-packing), and publishes them on a GitHub release with
`SHA256SUMS`. That is where `pack --target` and the [install
scripts](#install) download from.

## Roadmap

1. **v0.1 — Window.** `run --url` opens a window on an existing localhost server.
2. **v0.2 — Sidecar.** `run app.toml` starts your backend with `APP_PORT`,
   `APP_TOKEN` and `APP_DATA_DIR`, waits for it, and stops it with the window.
3. **v0.3 — Pack.** `pack` and `inspect`: one executable with the backend
   (or a static site, served from memory) inside.
4. **v0.4 — Auth kit.** The [handshake spec](#auth-handshake), reference
   middlewares for Express and FastAPI/Starlette, and a conformance test.
5. **v0.5 — Release polish.** Title bar modes and window controls, window
   icons, downloads, a stable sidecar port, Windows apps without a
   console, signing support, `.app` bundles, cross-packing from prebuilt
   release launchers, a Go example, and a startup budget in CI.
6. **v1.0 — Stable.** The [packed format](FORMAT.md) is frozen, the
   `.exe` file shows the app's icon in Explorer, and static sites answer
   range requests.

After 1.0: static + sidecar proxy mode, file associations and deep links,
tray icon, auto-update, multiple windows.

## Stability

From 1.0, Porthole follows semantic versioning. Within 1.x:

- **Packed apps** keep running: any 1.x launcher reads any app packed by
  1.x ([FORMAT.md](FORMAT.md)).
- **`app.toml`** only gains optional keys; existing keys keep their
  meaning. Unknown keys stay errors, so typos fail loudly.
- **The CLI** (`run`, `pack`, `inspect` and their flags) and a packed
  app's own flags (`--console`, `--verify`) keep working as documented.
- **What your backend gets** (`APP_PORT`, `APP_TOKEN`, `APP_DATA_DIR`), the
  [auth handshake](#auth-handshake) and the window-control API
  (`window.porthole.window`) don't change incompatibly.

Log file names and layout, cache folder paths, `inspect`'s output and the
exact wording of messages are not covered and may change.

Known limitations in 1.0: one window per app; a static app can't also run
a sidecar; on Windows 11 a custom title bar has no snap-layout flyout (see
[Title bar](#title-bar)); links and `window.open` to other sites go to the
browser, but a script that sets `location` to another site is not stopped.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
