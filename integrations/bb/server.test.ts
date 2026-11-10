import {
  createFakePluginHost,
  makeHostResponse,
  makePluginAgentConfigurationContext,
} from "@get-bb/plugin-sdk/testing";
import { describe, expect, it } from "vitest";
import type { HostStatus } from "./contract.js";
import { makeQueue } from "./fixtures.js";
import plugin, { AGENT_INSTRUCTIONS, SKILL } from "./server.js";

interface Setup {
  status?: (hostId: string, input: { session: string | null; limit: number }) => HostStatus;
  settings?: Record<string, boolean>;
}

async function setup({ status, settings }: Setup = {}) {
  const calls: Array<{ method: string; input: unknown; hostId: string }> = [];
  const host = createFakePluginHost({
    pluginId: "drmanhattan",
    agentSkillIds: [SKILL],
    experimental_hostEntry: true,
    ...(settings === undefined ? {} : { settings }),
    sdk: {
      hosts: {
        list: async () => [
          makeHostResponse({ id: "mac", status: "connected" }),
          makeHostResponse({ id: "linux", status: "connected" }),
        ],
      },
      threads: {
        get: async ({ threadId }: { threadId: string }) => ({ id: threadId, environmentId: "env_1" }),
      },
      environments: {
        get: async () => ({ id: "env_1", hostId: "mac" }),
      },
      subscribe: () => () => {},
    } as never,
    experimental_callHostRpc: ({ method, input, hostId }) => {
      calls.push({ method, input, hostId });
      if (method === "cancel") return { ok: true, message: "state=cancelled" };
      const fallback: HostStatus = { kind: "ok", binary: "/usr/local/bin/drmanhattan", queue: makeQueue() };
      return status?.(hostId, input as { session: string | null; limit: number }) ?? fallback;
    },
  });
  await plugin(host.bb);
  return { ...host, calls };
}

async function probeOnce(harness: Awaited<ReturnType<typeof setup>>["harness"], calls: unknown[]) {
  const service = harness.behavior.runService("probe");
  const deadline = Date.now() + 2_000;
  while (calls.length < 2 && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 5));
  service.controller.abort();
  await service.done;
}

const onHost = (id: string) => makePluginAgentConfigurationContext({ host: { id, name: id } });

describe("agent configuration", () => {
  it("teaches threads on machines with drmanhattan, and leaves the others alone", async () => {
    const { harness, calls } = await setup({
      status: (hostId) =>
        hostId === "linux"
          ? { kind: "missing", binary: null, message: "not installed" }
          : { kind: "ok", binary: "/x/drmanhattan", queue: makeQueue() },
    });
    expect((await harness.behavior.resolveAgentConfiguration(onHost("mac"))).skills).toEqual([]);
    await probeOnce(harness, calls);
    const mac = await harness.behavior.resolveAgentConfiguration(onHost("mac"));
    expect(mac.skills).toEqual([SKILL]);
    expect(mac.instructions).toContain(AGENT_INSTRUCTIONS.split("\n")[0]);
    const linux = await harness.behavior.resolveAgentConfiguration(onHost("linux"));
    expect([linux.skills, linux.instructions]).toEqual([[], null]);
  });

  it("keeps the skill but drops the instructions when the setting is off", async () => {
    const { harness, calls } = await setup({ settings: { agentInstructions: false } });
    await probeOnce(harness, calls);
    const config = await harness.behavior.resolveAgentConfiguration(onHost("mac"));
    expect([config.skills, config.instructions]).toEqual([[SKILL], null]);
  });
});

describe("the thread's runs", () => {
  it("asks the thread's machine for the thread's runs", async () => {
    const { harness, calls } = await setup();
    const result = await harness.behavior.callRpc("threadRuns", { threadId: "thr_abc" });
    expect(result).toMatchObject({ kind: "ok", hostId: "mac" });
    expect(calls).toEqual([{ method: "status", input: { session: "thr_abc", limit: 10 }, hostId: "mac" }]);
  });

  it("prints them for `bb drmanhattan runs`", async () => {
    const { harness } = await setup();
    const out = await harness.behavior.runCli(["runs"], { threadId: "thr_abc" });
    expect(out.exitCode).toBe(0);
    expect(out.stdout).toContain("1793647588-31235-3799f0  running  slot 1 · 1m 5s of ~4m 0s  cargo test");
    expect(out.stdout).toContain("waiting  #2 in line · waited 12s  npm run build");
    expect(out.stdout).toContain("failed · exit 101 · 41s  cargo clippy\n    error: unused variable");
    const json = await harness.behavior.runCli(["runs", "--thread", "thr_abc", "--limit", "3", "--json"]);
    expect(JSON.parse(json.stdout ?? "")).toMatchObject({ kind: "ok", queue: { version: "0.8.0" } });
  });

  it("explains a machine without drmanhattan", async () => {
    const { harness } = await setup({ status: () => ({ kind: "missing", binary: null, message: "drmanhattan is not installed" }) });
    const out = await harness.behavior.runCli(["runs"], { threadId: "thr_abc" });
    expect(out.exitCode).not.toBe(0);
    expect(`${out.stdout ?? ""}${out.stderr ?? ""}`).toContain("install.sh");
  });

  it("cancels only a run of the thread's own", async () => {
    const { harness, calls } = await setup();
    const foreign = await harness.behavior.callRpc("cancelRun", { threadId: "thr_abc", runId: "1-2-other" });
    expect(foreign).toMatchObject({ ok: false });
    expect(calls.some((call) => call.method === "cancel")).toBe(false);
    const own = await harness.behavior.callRpc("cancelRun", { threadId: "thr_abc", runId: "1793647590-31236-aa01" });
    expect(own).toEqual({ ok: true, message: "state=cancelled" });
    expect(calls.at(-1)).toEqual({ method: "cancel", input: { runId: "1793647590-31236-aa01" }, hostId: "mac" });
  });
});
