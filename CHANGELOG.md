# Changelog

Porthole follows [semantic versioning](https://semver.org) from 1.0. The
README's [Stability](README.md#stability) section says what that covers.

## 1.0.0

The packed format is frozen: every 1.x launcher runs every app packed by
1.x. [FORMAT.md](FORMAT.md) specifies it.

- **Explorer icon.** A packed Windows `.exe` shows `app.icon` as its file
  icon in Explorer, the Start menu and shortcuts. `pack` writes 16 to 256
  px icon resources into the exe, keeping the launcher's own resources,
  and it works the same when cross-packing from macOS or Linux. `inspect`
  lists the icon sizes.
- **Range requests in static mode.** Static sites answer `Range` requests
  with `206 Partial Content`, so `<video>` and `<audio>` can seek.
- **Format freeze.** `pack.toml` readers ignore keys they don't know, so
  later 1.x releases can record more. A byte-exact footer test and a
  committed v1 packed file guard the format.
- The auth kit's Node and Python packages are now 1.0.0; the handshake is
  unchanged.

## 0.5.0

Title bar modes (`native`, `custom`, `overlay`) and the window-control API,
`bar_color`, window icons, native "Save as" for downloads, links to other
sites opening in the browser, the WebView2 check, a stable sidecar port,
GUI-subsystem packed Windows exes, Authenticode-signed exes, `pack
--macos-app`, cross-packing with `pack --target` from release launchers,
the release workflow, a Go example and a startup budget in CI.

## 0.4.0

The auth handshake specification, Express and FastAPI/Starlette
middlewares, and a conformance test for any backend.

## 0.3.0

`pack` and `inspect`, the payload footer, the extract-once cache, and
static mode served from memory through the `porthole://` protocol.

## 0.2.0

`run app.toml` starts the backend as a sidecar with `APP_PORT`,
`APP_TOKEN` and `APP_DATA_DIR`, waits for it, and stops it with the
window. Single instance, window state, logs and `--console`.

## 0.1.0

`run --url` opens a webview window on a running localhost server.
