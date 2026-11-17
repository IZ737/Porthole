//! Static mode: Porthole serves a folder of HTML/CSS/JS itself, through
//! the webview's custom protocol rather than a TCP port, so nothing else on
//! the machine can reach the files.
//!
//! The page loads from `porthole://localhost/` on macOS and Linux and from
//! `http://porthole.localhost/` on Windows (WebView2 maps custom schemes
//! to that form). Packed sites are served from memory; large ones, and
//! `porthole run app.toml` in development, from disk.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;

use percent_encoding::percent_decode_str;
use url::Url;
use wry::http::{Method, Request, Response, StatusCode, header};

use crate::logs;

/// The custom protocol's scheme.
pub const SCHEME: &str = "porthole";

/// Blocks scripts from anywhere but the app itself (including inline
/// scripts), plugins, and `<base>` hijacking. Remote images, styles, fonts
/// and API calls over HTTPS are allowed. `static.csp` replaces it.
pub const DEFAULT_CSP: &str = "default-src 'self' data: blob:; \
    script-src 'self' 'wasm-unsafe-eval'; \
    style-src 'self' 'unsafe-inline' https:; \
    img-src 'self' data: blob: https:; \
    font-src 'self' data: https:; \
    media-src 'self' data: blob: https:; \
    connect-src 'self' https: wss:; \
    object-src 'none'; base-uri 'self'";

const NOT_FOUND_PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><title>Not found</title>\
    <style>body{font:15px system-ui,sans-serif;margin:3em;color:#444}</style>\
    <h1>Not found</h1><p>This app has no page at this address.</p>";

/// The URL the window opens for a static site.
pub fn start_url() -> Url {
    let url = if cfg!(windows) {
        "http://porthole.localhost/"
    } else {
        "porthole://localhost/"
    };
    Url::parse(url).expect("valid URL")
}

/// Where the site's files come from.
pub enum Files {
    /// Unpacked from the payload. Leaked: the site lives as long as the
    /// process, and responses then borrow the bytes without copying.
    Memory(&'static HashMap<String, Vec<u8>>),
    /// A folder on disk: the extract-once cache, or the source folder in
    /// development.
    Disk(PathBuf),
}

impl Files {
    pub fn memory(files: HashMap<String, Vec<u8>>) -> Self {
        Files::Memory(Box::leak(Box::new(files)))
    }

    fn get(&self, rel: &str) -> Option<Cow<'static, [u8]>> {
        match self {
            Files::Memory(map) => map.get(rel).map(|b| Cow::Borrowed(b.as_slice())),
            Files::Disk(root) => {
                let path = if rel.is_empty() {
                    root.clone()
                } else {
                    root.join(rel)
                };
                if !path.is_file() {
                    return None;
                }
                match std::fs::read(&path) {
                    Ok(bytes) => Some(Cow::Owned(bytes)),
                    Err(err) => {
                        logs::warning!("could not read {}: {err}", path.display());
                        None
                    }
                }
            }
        }
    }
}

pub struct StaticSite {
    pub files: Files,
    pub spa_fallback: bool,
    pub csp: String,
}

impl StaticSite {
    /// Answer one request from the webview.
    pub fn handle(&self, request: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
        let method = request.method();
        let path = request.uri().path();
        let mut response = self.respond(method, path);
        if method == Method::HEAD {
            *response.body_mut() = Cow::Borrowed(&[]);
        }
        if response.status() == StatusCode::NOT_FOUND {
            logs::warning!("static: no file for {path}");
        }
        response
    }

    fn respond(&self, method: &Method, path: &str) -> Response<Cow<'static, [u8]>> {
        if method != Method::GET && method != Method::HEAD {
            return self.simple(
                StatusCode::METHOD_NOT_ALLOWED,
                "text/plain",
                b"method not allowed",
            );
        }
        let Some((rel, dir_like)) = clean_path(path) else {
            return self.not_found();
        };

        let index = join(&rel, "index.html");
        let hit = if dir_like {
            self.files.get(&index).map(|b| (index.clone(), b))
        } else {
            self.files
                .get(&rel)
                .map(|b| (rel.clone(), b))
                // A folder without the trailing slash.
                .or_else(|| self.files.get(&index).map(|b| (index.clone(), b)))
        };
        let hit = hit.or_else(|| {
            let last = rel.rsplit('/').next().unwrap_or_default();
            (self.spa_fallback && !last.contains('.'))
                .then(|| {
                    self.files
                        .get("index.html")
                        .map(|b| ("index.html".to_owned(), b))
                })
                .flatten()
        });
        match hit {
            Some((file, body)) => self.ok(&file, body),
            None => self.not_found(),
        }
    }

    fn ok(&self, file: &str, body: Cow<'static, [u8]>) -> Response<Cow<'static, [u8]>> {
        self.builder(StatusCode::OK, &content_type(file))
            .body(body)
            .expect("valid response")
    }

    fn not_found(&self) -> Response<Cow<'static, [u8]>> {
        self.simple(
            StatusCode::NOT_FOUND,
            "text/html; charset=utf-8",
            NOT_FOUND_PAGE.as_bytes(),
        )
    }

    fn simple(
        &self,
        status: StatusCode,
        content_type: &str,
        body: &'static [u8],
    ) -> Response<Cow<'static, [u8]>> {
        self.builder(status, content_type)
            .body(Cow::Borrowed(body))
            .expect("valid response")
    }

    fn builder(&self, status: StatusCode, content_type: &str) -> wry::http::response::Builder {
        Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONTENT_SECURITY_POLICY, &self.csp)
            .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
            // The origin stays the same across app versions, so make the
            // webview ask again rather than reuse an older version's file.
            .header(header::CACHE_CONTROL, "no-cache")
    }
}

