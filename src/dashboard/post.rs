//! The dashboard's two writes: the manual queue order (dragging a queued run) and the
//! capacity override (the − and + buttons).

use std::collections::HashSet;
use std::io::Read;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tiny_http::Request;

use crate::dashboard::{http, Ctx};
use crate::fsutil;
use crate::job::{RunId, State};
use crate::queue;
use crate::settings::{load_config, standing_capacity, write_manual_order, write_override, write_worker_override};

/// How long the − and + buttons' capacity lasts.
const OVERRIDE_LASTS: Duration = Duration::from_secs(4 * 3600);
const MAX_BODY: u64 = 64 * 1024;
const MAX_ORDER: usize = 500;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityBody {
    /// None clears the override.
    slots: Option<u32>,
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
        "/api/update" => start_update(req, ctx),
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

fn set_capacity(req: Request, body: &str, ctx: &Ctx) {
    if let Some(CapacityBody { slots, worker: Some(worker) }) = object::<CapacityBody>(body) {
        return set_worker_capacity(req, &worker, slots, ctx);
    }
    let standing = standing_capacity(&ctx.paths, ctx.settings.slots_env.as_deref()).map_or(1, |(s, _)| s);
    let ceiling = ctx.settings.capacity_ceiling(standing);
    let slots = match object::<CapacityBody>(body) {
        Some(CapacityBody { slots, worker: None }) if slots.map_or(true, |s| (1..=ceiling).contains(&s)) => slots,
        _ => return http::error(req, format!("send {{\"slots\": 1 to {ceiling}}}, or {{\"slots\": null}} to clear")),
    };
    match write_override(&ctx.paths, slots, OVERRIDE_LASTS) {
        Ok(o) => http::json(req, 200, &json!({ "override": o })),
        Err(e) => http::error(req, e),
    }
}

/// A worker's override: 0 (paused) up to the dashboard's ceiling, or null to go back to its
/// `slots` in config.json.
fn set_worker_capacity(req: Request, worker: &str, slots: Option<u32>, ctx: &Ctx) {
    let workers = load_config(&ctx.paths).map(|c| c.workers).unwrap_or_default();
    let Some(w) = workers.iter().find(|w| w.name == worker) else {
        return http::error(req, format!("no worker is named {worker:?}"));
    };
    let ceiling = ctx.settings.capacity_ceiling(w.slots);
    if slots.is_some_and(|s| s > ceiling) {
        return http::error(req, format!("send {{\"worker\": …, \"slots\": 0 to {ceiling}}}, or null to clear"));
    }
    match write_worker_override(&ctx.paths, &w.name, slots, OVERRIDE_LASTS) {
        Ok(o) => http::json(req, 200, &json!({ "worker": w.name, "override": o })),
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
    if crate::update::available(&ctx.paths).is_none() {
        return http::error(req, "no newer release is known");
    }
    match crate::update::start_detached() {
        Ok(()) => http::json(req, 202, &json!({ "updating": true })),
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
