//! The dashboard on this network, for a phone: `drmanhattan lan on`.
//!
//! Beside the loopback server, a second one then listens on this machine's network address.
//! It answers only requests that carry this machine's key: once in the link (`/?key=…`),
//! which trades it for a cookie and drops it from the address bar, then in that cookie. The
//! cookie is `HttpOnly` and `SameSite=Strict`, so no other site can make a phone send it; a
//! write also needs the page's own origin and the custom header, as on this machine. The
//! loopback server is unchanged and needs no key.

use std::io::Read;
use std::net::{IpAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use tiny_http::{Request, Server};

use super::{http, Ctx};
use crate::error::{invalid, Result};
use crate::fsutil;
use crate::log;
use crate::paths::Paths;
use crate::settings::load_config;

const COOKIE: &str = "drmanhattan_key";
/// Threads for the network listener: a phone or two.
const THREADS: usize = 2;
const CHECK_EVERY: Duration = Duration::from_secs(3);

/// This machine's key, made the first time it is asked for.
pub(crate) fn key(paths: &Paths) -> Result<String> {
    let path = paths.lan_key();
    if let Some(key) = std::fs::read_to_string(&path).ok().map(|k| k.trim().to_string()).filter(|k| k.len() >= 32) {
        return Ok(key);
    }
    let mut bytes = [0u8; 24];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .or_else(|e| invalid(format!("could not make a key: {e}")))?;
    let key: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    fsutil::ensure_private_dir(&paths.data)?;
    fsutil::write_atomic(&path, &format!("{key}\n"))?;
    Ok(key)
}

/// A new key: links and phones that had the old one need the new link.
pub(crate) fn new_key(paths: &Paths) -> Result<String> {
    fsutil::remove_if_present(&paths.lan_key())?;
    key(paths)
}

/// This machine's address on the network: the one it would send from to a private address.
/// Nothing is sent.
pub(crate) fn address() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.168.255.255:9").ok()?;
    Some(socket.local_addr().ok()?.ip()).filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
}

/// The link to open on a phone.
pub(crate) fn link(port: u16, key: &str) -> Option<String> {
    Some(format!("http://{}/?key={key}", http::host_port(&address()?.to_string(), port)))
}

/// Serves the dashboard on this machine's network address too while `config.json` says
/// `lan`, checked every few seconds: turned on or off without a restart, and moved when the
/// address changes (another Wi-Fi, say).
pub(super) fn watch(ctx: Arc<Ctx>) {
    let _ = std::thread::Builder::new().name("lan".into()).spawn(move || {
        let mut current: Option<(IpAddr, Arc<Server>)> = None;
        let mut failed: Option<IpAddr> = None;
        loop {
            let want = load_config(&ctx.paths).is_ok_and(|c| c.lan).then(address).flatten();
            if want != current.as_ref().map(|(ip, _)| *ip) {
                if let Some((_, server)) = current.take() {
                    for _ in 0..THREADS {
                        server.unblock();
                    }
                    log::note("the dashboard is no longer served on the network");
                }
                if let Some(ip) = want {
                    let addr = http::host_port(&ip.to_string(), ctx.port);
                    match Server::http(&addr) {
                        Ok(server) => {
                            let server = Arc::new(server);
                            if super::serve(server.clone(), &ctx, THREADS, true).is_ok() {
                                if let Ok(key) = key(&ctx.paths) {
                                    log::note(format_args!("dashboard on this network: http://{addr}/?key={key}"));
                                }
                                current = Some((ip, server));
                                failed = None;
                            }
                        }
                        Err(e) if failed != Some(ip) => {
                            log::warn(format_args!("could not serve the dashboard on {addr}: {e}"));
                            failed = Some(ip);
                        }
                        Err(_) => {}
                    }
                }
            }
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

pub(super) enum Access {
    Allowed,
    /// The key came in the link: answer with the cookie and the address without it.
    TradeForCookie,
    Denied,
}

pub(super) fn access(req: &Request, key: &str) -> Access {
    if cookie(req).is_some_and(|c| same(&c, key)) {
        return Access::Allowed;
    }
    match http::query_param(req.url(), "key") {
        Some(k) if same(&k, key) => Access::TradeForCookie,
        _ => Access::Denied,
    }
}

pub(super) fn cookie_header(key: &str) -> String {
    format!("{COOKIE}={key}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000")
}

/// Whether a write from another machine may change anything: the key, from the page itself.
pub(super) fn write_allowed(req: &Request, key: &str) -> bool {
    let host = http::header(req, "Host");
    let origin_ok =
        http::header(req, "Origin").map_or(true, |o| host.as_ref().is_some_and(|h| o == format!("http://{h}")));
    cookie(req).is_some_and(|c| same(&c, key))
        && origin_ok
        && http::header(req, "X-Drmanhattan").as_deref() == Some("1")
        && http::header(req, "Content-Type").is_some_and(|c| c.starts_with("application/json"))
}

fn cookie(req: &Request) -> Option<String> {
    let all = http::header(req, "Cookie")?;
    all.split(';').find_map(|c| c.trim().strip_prefix(COOKIE)?.strip_prefix('=').map(String::from))
}

/// Compares without stopping at the first difference, so timing says nothing of the key.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_compare_whole() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd") && !same("abc", "ab") && !same("", "a"));
    }
}
