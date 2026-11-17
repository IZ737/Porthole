# Porthole

Porthole turns any "backend + localhost web UI" app into a desktop app. It
opens the app's local URL in the operating system's own webview (WebView2,
WKWebView or WebKitGTK), so the launcher stays around 1–3 MB instead of
shipping a browser.

> **Status: v0.1.** Porthole can open a window on a server you have already
> started. Running your backend as a sidecar (v0.2) and packing everything
> into a single executable (v0.3) are next. See the [roadmap](#roadmap).

## Usage

Start your server, then point Porthole at it:

```sh
npm run dev                                   # e.g. Vite on http://localhost:5173
porthole run --url http://localhost:5173
```

`--url` also accepts `localhost:5173` or just `5173`.

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

CI builds and tests on Ubuntu 24.04, macOS (arm64) and Windows, and runs a
window smoke test on Linux under Xvfb.

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
