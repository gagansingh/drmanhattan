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
use crate::settings::{standing_capacity, write_manual_order, write_override};

/// How long the − and + buttons' capacity lasts.
const OVERRIDE_LASTS: Duration = Duration::from_secs(4 * 3600);
const MAX_BODY: u64 = 64 * 1024;
const MAX_ORDER: usize = 500;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityBody {
    /// None clears the override.
    slots: Option<u32>,
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
    let standing = standing_capacity(&ctx.paths, ctx.settings.slots_env.as_deref()).map_or(1, |(s, _)| s);
    let ceiling = ctx.settings.capacity_ceiling(standing);
    let slots = match object::<CapacityBody>(body) {
        Some(CapacityBody { slots }) if slots.map_or(true, |s| (1..=ceiling).contains(&s)) => slots,
        _ => return http::error(req, format!("send {{\"slots\": 1 to {ceiling}}}, or {{\"slots\": null}} to clear")),
    };
    match write_override(&ctx.paths, slots, OVERRIDE_LASTS) {
        Ok(o) => http::json(req, 200, &json!({ "override": o })),
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
