//! Turning what the user typed into the URL the window loads.

use anyhow::{Context, Result, bail};
use url::{Host, Url};

/// Parse a `--url` value.
///
/// Accepts a full URL, a `host:port` without a scheme (`http://` is
/// assumed), or a bare port number (meaning `http://localhost:<port>`).
pub fn parse_url(input: &str) -> Result<Url> {
    let input = input.trim();
    let candidate = if !input.is_empty() && input.bytes().all(|b| b.is_ascii_digit()) {
        format!("http://localhost:{input}")
    } else if input.contains("://") {
        input.to_owned()
    } else {
        format!("http://{input}")
    };
    let url = Url::parse(&candidate).with_context(|| format!("invalid URL `{input}`"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => bail!("unsupported URL scheme `{other}`: use http or https"),
    }
    if url.host().is_none() {
        bail!("URL `{input}` has no host");
    }
    Ok(url)
}

/// Enforce the loopback-only default: Porthole is for apps served from this
/// machine, so remote hosts need an explicit `--allow-remote`.
pub fn check_allowed(url: &Url, allow_remote: bool) -> Result<()> {
    if allow_remote || is_loopback(url) {
        return Ok(());
    }
    bail!(
        "`{url}` is not on this machine. Porthole only opens localhost URLs \
         by default; pass --allow-remote if you really mean it"
    )
}

/// True for `localhost`, `*.localhost`, `127.0.0.0/8` and `::1`.
pub fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost")
        }
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// `host:port` for a TCP readiness probe, filling in the scheme's default port.
pub fn socket_addr_string(url: &Url) -> Result<String> {
    let host = url.host_str().context("URL has no host")?;
    let port = url
        .port_or_known_default()
        .context("URL has no port and no default port")?;
    // `*.localhost` names may not resolve through the system resolver, but
    // they always mean this machine.
    let host = match url.host() {
        Some(Host::Domain(_)) if is_loopback(url) => "localhost",
        _ => host,
    };
    Ok(format!("{host}:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(s: &str) -> String {
        parse_url(s).unwrap().to_string()
    }

    #[test]
    fn accepts_shorthands() {
        assert_eq!(parsed("3000"), "http://localhost:3000/");
        assert_eq!(parsed("localhost:5173"), "http://localhost:5173/");
        assert_eq!(parsed("127.0.0.1:8080/app"), "http://127.0.0.1:8080/app");
        assert_eq!(parsed(" http://[::1]:9000/ "), "http://[::1]:9000/");
        assert_eq!(parsed("https://localhost"), "https://localhost/");
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_url("").is_err());
        assert!(parse_url("file:///etc/passwd").is_err());
        assert!(parse_url("ftp://localhost").is_err());
        assert!(parse_url("http://").is_err());
    }

    #[test]
    fn loopback_policy() {
        for ok in [
            "http://localhost:1",
            "http://LOCALHOST:1",
            "http://app.localhost:1",
            "http://127.0.0.1:1",
            "http://127.5.6.7:1",
            "http://[::1]:1",
        ] {
            let url = parse_url(ok).unwrap();
            assert!(check_allowed(&url, false).is_ok(), "{ok} should be allowed");
        }
        for remote in [
            "http://example.com",
            "http://192.168.1.10:3000",
            "http://localhost.example.com",
            "http://[::2]:1",
        ] {
            let url = parse_url(remote).unwrap();
            assert!(
                check_allowed(&url, false).is_err(),
                "{remote} should be refused"
            );
            assert!(check_allowed(&url, true).is_ok());
        }
    }

    #[test]
    fn socket_addr_fills_defaults() {
        let addr = |s| socket_addr_string(&parse_url(s).unwrap()).unwrap();
        assert_eq!(addr("http://localhost"), "localhost:80");
        assert_eq!(addr("https://127.0.0.1"), "127.0.0.1:443");
        assert_eq!(addr("http://[::1]:3000"), "[::1]:3000");
        assert_eq!(addr("http://app.localhost:3000"), "localhost:3000");
    }
}
