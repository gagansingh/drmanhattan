import { defineRpcContract } from "@get-bb/plugin-sdk";
import { z } from "zod";

export const RUN_ID_PATTERN = /^[0-9A-Za-z][0-9A-Za-z._-]{0,199}$/;
export const SESSION_PATTERN = /^[0-9A-Za-z][0-9A-Za-z_-]{0,127}$/;
export const MAX_FINISHED = 50;

export const runSchema = z
  .object({
    runId: z.string(),
    label: z.string(),
    category: z.string(),
    state: z.string(),
    service: z.boolean(),
    quick: z.boolean(),
    slot: z.number().int().nullable(),
    position: z.number().int().nullable(),
    machine: z.string().nullable(),
    cwd: z.string().nullable(),
    startedAt: z.string().nullable(),
    finishedAt: z.string().nullable(),
    elapsedSeconds: z.number().nullable(),
    waitedSeconds: z.number().nullable(),
    expectedSeconds: z.number().nullable(),
    durationSeconds: z.number().nullable(),
    exit: z.number().int().nullable(),
    errorLine: z.string().nullable(),
    log: z.string().nullable(),
    /** The bb thread that queued it, when one did. */
    threadId: z.string().nullable(),
  })
  .strict();
export type Run = z.infer<typeof runSchema>;

export const queueSchema = z
  .object({
    version: z.string(),
    state: z.string(),
    dashboardUrl: z.string().nullable(),
    active: z.number().int(),
    capacity: z.number().int(),
    running: z.array(runSchema),
    waiting: z.array(runSchema),
    finished: z.array(runSchema),
  })
  .strict();
export type Queue = z.infer<typeof queueSchema>;

export const hostStatusSchema = z.discriminatedUnion("kind", [
  z
    .object({
      kind: z.literal("ok"),
      binary: z.string(),
      queue: queueSchema,
    })
    .strict(),
  z
    .object({
      kind: z.enum(["missing", "outdated", "error"]),
      binary: z.string().nullable(),
      message: z.string(),
    })
    .strict(),
]);
export type HostStatus = z.infer<typeof hostStatusSchema>;

export const drmanhattanHostContract = defineRpcContract({
  status: {
    input: z
      .object({
        session: z.string().regex(SESSION_PATTERN).nullable(),
        limit: z.number().int().min(0).max(MAX_FINISHED),
      })
      .strict(),
    output: hostStatusSchema,
    experimental_retryable: true,
  },
  cancel: {
    input: z.object({ runId: z.string().regex(RUN_ID_PATTERN) }).strict(),
    output: z.object({ ok: z.boolean(), message: z.string() }).strict(),
  },
  stopServices: {
    input: z.object({ session: z.string().regex(SESSION_PATTERN) }).strict(),
    output: z
      .object({
        ok: z.boolean(),
        /** The labels of the services it stopped. */
        stopped: z.array(z.string()),
        message: z.string(),
      })
      .strict(),
  },
});

export const threadRunsSchema = z.discriminatedUnion("kind", [
  z
    .object({
      kind: z.literal("ok"),
      hostId: z.string(),
      queue: queueSchema,
    })
    .strict(),
  z
    .object({
      kind: z.enum(["missing", "outdated", "error", "no-host"]),
      message: z.string(),
    })
    .strict(),
]);
export type ThreadRuns = z.infer<typeof threadRunsSchema>;

export const machineQueueSchema = z
  .object({
    hostId: z.string(),
    name: z.string(),
    status: hostStatusSchema,
  })
  .strict();
export type MachineQueue = z.infer<typeof machineQueueSchema>;

export const drmanhattanRpcContract = defineRpcContract({
  threadRuns: {
    input: z.object({ threadId: z.string().min(1).max(256) }).strict(),
    output: threadRunsSchema,
  },
  cancelRun: {
    input: z
      .object({
        threadId: z.string().min(1).max(256),
        runId: z.string().regex(RUN_ID_PATTERN),
      })
      .strict(),
    output: z.object({ ok: z.boolean(), message: z.string() }).strict(),
  },
  machines: {
    input: z.object({}).strict(),
    output: z.object({ machines: z.array(machineQueueSchema) }).strict(),
  },
  cancelMachineRun: {
    input: z
      .object({
        hostId: z.string().min(1).max(256),
        runId: z.string().regex(RUN_ID_PATTERN),
      })
      .strict(),
    output: z.object({ ok: z.boolean(), message: z.string() }).strict(),
  },
});
