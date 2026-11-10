import { execFile } from "node:child_process";
import { constants } from "node:fs";
import { access } from "node:fs/promises";
import { homedir } from "node:os";
import { delimiter, join } from "node:path";
import { experimental_defineHostEntry } from "@get-bb/plugin-sdk/host";
import { z } from "zod";
import {
  drmanhattanHostContract,
  type HostStatus,
  type Queue,
  type Run,
} from "./contract.js";

export const MIN_SESSION_VERSION = "0.8.0";
const COMMAND_TIMEOUT_MS = 15_000;
const MAX_OUTPUT_BYTES = 16 * 1024 * 1024;

const timestamp = z.string().nullish().transform((value) => value ?? null);
const count = z.number().nullish().transform((value) => value ?? null);
const text = z.string().nullish().transform((value) => value ?? null);

const rawRunning = z.object({
  run_id: z.string(),
  label: z.string(),
  category: z.string().default(""),
  slot: z.number().int().nullish(),
  machine: text,
  service: z.boolean().default(false),
  quick: z.boolean().default(false),
  ready: z.boolean().nullish(),
  cwd: text,
  started_at: timestamp,
  elapsed_seconds: count,
  expected_seconds: count,
  log: text,
});
const rawWaiter = z.object({
  run_id: z.string(),
  label: z.string(),
  category: z.string().default(""),
  position: z.number().int().nullish(),
  quick: z.boolean().default(false),
  waited_seconds: count,
  expected_seconds: count,
});
const rawFinished = z.object({
  run_id: z.string(),
  label: z.string(),
  category: z.string().default(""),
  state: z.string(),
  exit: z.number().int().nullish(),
  cwd: text,
  started_at: timestamp,
  finished_at: timestamp,
  duration_seconds: count,
  error_line: text,
  log: text,
});
const rawStatus = z.object({
  state: z.string(),
  version: z.string(),
  daemon: z.object({ url: text }).nullish(),
  active: z.number().int().default(0),
  capacity: z.number().int().default(0),
  running: z.array(rawRunning).default([]),
  waiters: z.array(rawWaiter).default([]),
  finished: z.array(rawFinished).nullish(),
});

const EMPTY_RUN: Omit<Run, "runId" | "label" | "category" | "state"> = {
  service: false,
  quick: false,
  slot: null,
  position: null,
  machine: null,
  cwd: null,
  startedAt: null,
  finishedAt: null,
  elapsedSeconds: null,
  waitedSeconds: null,
  expectedSeconds: null,
  durationSeconds: null,
  exit: null,
  errorLine: null,
  log: null,
};

export function parseStatus(stdout: string): Queue {
  const raw = rawStatus.parse(JSON.parse(stdout));
  return {
    version: raw.version,
    state: raw.state,
    dashboardUrl: raw.daemon?.url ?? null,
    active: raw.active,
    capacity: raw.capacity,
    running: raw.running.map((run) => ({
      ...EMPTY_RUN,
      runId: run.run_id,
      label: run.label,
      category: run.category,
      state: run.service && run.ready === false ? "starting" : "running",
      service: run.service,
      quick: run.quick,
      slot: run.service || run.quick ? null : (run.slot ?? null),
      machine: run.machine,
      cwd: run.cwd,
      startedAt: run.started_at,
      elapsedSeconds: run.elapsed_seconds,
      expectedSeconds: run.expected_seconds,
      log: run.log,
    })),
    waiting: raw.waiters.map((run) => ({
      ...EMPTY_RUN,
      runId: run.run_id,
      label: run.label,
      category: run.category,
      state: "waiting",
      quick: run.quick,
      position: run.position ?? null,
      waitedSeconds: run.waited_seconds,
      expectedSeconds: run.expected_seconds,
    })),
    finished: (raw.finished ?? []).map((run) => ({
      ...EMPTY_RUN,
      runId: run.run_id,
      label: run.label,
      category: run.category,
      state: run.state,
      cwd: run.cwd,
      startedAt: run.started_at,
      finishedAt: run.finished_at,
      durationSeconds: run.duration_seconds,
      exit: run.exit ?? null,
      errorLine: run.error_line,
      log: run.log,
    })),
  };
}

