//! GoPro discovery and webcam control over its local HTTP API (same as v1).
//!
//! The HTTP client is hand-rolled over TcpStream: the camera is always a plain
//! IPv4 literal on the local USB link (no TLS, DNS, redirects or proxies), and
//! callers only look at Ok/Err — pulling in a full HTTP library (and, through
//! its URL parser, the Unicode/IDNA tables) costs ~300 KB of binary for
//! nothing.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

const HTTP_PORT: u16 = 8080;
const GOPRO_HOST_OCTET: u8 = 51;
/// The control API answers with tiny JSON bodies; anything bigger is bogus.
const MAX_BODY: usize = 1024 * 1024;

/// Find the GoPro on the USB network interface it exposes when connected.
///
/// The GoPro USB net uses 172.2x.y.z with the camera at `.51`. We skip virtual
/// interfaces (Docker bridges, veth, ...) which on some machines occupy many
/// 172.x nets and would otherwise make us probe the wrong subnets, and use a
/// short timeout so scanning stays quick.
pub fn detect() -> Option<Ipv4Addr> {
    let ifaces = if_addrs::get_if_addrs().ok()?;
    for iface in ifaces {
        if iface.is_loopback() || is_virtual(&iface.name) {
            continue;
        }
        if let IpAddr::V4(v4) = iface.ip() {
            let o = v4.octets();
            if o[0] == 172 && (20..=29).contains(&o[1]) {
                let candidate = Ipv4Addr::new(o[0], o[1], o[2], GOPRO_HOST_OCTET);
                if get(candidate, "/gopro/webcam/version", 2).is_ok() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// True for virtual/container/bridge interfaces we should never probe.
fn is_virtual(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["br-", "veth", "docker", "virbr", "tap", "tun", "vmnet", "zt"]
        .iter()
        .any(|p| n.starts_with(p))
}

pub fn http_get(ip: Ipv4Addr, path: &str) -> Result<String, String> {
    get(ip, path, 4)
}

/// Plain HTTP/1.1 GET with `timeout_secs` as a total deadline (connect + write
/// + read). `Connection: close` so the body simply runs to EOF. Non-2xx
/// statuses are errors, like the HTTP library we replaced (callers rely on it).
fn get(ip: Ipv4Addr, path: &str, timeout_secs: u64) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let remaining = |what: &str| -> Result<Duration, String> {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| format!("timeout during {what}"))
    };

    let addr = SocketAddr::from((ip, HTTP_PORT));
    let mut stream =
        TcpStream::connect_timeout(&addr, remaining("connect")?).map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(remaining("send")?))
        .map_err(|e| e.to_string())?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: {ip}:{HTTP_PORT}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;

    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        stream
            .set_read_timeout(Some(remaining("response")?))
            .map_err(|e| e.to_string())?;
        match stream.read(&mut buf) {
            Ok(0) => break, // EOF: server honoured Connection: close
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > MAX_BODY {
                    return Err("response too large".into());
                }
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    parse_response(&raw)
}

/// Split a raw HTTP/1.x response into status + body; Err on non-2xx statuses.
fn parse_response(raw: &[u8]) -> Result<String, String> {
    let header_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("malformed HTTP response")?;
    let head = String::from_utf8_lossy(&raw[..header_end]);
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or("malformed HTTP status line")?;
    if !(200..300).contains(&status) {
        return Err(format!("HTTP status {status}"));
    }
    Ok(String::from_utf8_lossy(&raw[header_end + 4..]).into_owned())
}

/// Reset any stale session then start the webcam stream. Sending stop before
/// exit reliably clears a session left running by a previous, unclean shutdown.
pub fn start(ip: Ipv4Addr) -> Result<(), String> {
    let _ = http_get(ip, "/gopro/webcam/stop");
    let _ = http_get(ip, "/gopro/webcam/exit");
    std::thread::sleep(Duration::from_millis(600));
    http_get(ip, "/gopro/webcam/start").map(|_| ())
}

pub fn stop(ip: Ipv4Addr) {
    let _ = http_get(ip, "/gopro/webcam/stop");
    let _ = http_get(ip, "/gopro/webcam/exit");
}

#[cfg(test)]
mod tests {
    use super::parse_response;

    #[test]
    fn ok_response_returns_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"status\":2}";
        assert_eq!(parse_response(raw).unwrap(), "{\"status\":2}");
    }

    #[test]
    fn empty_body_is_ok() {
        let raw = b"HTTP/1.1 204 No Content\r\n\r\n";
        assert_eq!(parse_response(raw).unwrap(), "");
    }

    #[test]
    fn client_and_server_errors_are_err() {
        // The camera answers 500 when its webcam session is wedged; callers
        // must see that as Err (they only match Ok/Err).
        assert!(parse_response(b"HTTP/1.1 404 Not Found\r\n\r\nnope").is_err());
        assert!(parse_response(b"HTTP/1.1 500 Internal Server Error\r\n\r\n").is_err());
    }

    #[test]
    fn garbage_is_err() {
        assert!(parse_response(b"").is_err());
        assert!(parse_response(b"not http at all").is_err());
        assert!(parse_response(b"HTTP/1.1 abc\r\n\r\n").is_err());
    }
}
