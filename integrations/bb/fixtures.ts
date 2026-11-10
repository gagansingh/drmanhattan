import type { Queue, Run } from "./contract.js";

export function makeRun(overrides: Partial<Run> = {}): Run {
  return {
    runId: "1793647588-31235-3799f0",
    label: "cargo test",
    category: "cargo",
    state: "running",
    service: false,
    quick: false,
    slot: 1,
    position: null,
    machine: null,
    cwd: "/work/app",
    startedAt: "2026-11-10T12:00:00Z",
    finishedAt: null,
    elapsedSeconds: 65,
    waitedSeconds: null,
    expectedSeconds: 240,
    durationSeconds: null,
    exit: null,
    errorLine: null,
    log: "/home/me/.drmanhattan/inbox/cargo_test.log",
    ...overrides,
  };
}

export function makeQueue(overrides: Partial<Queue> = {}): Queue {
  return {
    version: "0.8.0",
    state: "busy",
    dashboardUrl: "http://127.0.0.1:8417/",
    active: 1,
    capacity: 1,
    running: [makeRun()],
    waiting: [
      makeRun({
        runId: "1793647590-31236-aa01",
        label: "npm run build",
        category: "npm",
        state: "waiting",
        slot: null,
        position: 2,
        startedAt: null,
        elapsedSeconds: null,
        waitedSeconds: 12,
        expectedSeconds: 90,
      }),
    ],
    finished: [
      makeRun({
        runId: "1793647000-31200-bb02",
        label: "cargo clippy",
        state: "failed",
        slot: null,
        finishedAt: "2026-11-10T11:50:00Z",
        elapsedSeconds: null,
        expectedSeconds: null,
        durationSeconds: 41,
        exit: 101,
        errorLine: "error: unused variable",
      }),
    ],
    ...overrides,
  };
}
