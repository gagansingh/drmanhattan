// @vitest-environment jsdom
import { fireEvent } from "@testing-library/react";
import { loadPluginApp, renderSlot } from "@get-bb/plugin-sdk/testing/app";
import { describe, expect, it } from "vitest";
import { drmanhattanRpcContract, type ThreadRuns } from "./contract.js";
import { makeQueue } from "./fixtures.js";
import { InfoRuns, SegmentDetails, SegmentSummary } from "./app.js";

const ok: ThreadRuns = { kind: "ok", hostId: "mac", queue: makeQueue() };
const threadProps = { threadId: "thr_abc", projectId: "proj_1", environment: { id: "env_1" } };
const segmentProps = {
  ...threadProps,
  segments: [{ key: "drmanhattan/runs", pluginId: "drmanhattan" }],
  isExpanded: true,
  onToggle: () => {},
};

function rpc(runs: ThreadRuns, cancelled: string[] = []) {
  return {
    experimental_rpcContract: drmanhattanRpcContract,
    rpc: {
      threadRuns: () => runs,
      cancelRun: (input: unknown) => {
        cancelled.push((input as { runId: string }).runId);
        return { ok: true, message: "cancelled" };
      },
    },
    openUrl: () => true,
  };
}

describe("app", () => {
  it("registers a context segment and an Info section", async () => {
    const app = await loadPluginApp(() => import("./app.js"));
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
