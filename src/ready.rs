//! Waiting for a server to come up.

use std::net::{TcpStream, ToSocketAddrs};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

/// How often to retry while waiting, as in the design's readiness wait.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Block until something accepts a TCP connection on `addr` (`host:port`),
/// or fail once `timeout` has passed.
///
/// A zero timeout still makes one attempt.
pub fn wait_for_tcp(addr: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now().checked_add(timeout);
    loop {
        if try_connect(addr) {
            return Ok(());
        }
        let now = Instant::now();
        if deadline.is_some_and(|d| now >= d) {
            bail!(
                "nothing is listening on {addr} (waited {:.1}s). \
                 Start your server first, or pass --wait <secs> to wait longer",
                timeout.as_secs_f64()
            );
        }
        let sleep = deadline.map_or(POLL_INTERVAL, |d| POLL_INTERVAL.min(d - now));
        thread::sleep(sleep);
    }
}

/// One connection attempt to every address `addr` resolves to (`localhost`
/// can mean both `::1` and `127.0.0.1`, and servers often bind only one).
fn try_connect(addr: &str) -> bool {
    let Ok(addrs) = addr.to_socket_addrs() else {
        return false;
    };
    addrs
        .into_iter()
        .any(|a| TcpStream::connect_timeout(&a, Duration::from_millis(250)).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn succeeds_when_listening() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        wait_for_tcp(&addr, Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn times_out_when_nothing_listens() {
        // Bind then drop to get a port that is very likely free.
        let addr = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .to_string();
        let start = Instant::now();
        let err = wait_for_tcp(&addr, Duration::from_millis(200)).unwrap_err();
        assert!(err.to_string().contains("nothing is listening"));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn waits_for_late_server() {
        let addr = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            let l = TcpListener::bind(addr).unwrap();
            let _ = l.accept();
        });
        wait_for_tcp(&addr.to_string(), Duration::from_secs(5)).unwrap();
        handle.join().unwrap();
    }
}
