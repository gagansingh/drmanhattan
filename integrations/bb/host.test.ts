import { chmodSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { cancelRun, parseStatus, readStatus, stopServices, versionAtLeast } from "./host.js";

/** What `drmanhattan status --session thr_a --json` prints (0.8.0). */
const STATUS = JSON.stringify({
  state: "busy",
  daemon: { pid: 4242, url: "http://127.0.0.1:8417/" },
  active: 1,
  capacity: 1,
  quick: 0,
  quick_capacity: 8,
  version: "0.8.0",
  auto_update: false,
  auto_update_state: "off",
  session: "thr_a",
  running: [
    {
      slot: 1,
      run_id: "1793647588-31235-3799f0",
      label: "cargo test",
      category: "cargo",
      cwd: "/work/app",
      started_at: "2026-11-10T12:00:00Z",
      elapsed_seconds: 65,
      expected_seconds: 240,
      log: "/inbox/cargo_test.log",
      job_file: "/inbox/cargo_test.json",
      source: { tool: "bb", session: "thr_a" },
    },
    {
      slot: 0,
      service: true,
      ready: false,
      run_id: "1793647589-31235-3799f1",
      label: "npm run dev",
      category: "npm",
      cwd: "/work/app",
      started_at: "2026-11-10T12:00:01Z",
      elapsed_seconds: 3,
      expected_seconds: null,
      log: "/inbox/npm_run_dev.log",
      job_file: "/inbox/npm_run_dev.json",
    },
  ],
  waiters: [
    {
      position: 3,
      run_id: "1793647590-31236-aa01",
      label: "npm run build",
      category: "npm",
      waited_seconds: 12,
      expected_seconds: 90,
      skip_after_seconds: 600,
      priority: false,
      overdue: false,
      manual: false,
      quick: false,
      job_file: "/inbox/b.json",
    },
  ],
  finished: [
    {
      run_id: "1793647000-31200-bb02",
      label: "cargo clippy",
      category: "cargo",
      state: "failed",
      exit: 101,
      cwd: "/work/app",
      started_at: "2026-11-10T11:49:19Z",
      finished_at: "2026-11-10T11:50:00Z",
      duration_seconds: 41,
      error_line: "error: unused variable",
      log: "/inbox/cargo_clippy.log",
    },
  ],
});

function fakeDrmanhattan(script: string): NodeJS.ProcessEnv {
  const dir = mkdtempSync(join(tmpdir(), "drmanhattan-bb-"));
  const bin = join(dir, "drmanhattan");
  writeFileSync(join(dir, "status.json"), STATUS);
  writeFileSync(join(dir, "args"), "");
  writeFileSync(bin, `#!/bin/sh\necho "$@" >> "${dir}/args"\n${script}\n`);
  chmodSync(bin, 0o755);
  return { PATH: "/usr/bin:/bin", DRMANHATTAN_BIN: bin, DIR: dir };
}

const signal = new AbortController().signal;

describe("parseStatus", () => {
  it("reads drmanhattan's status document into runs", () => {
    const queue = parseStatus(STATUS);
    expect(queue).toMatchObject({ version: "0.8.0", state: "busy", dashboardUrl: "http://127.0.0.1:8417/" });
    expect(queue.running.map((run) => [run.state, run.slot, run.service])).toEqual([
      ["running", 1, false],
      ["starting", null, true],
    ]);
    expect(queue.waiting[0]).toMatchObject({ state: "waiting", position: 3, waitedSeconds: 12 });
    expect(queue.finished[0]).toMatchObject({ state: "failed", exit: 101, durationSeconds: 41 });
  });

  it("gives a run in the light lane no slot: its number is not one", () => {
    const running = [{ ...JSON.parse(STATUS).running[0], slot: 257, light_lane: true }];
    const queue = parseStatus(JSON.stringify({ ...JSON.parse(STATUS), running }));
    expect(queue.running.map((run) => [run.state, run.slot])).toEqual([["running", null]]);
  });

  it("links a run to the bb thread that queued it", () => {
    const queue = parseStatus(STATUS);
    expect(queue.running.map((run) => run.threadId)).toEqual(["thr_a", null]);
    const other = parseStatus(
      JSON.stringify({ state: "busy", version: "0.9.0", running: [], waiters: [{ ...JSON.parse(STATUS).waiters[0], source: { tool: "Claude Code", session: "s1" } }] }),
    );
    expect(other.waiting[0]?.threadId).toBeNull();
  });

  it("takes a document without --session's fields", () => {
    const queue = parseStatus(JSON.stringify({ state: "stopped", daemon: null, version: "0.7.1", running: [], waiters: [] }));
    expect(queue).toMatchObject({ dashboardUrl: null, finished: [], running: [], waiting: [] });
  });
});

describe("readStatus", () => {
  it("asks for one session's runs", async () => {
    const env = fakeDrmanhattan(`cat "$(dirname "$0")/status.json"`);
    const status = await readStatus({ session: "thr_a", limit: 5 }, signal, env);
    expect(status.kind).toBe("ok");
    expect(readFileSync(join(env.DIR!, "args"), "utf8")).toBe("status --json --session thr_a --limit 5\n");
  });

  it("asks for the whole queue's newest finished runs", async () => {
    const env = fakeDrmanhattan(`cat "$(dirname "$0")/status.json"`);
    expect((await readStatus({ session: null, limit: 15 }, signal, env)).kind).toBe("ok");
    expect(readFileSync(join(env.DIR!, "args"), "utf8")).toBe("status --json --limit 15\n");
  });

  it("gives the queue without them from a drmanhattan before 0.9.0", async () => {
    const env = fakeDrmanhattan(`case "$*" in
      --version) echo "drmanhattan 0.8.2";;
      *--limit*) echo "error: --limit goes with --session" >&2; exit 2;;
      *) cat "$(dirname "$0")/status.json";;
    esac`);
    expect((await readStatus({ session: null, limit: 15 }, signal, env)).kind).toBe("ok");
    expect(readFileSync(join(env.DIR!, "args"), "utf8")).toBe("status --json --limit 15\n--version\nstatus --json\n");
  });

  it("says when drmanhattan is not installed", async () => {
    const status = await readStatus({ session: null, limit: 0 }, signal, { PATH: "", DRMANHATTAN_BIN: "/nowhere/drmanhattan" });
    expect(status.kind).toBe("missing");
  });

  it("says when drmanhattan is too old to list a thread's runs", async () => {
    const env = fakeDrmanhattan(
      `case "$1" in --version) echo "drmanhattan 0.7.1";; *) echo "unknown option --session" >&2; exit 2;; esac`,
    );
    const status = await readStatus({ session: "thr_a", limit: 5 }, signal, env);
    expect(status).toMatchObject({ kind: "outdated" });
    expect(status.kind !== "ok" && status.message).toContain("drmanhattan update");
  });

  it("reports another failure as it is", async () => {
    const env = fakeDrmanhattan(`case "$1" in --version) echo "drmanhattan 0.8.0";; *) echo "state directory unreadable" >&2; exit 1;; esac`);
    expect(await readStatus({ session: "thr_a", limit: 5 }, signal, env)).toMatchObject({
      kind: "error",
      message: "state directory unreadable",
    });
  });
});

describe("cancelRun", () => {
  it("runs drmanhattan cancel with the run id", async () => {
    const env = fakeDrmanhattan(`echo "run_id=$2 state=cancelled"`);
    expect(await cancelRun("1793647588-31235-3799f0", signal, env)).toEqual({
      ok: true,
      message: "run_id=1793647588-31235-3799f0 state=cancelled",
    });
  });
});

describe("stopServices", () => {
  it("cancels the session's running services and nothing else", async () => {
    const env = fakeDrmanhattan(`case "$1" in
      status) cat "$(dirname "$0")/status.json";;
      cancel) echo "run_id=$2 state=cancelled";;
    esac`);
    expect(await stopServices("thr_a", signal, env)).toEqual({ ok: true, stopped: ["npm run dev"], message: "Stopped npm run dev." });
    expect(readFileSync(join(env.DIR!, "args"), "utf8")).toBe(
      "status --json --session thr_a --limit 0\ncancel 1793647589-31235-3799f1\n",
    );
  });

  it("does nothing on a machine without drmanhattan", async () => {
    expect(await stopServices("thr_a", signal, { PATH: "", DRMANHATTAN_BIN: "/nowhere/drmanhattan" })).toMatchObject({
      ok: true,
      stopped: [],
    });
  });
});

describe("versionAtLeast", () => {
  it("compares release versions", () => {
    expect(versionAtLeast("0.8.0", "0.8.0")).toBe(true);
    expect(versionAtLeast("0.10.1", "0.8.0")).toBe(true);
    expect(versionAtLeast("v1.0.0", "0.8.0")).toBe(true);
    expect(versionAtLeast("0.7.9", "0.8.0")).toBe(false);
  });
});
