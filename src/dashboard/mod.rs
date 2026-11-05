//! The dashboard: a small HTTP server serving one embedded page and its API.
//!
//! - `GET /` is the page (`assets/dashboard.html`), which polls
//! - `GET /api/state?…` (`state`) every two seconds, asking for a page of finished runs, and
//! - `GET /api/log?run_id=…&lines=…` for the tail of a run's log, as text.
//! - `POST /api/…` (`post`) for what the page can change: the manual queue order, the
//!   slots, a service, and updates.
//!
//! See `http` for the defences against other pages in the same browser, and `lan` for the
//! dashboard on this network.

mod http;
pub(crate) mod lan;
mod post;
mod run_view;
mod state;

use std::path::PathBuf;
use std::sync::Arc;

use tiny_http::{Method, Request, Server};

pub(crate) use http::{host_port, is_loopback};

use crate::error::{invalid, Result};
use crate::fsutil;
use crate::job::RunId;
use crate::paths::Paths;
use crate::queue::{self, Shared};
use crate::settings::Settings;

const PAGE: &str = include_str!("../../assets/dashboard.html");
/// The page's command formatter, kept apart so it can be read and tested on its own.
const COMMAND_FORMAT: &str = include_str!("../../assets/command-format.js");
/// The page polls every two seconds; a few threads serve several tabs without queueing.
const HTTP_THREADS: usize = 4;
const LOG_TAIL_BYTES: u64 = 256 * 1024;

/// What every request handler needs, fixed when the server starts.
pub(crate) struct Ctx {
    shared: Shared,
    paths: Paths,
    settings: Settings,
    inbox: PathBuf,
    bind: String,
    port: u16,
    session_links: bool,
}

/// Binds the server (port 0 picks a free one) and serves it from a small pool of threads.
/// Returns the page's URL.
pub(crate) fn start(shared: Shared, bind: &str, port: u16, session_links: bool) -> Result<String> {
    let addr = host_port(bind, port);
    let server = Server::http(&addr).or_else(|e| invalid(format!("could not listen on {addr}: {e}")))?;
    let port = server.server_addr().to_ip().map_or(port, |a| a.port());
    let (paths, settings, inbox) = {
        let q = queue::lock(&shared);
        (q.paths.clone(), q.settings.clone(), q.inbox.clone())
    };
    let ctx = Arc::new(Ctx { shared, paths, settings, inbox, bind: bind.to_string(), port, session_links });
    serve(Arc::new(server), &ctx, HTTP_THREADS, false)?;
    if is_loopback(bind) {
        lan::watch(ctx);
    }
    Ok(format!("http://{}/", host_port(bind, port)))
}

/// Answers `server`'s requests from `threads` threads, until it is unblocked. `lan`: it
/// listens on the network, where every request needs the key.
fn serve(server: Arc<Server>, ctx: &Arc<Ctx>, threads: usize, lan: bool) -> Result<()> {
    for i in 0..threads {
        let (server, ctx) = (server.clone(), ctx.clone());
        let thread = std::thread::Builder::new().name(format!("http-{}{i}", if lan { "lan-" } else { "" }));
        thread
            .spawn(move || {
                while let Ok(req) = server.recv() {
                    handle(req, &ctx, lan);
                }
            })
            .or_else(|e| invalid(format!("could not start the dashboard: {e}")))?;
    }
    Ok(())
}

fn handle(req: Request, ctx: &Ctx, lan: bool) {
    if lan {
        // Read each time, so a new key (`drmanhattan lan new-key`) holds at once.
        let Ok(key) = lan::key(&ctx.paths) else { return http::respond(req, 500, "no key", "text/plain") };
        match lan::access(&req, &key) {
            lan::Access::Allowed => {}
            lan::Access::TradeForCookie => {
                let cookie = lan::cookie_header(&key);
                return http::respond_with(req, 303, "", "text/plain", &[("Location", "/"), ("Set-Cookie", &cookie)]);
            }
            lan::Access::Denied => {
                let why = "forbidden: open the link `drmanhattan lan` prints on the machine running drmanhattan";
                return http::respond(req, 403, why, "text/plain");
            }
        }
    } else if !http::host_allowed(&req, &ctx.bind, ctx.port) {
        return http::respond(req, 403, "forbidden: this server answers only to localhost", "text/plain");
    }
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("/");
    match (req.method(), path) {
        (Method::Get, "/") => http::respond(req, 200, PAGE, "text/html; charset=utf-8"),
        (Method::Get, "/command-format.js") => {
            http::respond(req, 200, COMMAND_FORMAT, "text/javascript; charset=utf-8")
        }
        (Method::Get, "/api/state") => http::json(req, 200, &state::build(ctx, &state::Query::parse(&url))),
        (Method::Get, "/api/log") => log_tail(req, &url, ctx),
        (
            Method::Post,
            "/api/queue-order" | "/api/capacity" | "/api/service" | "/api/sharing" | "/api/route" | "/api/update"
            | "/api/auto-update",
        ) => {
            let path = path.to_string();
            post::handle(req, &path, ctx, lan);
        }
        _ => http::respond(req, 404, "not found", "text/plain"),
    }
}

/// The last `lines` lines (10 to 2000, default 200) of a run's log.
fn log_tail(req: Request, url: &str, ctx: &Ctx) {
    let Some(run_id) = http::query_param(url, "run_id").and_then(|id| id.parse::<RunId>().ok()) else {
        return http::respond(req, 400, "bad run_id", "text/plain");
    };
    let lines = http::query_param(url, "lines").and_then(|l| l.parse::<usize>().ok()).unwrap_or(200).clamp(10, 2000);
    let log = queue::lock(&ctx.shared).runs.get(&run_id).map(|e| e.rec.job.log.clone());
    let text = log.filter(|l| fsutil::is_own_file(l)).and_then(|l| fsutil::read_tail(&l, LOG_TAIL_BYTES));
    match text {
        Some(text) => {
            let all: Vec<&str> = text.split('\n').collect();
            let tail = all[all.len().saturating_sub(lines)..].join("\n");
            http::respond(req, 200, tail, "text/plain; charset=utf-8");
        }
        None => http::respond(req, 404, "no log for this run", "text/plain"),
    }
}