export function versionAtLeast(version: string, minimum: string): boolean {
  const parts = (value: string) =>
    value
      .replace(/^v/, "")
      .split(/[.+-]/)
      .slice(0, 3)
      .map((part) => Number.parseInt(part, 10) || 0);
  const have = parts(version);
  const need = parts(minimum);
  for (let index = 0; index < 3; index += 1) {
    const left = have[index] ?? 0;
    const right = need[index] ?? 0;
    if (left !== right) return left > right;
  }
  return true;
}

async function isExecutable(path: string): Promise<boolean> {
  try {
    await access(path, constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

export async function findBinary(
  env: NodeJS.ProcessEnv = process.env,
): Promise<string | null> {
  const explicit = env.DRMANHATTAN_BIN?.trim();
  if (explicit) return (await isExecutable(explicit)) ? explicit : null;
  const home = homedir();
  const directories = [
    ...(env.PATH ?? "").split(delimiter),
    join(home, ".local", "bin"),
    join(home, ".cargo", "bin"),
    "/opt/homebrew/bin",
    "/usr/local/bin",
  ].filter((directory) => directory !== "");
  for (const directory of new Set(directories)) {
    const candidate = join(directory, "drmanhattan");
    if (await isExecutable(candidate)) return candidate;
  }
  return null;
}

interface Output {
  code: number;
  stdout: string;
  stderr: string;
}

function run(
  binary: string,
  args: string[],
  signal: AbortSignal,
): Promise<Output> {
  return new Promise((resolve, reject) => {
    execFile(
      binary,
      args,
      {
        signal,
        timeout: COMMAND_TIMEOUT_MS,
        maxBuffer: MAX_OUTPUT_BYTES,
        encoding: "utf8",
      },
      (error, stdout, stderr) => {
        if (error === null) {
          resolve({ code: 0, stdout, stderr });
          return;
        }
        if (typeof error.code === "number") {
          resolve({ code: error.code, stdout, stderr });
          return;
        }
        reject(error);
      },
    );
  });
}

function firstLine(value: string): string {
  return value.trim().split("\n")[0] ?? "";
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export async function readStatus(
  input: { session: string | null; limit: number },
  signal: AbortSignal,
  env: NodeJS.ProcessEnv = process.env,
): Promise<HostStatus> {
  const binary = await findBinary(env);
  if (binary === null) {
    return {
      kind: "missing",
      binary: null,
      message:
        "drmanhattan is not installed on this machine (looked on PATH, in ~/.local/bin and ~/.cargo/bin).",
    };
  }
  const args = ["status", "--json"];
  if (input.session !== null) {
    args.push("--session", input.session, "--limit", String(input.limit));
  }
  try {
    const output = await run(binary, args, signal);
    if (output.code === 0) {
      return { kind: "ok", binary, queue: parseStatus(output.stdout) };
    }
    const version = await run(binary, ["--version"], signal).catch(() => null);
    const installed = version?.stdout.trim().split(/\s+/).at(-1) ?? "";
    if (
      input.session !== null &&
      installed !== "" &&
      !versionAtLeast(installed, MIN_SESSION_VERSION)
    ) {
      return {
        kind: "outdated",
        binary,
        message: `drmanhattan ${installed} cannot list one thread's runs; ${MIN_SESSION_VERSION} or later can. Run \`drmanhattan update\`.`,
      };
    }
    return {
      kind: "error",
      binary,
      message:
        firstLine(output.stderr) ||
        `drmanhattan status exited with ${output.code}`,
    };
  } catch (error) {
    return { kind: "error", binary, message: message(error) };
  }
}

export async function cancelRun(
  runId: string,
  signal: AbortSignal,
  env: NodeJS.ProcessEnv = process.env,
): Promise<{ ok: boolean; message: string }> {
  const binary = await findBinary(env);
  if (binary === null) {
    return { ok: false, message: "drmanhattan is not installed on this machine." };
  }
  try {
    const output = await run(binary, ["cancel", runId], signal);
    return {
      ok: output.code === 0,
      message:
        firstLine(output.code === 0 ? output.stdout : output.stderr) ||
        `drmanhattan cancel exited with ${output.code}`,
    };
  } catch (error) {
    return { ok: false, message: message(error) };
  }
}

export default experimental_defineHostEntry({
  contract: drmanhattanHostContract,
  handlers: {
    status: (input, context) => readStatus(input, context.signal),
    cancel: ({ runId }, context) => cancelRun(runId, context.signal),
  },
});
