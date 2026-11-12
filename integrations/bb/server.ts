import {
  PluginCliError,
  cliCommand,
  defineCli,
  type BbPluginApi,
} from "@get-bb/plugin-sdk";
import {
  drmanhattanHostContract,
  drmanhattanRpcContract,
  MAX_FINISHED,
  RUN_ID_PATTERN,
  SESSION_PATTERN,
  type HostStatus,
  type MachineQueue,
  type Queue,
  type SetupAction,
  type Run,
  type ThreadRuns,
} from "./contract.js";
import { isActive, runDetail } from "./format.js";

export const SKILL = "drmanhattan";
const PANEL_FINISHED = 10;
/** Finished runs the drmanhattan page lists for each machine. */
export const PAGE_FINISHED = 15;
const PROBE_INTERVAL_MS = 5 * 60_000;
const RETRY_MS = 30_000;
/** The installer downloads a release; the host call gets longer than its 30 s default. */
const SETUP_TIMEOUT_MS = 5 * 60_000;

/** What bb's agents are told when their machine has drmanhattan. */
export const AGENT_INSTRUCTIONS = `## Running commands through drmanhattan
This machine runs drmanhattan, a local queue for heavy commands. Run builds, tests and
other long or heavy commands through it:

    manhattan submit --wait -- <command>

Long commands wait for a free slot, so they never overlap with other threads' builds;
commands that usually finish in seconds here start at once. It prints the output and exits
with the command's exit code. If your own tool times out while waiting, don't submit again:
the run carries on, and \`manhattan follow <name>\` (the name after \`as\` in the line
\`submit\` printed) prints its output and exits with its exit code. \`bb drmanhattan runs\`
lists this thread's runs; \`manhattan status\` shows the whole queue.

Start dev servers and databases as services, never with \`--wait\`:
\`manhattan submit --service --ready-port <port> -- <command>\`. It returns once the port
accepts connections; run \`manhattan stop <name>\` when you are done with it. For commands
that must never overlap and where only the latest run matters, such as deploys, add
\`--group <name>\`.

Jobs get the user's login shell environment but not variables set in this session: pass
those with \`--env KEY=VALUE\`. Jobs get no stdin. If drmanhattan reports that it isn't
running, ask the user to start it; don't start it yourself.`;

type Availability = "installed" | "missing";

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** One run as a line of `bb drmanhattan` output. */
export function describeRun(run: Run): string {
  const state = isActive(run) ? run.state : "";
  const line = [run.runId, state, runDetail(run), run.label].filter((part) => part !== "").join("  ");
  return run.errorLine === null ? line : `${line}\n    ${run.errorLine}`;
}

function describeQueue(queue: Queue, heading: string): string {
  const lines = [
    `${heading}: drmanhattan ${queue.version}, ${queue.state}, ${queue.active} of ${queue.capacity} slots in use`,
  ];
  if (queue.dashboardUrl !== null) lines.push(`dashboard: ${queue.dashboardUrl}`);
  const section = (title: string, runs: Run[]) => {
    if (runs.length === 0) return;
    lines.push("", `${title}:`, ...runs.map((run) => `  ${describeRun(run)}`));
  };
  section("running", queue.running);
  section("waiting", queue.waiting);
  section("finished", queue.finished);
  if (
    queue.running.length + queue.waiting.length + queue.finished.length ===
    0
  ) {
    lines.push("", "no runs");
  }
  return lines.join("\n");
}

