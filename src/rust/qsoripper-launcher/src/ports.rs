//! TCP probe + a small `is_port_listening` helper used during readiness checks.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// Try to connect to `127.0.0.1:port` with the given timeout. Returns `true`
/// when something is accepting connections.
pub(crate) fn is_port_listening(port: u16, timeout: Duration) -> bool {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    TcpStream::connect_timeout(&addr, timeout).is_ok()
}

/// Poll `is_port_listening` until it succeeds or `total` elapses.
pub(crate) fn wait_for_port(port: u16, total: Duration, per_attempt: Duration) -> bool {
    let deadline = Instant::now() + total;
    loop {
        if is_port_listening(port, per_attempt) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::needless_pass_by_value
)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn detects_a_listener_on_localhost() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().unwrap().port();
        assert!(is_port_listening(port, Duration::from_millis(250)));
    }

    #[test]
    fn reports_false_on_unbound_port() {
        // A high port that nothing is realistically bound to. If this is
        // flaky in some environment, swap to spawning a listener, recording
        // the port, dropping the listener, and asserting closed.
        assert!(!is_port_listening(1, Duration::from_millis(50)));
    }
}
