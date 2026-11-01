//! HTTP plumbing: responses with safe headers, the request guards, query strings.
//!
//! Defences, because a loopback port is reachable from every page open in the browser:
//!
//! - every request must name a loopback host when the server is bound to loopback, which
//!   defeats DNS rebinding (a hostile page whose DNS name resolves to 127.0.0.1 would
//!   otherwise read the logs);
//! - every response forbids framing, so the page cannot be clickjacked;
//! - a write must also come from this page's origin (or none), and carry a custom header
//!   and a JSON content type, which no cross-site form can send without a CORS preflight
//!   this server never approves.

use tiny_http::{Header, Request, Response};

const LOOPBACK: [&str; 3] = ["127.0.0.1", "localhost", "::1"];

/// `host:port` for a URL or a bind address: IPv6 literals need brackets.
pub(crate) fn host_port(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

pub(crate) fn is_loopback(bind: &str) -> bool {
    LOOPBACK.contains(&bind)
}

pub(super) fn respond(req: Request, code: u16, body: impl Into<Vec<u8>>, content_type: &str) {
    let mut response = Response::from_data(body.into()).with_status_code(code);
    let headers = [
        ("Content-Type", content_type),
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("X-Frame-Options", "DENY"),
        ("Content-Security-Policy", "frame-ancestors 'none'"),
    ];
    for (name, value) in headers {
        if let Ok(h) = Header::from_bytes(name.as_bytes(), value.as_bytes()) {
            response.add_header(h);
        }
    }
    let _ = req.respond(response);
}

pub(super) fn json(req: Request, code: u16, body: &impl serde::Serialize) {
    respond(req, code, serde_json::to_string(body).unwrap_or_default(), "application/json")
}

pub(super) fn error(req: Request, message: impl std::fmt::Display) {
    json(req, 400, &serde_json::json!({ "error": message.to_string() }))
}

fn header(req: &Request, name: &str) -> Option<String> {
    let found = req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name));
    found.map(|h| h.value.as_str().to_string())
}

fn local_hosts(port: u16) -> Vec<String> {
    LOOPBACK.iter().map(|h| host_port(h, port)).collect()
}

/// Whether any request may be served: on a loopback bind, the Host must be loopback too.
pub(super) fn host_allowed(req: &Request, bind: &str, port: u16) -> bool {
    !is_loopback(bind) || header(req, "Host").is_some_and(|h| local_hosts(port).contains(&h))
}

/// Whether a POST may change anything (see the module comment). Never beyond loopback.
pub(super) fn write_allowed(req: &Request, bind: &str, port: u16) -> bool {
    let origin_ok =
        header(req, "Origin").map_or(true, |o| local_hosts(port).iter().any(|l| o == format!("http://{l}")));
    is_loopback(bind)
        && host_allowed(req, bind, port)
        && origin_ok
        && header(req, "X-Drmanhattan").as_deref() == Some("1")
        && header(req, "Content-Type").is_some_and(|c| c.starts_with("application/json"))
}

/// One query parameter, percent-decoded.
pub(super) fn query_param(url: &str, key: &str) -> Option<String> {
    url.split_once('?')?.1.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == key).then(|| percent_decode(v))
    })
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
        match (bytes[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (b'+', _) => {
                out.push(b' ');
                i += 1;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parameters_are_decoded() {
        assert_eq!(query_param("/api/log?run_id=1-2-ab&lines=50", "lines").as_deref(), Some("50"));
        assert_eq!(query_param("/api/log?run_id=1%2D2%2dab", "run_id").as_deref(), Some("1-2-ab"));
        assert_eq!(query_param("/x?a=b%20c+d%zz", "a").as_deref(), Some("b c d%zz"));
        assert_eq!(query_param("/api/log", "run_id"), None);
    }

    #[test]
    fn ipv6_hosts_are_bracketed() {
        assert_eq!(host_port("::1", 80), "[::1]:80");
        assert_eq!(host_port("127.0.0.1", 80), "127.0.0.1:80");
    }
}