export default async function drmanhattanPlugin(
  bb: BbPluginApi,
): Promise<void> {
  const settings = bb.settings.define({
    agentInstructions: {
      type: "boolean",
      label: "Tell agents to run heavy commands through drmanhattan",
      description:
        "Adds drmanhattan's usage to the instructions of threads on machines where drmanhattan is installed.",
      default: true,
    },
    stopServicesOnArchive: {
      type: "boolean",
      label: "Stop a thread's services when it is archived",
      description:
        "When a thread is archived or deleted, stops the dev servers, databases and other services it started through drmanhattan, on every machine.",
      default: true,
    },
  });
  const initial = await settings.get();
  let instruct = initial.agentInstructions;
  let stopOnArchive = initial.stopServicesOnArchive;
  settings.onChange((next) => {
    instruct = next.agentInstructions;
    stopOnArchive = next.stopServicesOnArchive;
  });

  const host = bb.hosts.experimental_client({
    contract: drmanhattanHostContract,
  });
  const availability = new Map<string, Availability>();

  async function hostStatus(
    hostId: string,
    session: string | null,
    limit: number,
    signal?: AbortSignal,
  ): Promise<HostStatus> {
    const status = await host.call(
      "status",
      { session, limit },
      { hostId, ...(signal === undefined ? {} : { signal }) },
    );
    availability.set(hostId, status.kind === "missing" ? "missing" : "installed");
    return status;
  }

  async function hostOfThread(threadId: string): Promise<string | null> {
    const thread = await bb.sdk.threads.get({ threadId });
    if (!thread.environmentId) return null;
    const environment = await bb.sdk.environments.get({
      environmentId: thread.environmentId,
    });
    return environment.hostId ?? null;
  }

  async function threadRuns(
    threadId: string,
    limit: number,
  ): Promise<ThreadRuns> {
    if (!SESSION_PATTERN.test(threadId)) {
      return { kind: "error", message: `Not a thread id: ${threadId}` };
    }
    let hostId: string | null;
    try {
      hostId = await hostOfThread(threadId);
    } catch (error) {
      return {
        kind: "error",
        message: `Could not look up thread ${threadId}: ${errorText(error)}`,
      };
    }
    if (hostId === null) {
      return {
        kind: "no-host",
        message: "This thread has no machine to look on yet.",
      };
    }
    try {
      const status = await hostStatus(hostId, threadId, limit);
      if (status.kind === "ok") {
        return { kind: "ok", hostId, queue: status.queue };
      }
      return { kind: status.kind, message: status.message };
    } catch (error) {
      return { kind: "error", message: errorText(error) };
    }
  }

  async function cancelThreadRun(threadId: string, runId: string) {
    const runs = await threadRuns(threadId, 0);
    if (runs.kind !== "ok") return { ok: false, message: runs.message };
    const active = [...runs.queue.running, ...runs.queue.waiting];
    if (!active.some((run) => run.runId === runId)) {
      return {
        ok: false,
        message: `${runId} is not running or waiting for this thread.`,
      };
    }
    return host.call("cancel", { runId }, { hostId: runs.hostId });
  }

  /** Each connected machine's whole queue, for the drmanhattan page. */
  async function machines(): Promise<{ machines: MachineQueue[] }> {
    const hosts = (await bb.sdk.hosts.list()).filter(
      (candidate) => candidate.status === "connected",
    );
    return {
      machines: await Promise.all(
        hosts.map(async (candidate): Promise<MachineQueue> => {
          let status: HostStatus;
          try {
            status = await hostStatus(candidate.id, null, PAGE_FINISHED);
          } catch (error) {
            status = { kind: "error", binary: null, message: errorText(error) };
          }
          return { hostId: candidate.id, name: candidate.name, status };
        }),
      ),
    };
  }

  async function cancelMachineRun(hostId: string, runId: string) {
    const status = await hostStatus(hostId, null, 0);
    if (status.kind !== "ok") return { ok: false, message: status.message };
    const active = [...status.queue.running, ...status.queue.waiting];
    if (!active.some((run) => run.runId === runId)) {
      return { ok: false, message: `${runId} is not running or waiting on this machine.` };
    }
    return host.call("cancel", { runId }, { hostId });
  }

  /**
   * The drmanhattan page's setup buttons: installs drmanhattan on a machine that lacks it,
   * or starts one that is stopped, and nothing else. Threads there get the instructions
   * from their next turn on.
   */
  async function setupMachine(hostId: string, action: SetupAction) {
    const before = await hostStatus(hostId, null, 0);
    const missing = before.kind === "missing";
    const stopped = before.kind === "ok" && before.queue.state === "stopped";
    if (action === "install" && !missing) {
      return { ok: false, message: "drmanhattan is installed on this machine already." };
    }
    if (action === "start" && !stopped) {
      return {
        ok: false,
        message: before.kind === "ok" ? "drmanhattan is running already." : before.message,
      };
    }
    let result: { ok: boolean; message: string };
    try {
      result = await host.call("setup", { action }, { hostId, timeoutMs: SETUP_TIMEOUT_MS });
    } catch (error) {
      result = { ok: false, message: errorText(error) };
    }
    await hostStatus(hostId, null, 0).catch(() => null);
    bb.log.info(`drmanhattan ${action} on ${hostId}: ${result.message}`);
    return result;
  }

  bb.rpc.register(drmanhattanRpcContract, {
    threadRuns: ({ threadId }) => threadRuns(threadId, PANEL_FINISHED),
    cancelRun: ({ threadId, runId }) => cancelThreadRun(threadId, runId),
    machines: () => machines(),
    cancelMachineRun: ({ hostId, runId }) => cancelMachineRun(hostId, runId),
    setupMachine: ({ hostId, action }) => setupMachine(hostId, action),
  });

  /**
   * Stops the services an archived or deleted thread started. Its machine may be gone
   * with its environment by now, so every connected machine with drmanhattan is asked:
   * services are found by the thread's id, which no other thread has.
   */
  async function stopThreadServices(threadId: string): Promise<void> {
    if (!stopOnArchive || !SESSION_PATTERN.test(threadId)) return;
    const hosts = (await bb.sdk.hosts.list()).filter(
      (candidate) =>
        candidate.status === "connected" && availability.get(candidate.id) !== "missing",
    );
    await Promise.all(
      hosts.map(async (candidate) => {
        try {
          const result = await host.call("stopServices", { session: threadId }, { hostId: candidate.id });
          if (result.stopped.length > 0) {
            bb.log.info(`Stopped thread ${threadId}'s services on ${candidate.id}: ${result.stopped.join(", ")}`);
          }
          if (!result.ok) bb.log.warn(`Thread ${threadId} on ${candidate.id}: ${result.message}`);
        } catch (error) {
          bb.log.warn(`Could not stop thread ${threadId}'s services on ${candidate.id}: ${errorText(error)}`);
        }
      }),
    );
  }

  bb.events.on("thread.archived", ({ thread }) => stopThreadServices(thread.id));
  bb.events.on("thread.deleted", ({ thread }) => stopThreadServices(thread.id));

  bb.agents.configure((context) => {
    const state = availability.get(context.host.id);
    if (state === undefined) requestProbe(context.host.id);
    if (state !== "installed") return { tools: [], skills: [] };
    return {
      tools: [],
      skills: [SKILL],
      ...(instruct ? { instructions: AGENT_INSTRUCTIONS } : {}),
    };
  });

  const json = { type: "boolean", description: "Emit machine-readable JSON" } as const;
  const limitOption = {
    type: "string",
    description: `How many finished runs to list, 0 to ${MAX_FINISHED} (default ${PANEL_FINISHED})`,
  } as const;

  function parseLimit(raw: string | undefined): number {
    if (raw === undefined) return PANEL_FINISHED;
    const value = Number(raw);
    if (!Number.isInteger(value) || value < 0 || value > MAX_FINISHED) {
      throw new PluginCliError(
        `--limit must be a whole number from 0 to ${MAX_FINISHED}`,
        { code: "invalid_limit" },
      );
    }
    return value;
  }

  async function resolveHost(
    machine: string | undefined,
    threadId: string | undefined,
  ): Promise<string> {
    if (machine !== undefined) return machine;
    if (threadId !== undefined) {
      const hostId = await hostOfThread(threadId);
      if (hostId !== null) return hostId;
    }
    const connected = (await bb.sdk.hosts.list()).filter(
      (candidate) => candidate.status === "connected",
    );
    if (connected.length === 1 && connected[0] !== undefined) {
      return connected[0].id;
    }
    throw new PluginCliError("Which machine? Several are connected.", {
      code: "machine_required",
      hint: "Add --machine <host-id>; `bb machine list` prints the ids.",
    });
  }

  function failure(status: Exclude<HostStatus, { kind: "ok" }> | Exclude<ThreadRuns, { kind: "ok" }>): never {
    throw new PluginCliError(status.message, {
      code: `drmanhattan_${status.kind.replace("-", "_")}`,
      ...(status.kind === "missing"
        ? {
            hint: "Install it: curl -fsSL https://github.com/gagansingh/drmanhattan/releases/latest/download/install.sh | sh",
          }
        : {}),
    });
  }

  bb.cli.register(
    defineCli({
      name: "drmanhattan",
      summary: "The drmanhattan job queue: this thread's runs and the whole queue",
      description:
        "drmanhattan queues builds and tests so threads' heavy commands never overlap. Agents submit with `manhattan submit --wait -- <command>`; these commands show what is queued.",
      commands: {
        runs: cliCommand({
          summary: "A thread's runs: running, waiting and recently finished",
          options: {
            thread: {
              type: "string",
              description: "Thread id (default: the thread this runs in)",
            },
            limit: limitOption,
            json,
          },
          async run(input, ctx) {
            const threadId = input.options.thread ?? ctx.threadId;
            if (threadId === undefined) {
              throw new PluginCliError("Which thread?", {
                code: "thread_required",
                hint: "Add --thread <thread-id>.",
              });
            }
            const runs = await threadRuns(threadId, parseLimit(input.options.limit));
            if (runs.kind !== "ok") failure(runs);
            return {
              exitCode: 0,
              stdout: input.options.json
                ? JSON.stringify(runs)
                : describeQueue(runs.queue, `thread ${threadId}`),
            };
          },
        }),
        status: cliCommand({
          summary: "The whole queue on a machine",
          options: {
            machine: {
              type: "string",
              description:
                "Host id, as `bb machine list` prints it (default: this thread's machine, or the only connected one)",
            },
            json,
          },
          async run(input, ctx) {
            const hostId = await resolveHost(input.options.machine, ctx.threadId);
            const status = await hostStatus(hostId, null, 0);
            if (status.kind !== "ok") failure(status);
            return {
              exitCode: 0,
              stdout: input.options.json
                ? JSON.stringify({ hostId, binary: status.binary, ...status.queue })
                : describeQueue(status.queue, `machine ${hostId}`),
            };
          },
        }),
        cancel: cliCommand({
          summary: "Cancel one of a thread's running or waiting runs",
          positionals: [
            {
              name: "run-id",
              description: "The run id, as `bb drmanhattan runs` prints it",
              required: true,
            },
          ],
          options: {
            thread: {
              type: "string",
              description: "Thread id (default: the thread this runs in)",
            },
            json,
          },
          async run(input, ctx) {
            const runId = input.positionals["run-id"];
            const threadId = input.options.thread ?? ctx.threadId;
            if (threadId === undefined) {
              throw new PluginCliError("Which thread?", {
                code: "thread_required",
                hint: "Add --thread <thread-id>.",
              });
            }
            if (!RUN_ID_PATTERN.test(runId)) {
              throw new PluginCliError(`Not a run id: ${runId}`, {
                code: "invalid_run_id",
              });
            }
            const result = await cancelThreadRun(threadId, runId);
            return {
              exitCode: result.ok ? 0 : 1,
              ...(input.options.json
                ? { stdout: JSON.stringify(result) }
                : result.ok
                  ? { stdout: result.message }
                  : { stderr: result.message }),
            };
          },
        }),
      },
    }),
  );

  let probeAll = true;
  const probeHosts = new Set<string>();
  let wake: (() => void) | null = null;

  function requestProbe(hostId?: string): void {
    if (hostId === undefined) probeAll = true;
    else probeHosts.add(hostId);
    wake?.();
  }

  async function probe(signal: AbortSignal): Promise<boolean> {
    let failed = false;
    try {
      const hosts = await bb.sdk.hosts.list({ signal });
      const all = probeAll;
      const wanted = new Set(probeHosts);
      probeAll = false;
      probeHosts.clear();
      await Promise.all(
        hosts
          .filter(
            (candidate) =>
              candidate.status === "connected" &&
              (all || wanted.has(candidate.id)),
          )
          .map(async (candidate) => {
            try {
              await hostStatus(candidate.id, null, 0, signal);
            } catch (error) {
              if (signal.aborted) return;
              failed = true;
              bb.log.warn(
                `Could not look for drmanhattan on ${candidate.id}: ${errorText(error)}`,
              );
            }
          }),
      );
    } catch (error) {
      if (!signal.aborted) {
        failed = true;
        bb.log.warn(`Could not list machines: ${errorText(error)}`);
      }
    }
    return failed;
  }

  function sleep(ms: number, signal: AbortSignal): Promise<void> {
    if (probeAll || probeHosts.size > 0) return Promise.resolve();
    return new Promise((resolve) => {
      const done = () => {
        clearTimeout(timer);
        signal.removeEventListener("abort", done);
        if (wake === done) wake = null;
        resolve();
      };
      const timer = setTimeout(() => {
        probeAll = true;
        done();
      }, ms);
      wake = done;
      signal.addEventListener("abort", done, { once: true });
    });
  }

  bb.background.service("probe", {
    async start(signal) {
      const unsubscribe = bb.sdk.subscribe({
        event: "host:changed",
        callback: (event) => {
          if (event.changes.includes("host-connected")) requestProbe(event.id);
        },
      });
      try {
        while (!signal.aborted) {
          const failed = await probe(signal);
          await sleep(failed ? RETRY_MS : PROBE_INTERVAL_MS, signal);
        }
      } finally {
        unsubscribe();
        wake?.();
        wake = null;
      }
    },
  });
}
