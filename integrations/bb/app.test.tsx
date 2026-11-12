// @vitest-environment jsdom
import { cleanup, fireEvent } from "@testing-library/react";
import { loadPluginApp, renderSlot } from "@get-bb/plugin-sdk/testing/app";
import { afterEach, describe, expect, it } from "vitest";
import { drmanhattanRpcContract, type MachineQueue, type ThreadRuns } from "./contract.js";
import { makeQueue } from "./fixtures.js";
import { ATOM, InfoRuns, PAGE_PATH, QueuePage, SegmentDetails, SegmentSummary } from "./app.js";

afterEach(cleanup);

const ok: ThreadRuns = { kind: "ok", hostId: "mac", queue: makeQueue() };
const threadProps = { threadId: "thr_abc", projectId: "proj_1", environment: { id: "env_1" } };
const segmentProps = {
  ...threadProps,
  segments: [{ key: "drmanhattan/runs", pluginId: "drmanhattan" }],
  isExpanded: true,
  onToggle: () => {},
};

const machines: MachineQueue[] = [
  { hostId: "mac", name: "MacBook", status: { kind: "ok", binary: "/x/drmanhattan", queue: makeQueue() } },
  { hostId: "linux", name: "devbox", status: { kind: "missing", binary: null, message: "not installed" } },
];

function rpc(runs: ThreadRuns, cancelled: string[] = [], queues: MachineQueue[] = machines, setups: string[] = []) {
  return {
    experimental_rpcContract: drmanhattanRpcContract,
    rpc: {
      threadRuns: () => runs,
      cancelRun: (input: unknown) => {
        cancelled.push((input as { runId: string }).runId);
        return { ok: true, message: "cancelled" };
      },
      machines: () => ({ machines: queues }),
      setupMachine: (input: unknown) => {
        const { hostId, action } = input as { hostId: string; action: string };
        setups.push(`${hostId}/${action}`);
        return { ok: true, message: "drmanhattan 0.10.1 is running." };
      },
      cancelMachineRun: (input: unknown) => {
        const { hostId, runId } = input as { hostId: string; runId: string };
        cancelled.push(`${hostId}/${runId}`);
        return { ok: true, message: "cancelled" };
      },
    },
    openUrl: () => true,
  };
}

describe("app", () => {
  it("registers a context segment, an Info section and a page in bb's navigation", async () => {
    const app = await loadPluginApp(() => import("./app.js"));
    expect(app.navPanels.map((panel) => [panel.title, panel.path, panel.icon])).toEqual([["drmanhattan", PAGE_PATH, ATOM]]);
    expect(app.threadContextSegments.map((segment) => segment.id)).toEqual(["runs"]);
    expect(app.threadInfoSections.map((section) => section.id)).toEqual(["runs"]);
  });

  it("sums up the thread's runs beside the composer", async () => {
    const slot = renderSlot({ component: SegmentSummary }, segmentProps, rpc(ok));
    expect(await slot.findByText("drmanhattan · 1 running · 1 waiting")).toBeTruthy();
  });

  it("lists active runs and cancels one", async () => {
    const cancelled: string[] = [];
    const slot = renderSlot({ component: SegmentDetails }, segmentProps, rpc(ok, cancelled));
    expect(await slot.findByText("slot 1 · 1m 5s of ~4m 0s")).toBeTruthy();
    expect(slot.getByText("#2 in line · waited 12s")).toBeTruthy();
    fireEvent.click(slot.getByRole("button", { name: "Cancel npm run build" }));
    await expect.poll(() => cancelled).toEqual(["1793647590-31236-aa01"]);
    fireEvent.click(slot.getByRole("button", { name: "cargo test" }));
    expect(slot.inspection.navigateCalls).toContainEqual(
      expect.objectContaining({ url: "http://127.0.0.1:8417/#run=1793647588-31235-3799f0" }),
    );
  });

  it("shows the thread's history in the Info tab", async () => {
    const slot = renderSlot({ component: InfoRuns }, threadProps, rpc(ok));
    expect(await slot.findByText("cargo clippy")).toBeTruthy();
    expect(slot.getByText("failed · exit 101 · 41s")).toBeTruthy();
  });

  it("shows nothing on a machine without drmanhattan", async () => {
    const slot = renderSlot(
      { component: InfoRuns },
      threadProps,
      rpc({ kind: "missing", message: "not installed" }),
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(slot.container.textContent).toBe("");
  });
});

describe("the drmanhattan page", () => {
  const pageProps = { subPath: "" };

  it("shows each machine's queue, links runs to their threads, and cancels", async () => {
    const cancelled: string[] = [];
    const slot = renderSlot({ component: QueuePage }, pageProps, rpc(ok, cancelled));
    expect(await slot.findByText("MacBook")).toBeTruthy();
    expect(slot.getByText("1 of 1 slots in use · drmanhattan 0.8.0")).toBeTruthy();
    expect(slot.getByText("Running · 1")).toBeTruthy();
    expect(slot.getByText("Finished · 1")).toBeTruthy();
    expect(slot.getByRole("region", { name: "Set up drmanhattan on devbox" })).toBeTruthy();
    fireEvent.click(slot.getByRole("button", { name: "Cancel npm run build" }));
    await expect.poll(() => cancelled).toEqual(["mac/1793647590-31236-aa01"]);
    expect(slot.queryByRole("button", { name: "Cancel cargo clippy" })).toBeNull();
    fireEvent.click(slot.getByRole("button", { name: "Open the thread that queued cargo test" }));
    expect(slot.inspection.navigateCalls).toContainEqual({ method: "toThread", threadId: "thr_abc" });
    fireEvent.click(slot.getByRole("button", { name: /Open the dashboard/ }));
    expect(slot.inspection.navigateCalls).toContainEqual(expect.objectContaining({ url: "http://127.0.0.1:8417/" }));
  });

  it("offers to install drmanhattan on a machine without it, showing what that runs", async () => {
    const setups: string[] = [];
    const slot = renderSlot({ component: QueuePage }, pageProps, rpc(ok, [], [machines[1]!], setups));
    expect(await slot.findByText("Set up drmanhattan on this machine")).toBeTruthy();
    expect(slot.container.querySelector("pre")?.textContent).toBe(
      "curl -fsSL https://github.com/gagansingh/drmanhattan/releases/latest/download/install.sh | sh\ndrmanhattan --yes &",
    );
    fireEvent.click(slot.getByRole("button", { name: "Install and start" }));
    await expect.poll(() => setups).toEqual(["linux/install"]);
    expect(await slot.findByText("drmanhattan 0.10.1 is running.")).toBeTruthy();
  });

  it("offers to start a stopped queue", async () => {
    const setups: string[] = [];
    const stopped: MachineQueue = {
      hostId: "mac",
      name: "MacBook",
      status: { kind: "ok", binary: "/x/drmanhattan", queue: makeQueue({ state: "stopped", running: [], waiting: [] }) },
    };
    const slot = renderSlot({ component: QueuePage }, pageProps, rpc(ok, [], [stopped], setups));
    fireEvent.click(await slot.findByRole("button", { name: "Start drmanhattan" }));
    await expect.poll(() => setups).toEqual(["mac/start"]);
  });
});
