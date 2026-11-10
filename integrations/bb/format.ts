import type { Run } from "./contract.js";

/** Seconds as `42s`, `3m 5s` or `1h 20m`. */
export function duration(seconds: number | null): string | null {
  if (seconds === null) return null;
  const whole = Math.max(0, Math.round(seconds));
  if (whole < 60) return `${whole}s`;
  if (whole < 3600) return `${Math.floor(whole / 60)}m ${whole % 60}s`;
  return `${Math.floor(whole / 3600)}h ${Math.floor((whole % 3600) / 60)}m`;
}

export function isActive(run: Run): boolean {
  return run.finishedAt === null;
}

/** Where a run stands, in a few words: `slot 1 · 2m 5s of ~4m`, `#2 in line`, `exit 3 · 40s`. */
export function runDetail(run: Run): string {
  const parts: string[] = [];
  if (run.state === "waiting") {
    if (run.position !== null) parts.push(`#${run.position} in line`);
    if (run.quick) parts.push("quick");
    const waited = duration(run.waitedSeconds);
    if (waited !== null) parts.push(`waited ${waited}`);
  } else if (isActive(run)) {
    if (run.service) parts.push(run.state === "starting" ? "service, starting" : "service");
    else if (run.quick) parts.push("quick lane");
    else if (run.slot !== null) parts.push(`slot ${run.slot}`);
    if (run.machine !== null) parts.push(`on ${run.machine}`);
    const elapsed = duration(run.elapsedSeconds);
    const expected = duration(run.expectedSeconds);
    if (elapsed !== null) parts.push(expected === null ? elapsed : `${elapsed} of ~${expected}`);
  } else {
    parts.push(run.state.replace("_", " "));
    if (run.exit !== null) parts.push(`exit ${run.exit}`);
    const took = duration(run.durationSeconds);
    if (took !== null) parts.push(took);
  }
  return parts.join(" · ");
}

/** The dashboard page with this run's log open. */
export function runUrl(dashboardUrl: string, runId: string): string {
  return `${dashboardUrl.replace(/\/+$/, "")}/#run=${encodeURIComponent(runId)}`;
}
