//! What the dashboard can change: the manual queue order (dragging a queued run), the slots
//! (the − and + buttons, kept in config.json), pausing, the route to workers, a service
//! (stop, restart), and updates.

use std::collections::HashSet;
use std::io::Read;

use serde::Deserialize;
use serde_json::json;
use tiny_http::Request;

use crate::dashboard::{http, Ctx};
use crate::fsutil;
use crate::job::{RunId, State};
use crate::queue;
use crate::settings::{set_paused, set_route, set_slots, set_worker_slots, write_manual_order, MAX_SLOTS};

const MAX_BODY: u64 = 64 * 1024;
const MAX_ORDER: usize = 500;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityBody {
    slots: u32,
    /// A worker's slots rather than this machine's; 0 pauses it.
    #[serde(default)]
    worker: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderBody {
    /// None or empty clears the manual order.
    order: Option<Vec<RunId>>,
}

pub(super) fn handle(mut req: Request, path: &str, ctx: &Ctx) {
    if !http::write_allowed(&req, &ctx.bind, ctx.port) {
        return http::respond(req, 403, "forbidden", "text/plain");
    }
    let mut body = String::new();
    let too_big = req.body_length().is_some_and(|n| n as u64 > MAX_BODY);
    if too_big || req.as_reader().take(MAX_BODY).read_to_string(&mut body).is_err() {
        return http::error(req, "the body is too large or not text");
    }
    let body = if body.trim().is_empty() { "{}" } else { &body };
    if !fsutil::is_private_dir(&ctx.paths.state) {
        return http::error(req, "the state directory is missing or not private");
    }
    match path {
        "/api/capacity" => set_capacity(req, body, ctx),
        "/api/service" => service_action(req, body, ctx),
        "/api/pause" => set_pause(req, body, ctx),
        "/api/route" => set_route_to(req, body, ctx),
        "/api/update" => start_update(req, ctx),
        "/api/auto-update" => set_auto_update(req, body, ctx),
        _ => set_order(req, body, ctx),
    }
}

/// The body as `T`, from a JSON object only: serde would also read a struct from an array.
fn object<T: serde::de::DeserializeOwned>(body: &str) -> Option<T> {
    match serde_json::from_str(body).ok()? {
        value @ serde_json::Value::Object(_) => serde_json::from_value(value).ok(),
        _ => None,
    }
}

/// The − and + buttons: the standing count in `config.json`, which stays until changed.
fn set_capacity(req: Request, body: &str, ctx: &Ctx) {
    let Some(CapacityBody { slots, worker }) = object::<CapacityBody>(body) else {
        return http::error(
            req,
            format!("send {{\"slots\": 1 to {MAX_SLOTS}}}, with \"worker\" for a worker's (0 pauses it)"),
        );
    };
    if let Some(worker) = worker {
        return match set_worker_slots(&ctx.paths, &worker, slots) {
            Ok(slots) => {
                if slots > 0 {
                    crate::workers::push_slots(&ctx.paths, &worker);
                }
                http::json(req, 200, &json!({ "worker": worker, "slots": slots }))
            }
            Err(e) => http::error(req, e),
        };
    }
    if let Some(raw) = &ctx.settings.slots_env {
        return http::error(req, format!("DRMANHATTAN_SLOTS={raw} is set for the daemon, and it decides the slots"));
    }
    match set_slots(&ctx.paths, slots) {
        Ok(slots) => http::json(req, 200, &json!({ "slots": slots })),
        Err(e) => http::error(req, e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PauseBody {
    paused: bool,
    /// A worker, rather than this machine as a worker for others.
    #[serde(default)]
    worker: Option<String>,
}

/// Pause and resume: this machine as a worker for others, or one of this machine's workers
/// (which is told over SSH, so its own dashboard shows it too).
fn set_pause(req: Request, body: &str, ctx: &Ctx) {
    let Some(PauseBody { paused, worker }) = object::<PauseBody>(body) else {
        return http::error(req, "send {\"paused\": true or false}, with \"worker\" for a worker");
    };
    let done = match &worker {
        Some(worker) => crate::workers::push_pause(&ctx.paths, worker, paused).map(|()| paused),
        None => set_paused(&ctx.paths, paused),
    };
    match done {
        Ok(paused) => http::json(req, 200, &json!({ "paused": paused, "worker": worker })),
        Err(e) => http::error(req, e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteBody {
    /// Workers; empty turns the route off.
    route: Vec<String>,
}

/// A worker's route button: the workers that jobs which may run on a worker go to, and
/// only to, as `drmanhattan route` sets them.
fn set_route_to(req: Request, body: &str, ctx: &Ctx) {
    let Some(RouteBody { route }) = object::<RouteBody>(body) else {
        return http::error(req, "send {\"route\": [worker names]}, or [] to turn it off");
    };
    match set_route(&ctx.paths, &route) {
        Ok(route) => http::json(req, 200, &json!({ "route": route })),
        Err(e) => http::error(req, e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceBody {
    run_id: RunId,
    /// `stop` or `restart`.
    action: String,
}

/// The Services section's buttons: stop a service, or stop it and start it again.
fn service_action(req: Request, body: &str, ctx: &Ctx) {
    let Some(ServiceBody { run_id, action }) = object::<ServiceBody>(body) else {
        return http::error(req, "send {\"run_id\": …, \"action\": \"stop\" or \"restart\"}");
    };
    let mut q = queue::lock(&ctx.shared);
    let live_service = q.runs.get(&run_id).is_some_and(|e| e.rec.job.service && !e.rec.state.is_terminal());
    if !live_service {
        drop(q);
        return http::error(req, "no running service has that run id");
    }
    match action.as_str() {
        "stop" => q.cancel(&run_id),
        "restart" => q.restart(&run_id),
        _ => {
            drop(q);
            return http::error(req, "action must be stop or restart");
        }
    }
    drop(q);
    http::json(req, 200, &json!({ "run_id": run_id, "action": action }));
}

/// The footer's Update button: only while nothing runs or waits, and only to a newer release.
fn start_update(req: Request, ctx: &Ctx) {
    if !crate::update::idle(&ctx.paths) {
        return http::respond(req, 409, "jobs are running or waiting; update once the queue is idle", "text/plain");
    }
    // Either a newer release, or one installed already that this daemon has yet to run.
    if crate::update::available(&ctx.paths).is_none() && crate::update::installed_version().is_none() {
        return http::error(req, "no newer release is known");
    }
    match crate::update::start_detached() {
        Ok(()) => http::json(req, 202, &json!({ "updating": true })),
        Err(e) => http::error(req, e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AutoUpdateBody {
    on: bool,
}

/// The footer's auto-update toggle.
fn set_auto_update(req: Request, body: &str, ctx: &Ctx) {
    let Some(AutoUpdateBody { on }) = object::<AutoUpdateBody>(body) else {
        return http::error(req, "send {\"on\": true or false}");
    };
    let saved = crate::settings::update_config(&ctx.paths, |c| {
        c.auto_update = on;
        Ok(())
    });
    match saved {
        Ok(()) => http::json(req, 200, &json!({ "auto_update": on })),
        Err(e) => http::error(req, e),
    }
}

/// Keeps only ids of runs still queued and not priority (the manual order cannot move a
/// priority run), each once, in the order given.
fn set_order(req: Request, body: &str, ctx: &Ctx) {
    let order = match object::<OrderBody>(body) {
        Some(OrderBody { order }) if order.as_ref().map_or(0, Vec::len) <= MAX_ORDER => order.unwrap_or_default(),
        _ => return http::error(req, "send {\"order\": [run ids]}, or {\"order\": null} to clear"),
    };
    let order: Vec<RunId> = {
        let q = queue::lock(&ctx.shared);
        let mut seen = HashSet::new();
        let movable = |id: &RunId| q.runs.get(id).is_some_and(|e| e.rec.state == State::Queued && !e.rec.job.priority);
        order.into_iter().filter(|id| movable(id) && seen.insert(id.clone())).collect()
    };
    match write_manual_order(&ctx.paths, &order) {
        Ok(()) => http::json(req, 200, &json!({ "order": order })),
        Err(e) => http::error(req, e),
    }
}
