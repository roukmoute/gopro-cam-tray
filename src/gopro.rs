//! GoPro discovery and webcam control over its local HTTP API (same as v1).

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

const HTTP_PORT: u16 = 8080;
const GOPRO_HOST_OCTET: u8 = 51;

/// Find the GoPro on the USB network interface it exposes when connected.
pub fn detect() -> Option<Ipv4Addr> {
    let ifaces = if_addrs::get_if_addrs().ok()?;
    for iface in ifaces {
        if let IpAddr::V4(v4) = iface.ip() {
            let o = v4.octets();
            if o[0] == 172 && (20..=29).contains(&o[1]) {
                let candidate = Ipv4Addr::new(o[0], o[1], o[2], GOPRO_HOST_OCTET);
                if http_get(candidate, "/gopro/webcam/version").is_ok() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

pub fn http_get(ip: Ipv4Addr, path: &str) -> Result<String, String> {
    let url = format!("http://{ip}:{HTTP_PORT}{path}");
    ureq::get(&url)
        .timeout(Duration::from_secs(4))
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())
}

/// Reset any stale session then start the webcam stream.
pub fn start(ip: Ipv4Addr) -> Result<(), String> {
    let _ = http_get(ip, "/gopro/webcam/exit");
    std::thread::sleep(Duration::from_millis(500));
    http_get(ip, "/gopro/webcam/start").map(|_| ())
}

pub fn stop(ip: Ipv4Addr) {
    let _ = http_get(ip, "/gopro/webcam/stop");
    let _ = http_get(ip, "/gopro/webcam/exit");
}