/// Turn a request path into a relative file path with `/` separators, and
/// whether it named a folder (empty, or ending in `/`). `None` for paths
/// that try to leave the site or can't be a file name.
fn clean_path(path: &str) -> Option<(String, bool)> {
    let decoded = percent_decode_str(path).decode_utf8().ok()?;
    let dir_like = decoded.is_empty() || decoded.ends_with('/');
    let mut parts = Vec::new();
    for seg in decoded.split('/') {
        match seg {
            "" | "." => {}
            ".." => return None,
            // Backslashes and drive letters would let a segment act as
            // several on Windows; NUL is never valid.
            s if s.contains(['\\', ':', '\0']) => return None,
            s => parts.push(s),
        }
    }
    Some((parts.join("/"), dir_like || parts.is_empty()))
}

fn join(dir: &str, file: &str) -> String {
    if dir.is_empty() {
        file.to_owned()
    } else {
        format!("{dir}/{file}")
    }
}

/// `Content-Type` from the extension, with a UTF-8 charset for text.
fn content_type(file: &str) -> String {
    let mime = mime_guess::from_path(file).first_or_octet_stream();
    let essence = mime.essence_str();
    let texty = mime.type_() == mime_guess::mime::TEXT
        || matches!(
            essence,
            "application/javascript" | "application/json" | "image/svg+xml"
        );
    if texty {
        format!("{essence}; charset=utf-8")
    } else {
        essence.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(spa: bool) -> StaticSite {
        let files: HashMap<String, Vec<u8>> = [
            ("index.html", "home"),
            ("app.js", "js"),
            ("docs/index.html", "docs"),
            ("module.wasm", "wasm"),
            ("a b.txt", "space"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.as_bytes().to_vec()))
        .collect();
        StaticSite {
            files: Files::memory(files),
            spa_fallback: spa,
            csp: DEFAULT_CSP.to_owned(),
        }
    }

    fn get(site: &StaticSite, path: &str) -> (u16, String, String) {
        let request = Request::builder()
            .uri(format!("porthole://localhost{path}"))
            .body(Vec::new())
            .unwrap();
        let r = site.handle(&request);
        let ct = r.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .to_owned();
        (
            r.status().as_u16(),
            ct,
            String::from_utf8_lossy(r.body()).into_owned(),
        )
    }

    #[test]
    fn serves_files_and_indexes() {
        let s = site(false);
        assert_eq!(
            get(&s, "/"),
            (200, "text/html; charset=utf-8".into(), "home".into())
        );
        assert_eq!(get(&s, "/index.html").2, "home");
        assert_eq!(get(&s, "/docs/").2, "docs");
        assert_eq!(get(&s, "/docs").2, "docs");
        assert_eq!(get(&s, "/a%20b.txt").2, "space");
        assert_eq!(get(&s, "/app.js?v=1").2, "js");
        assert_eq!(get(&s, "/module.wasm").1, "application/wasm");
        assert!(get(&s, "/app.js").1.contains("javascript"));
    }

    #[test]
    fn missing_files_are_404() {
        let s = site(false);
        for path in [
            "/nope",
            "/nope.js",
            "/docs/x",
            "/../index.html",
            "/%2e%2e/index.html",
            "/a\\b",
            "/c:x",
        ] {
            let (status, _, body) = get(&s, path);
            assert_eq!(status, 404, "{path}");
            assert!(body.contains("Not found"), "{path}");
        }
    }

    #[test]
    fn spa_fallback_only_for_extensionless_paths() {
        let s = site(true);
        assert_eq!(
            get(&s, "/settings/profile"),
            (200, "text/html; charset=utf-8".into(), "home".into())
        );
        assert_eq!(get(&s, "/missing.js").0, 404);
        assert_eq!(get(&s, "/docs").2, "docs");
    }

    #[test]
    fn sets_security_headers_and_rejects_writes() {
        let s = site(false);
        let request = Request::builder()
            .uri("porthole://localhost/")
            .body(Vec::new())
            .unwrap();
        let r = s.handle(&request);
        assert_eq!(r.headers()[header::CONTENT_SECURITY_POLICY], DEFAULT_CSP);
        assert_eq!(r.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");

        let post = Request::builder()
            .method(Method::POST)
            .uri("porthole://localhost/")
            .body(Vec::new())
            .unwrap();
        assert_eq!(s.handle(&post).status(), StatusCode::METHOD_NOT_ALLOWED);

        let head = Request::builder()
            .method(Method::HEAD)
            .uri("porthole://localhost/app.js")
            .body(Vec::new())
            .unwrap();
        let r = s.handle(&head);
        assert_eq!(r.status(), StatusCode::OK);
        assert!(r.body().is_empty());
    }

    #[test]
    fn serves_from_disk() {
        let dir = std::env::temp_dir().join(format!("porthole-static-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("index.html"), "root").unwrap();
        std::fs::write(dir.join("sub/page.css"), "css").unwrap();
        let s = StaticSite {
            files: Files::Disk(dir.clone()),
            spa_fallback: false,
            csp: DEFAULT_CSP.to_owned(),
        };
        assert_eq!(get(&s, "/").2, "root");
        assert_eq!(
            get(&s, "/sub/page.css"),
            (200, "text/css; charset=utf-8".into(), "css".into())
        );
        assert_eq!(get(&s, "/sub").0, 404);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
