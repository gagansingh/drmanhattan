import { useEffect, useState } from "react";
import {
  definePluginApp,
  experimental_Icon as Icon,
  experimental_InfoList as InfoList,
  experimental_InfoListRow as InfoListRow,
  experimental_InfoSection as InfoSection,
  experimental_THREAD_INFO_SECTION_GROUPS as INFO_SECTION_GROUPS,
  experimental_useRpcMutation as useRpcMutation,
  experimental_useRpcQuery as useRpcQuery,
  useBbNavigate,
  type ExperimentalThreadContextSegmentInput,
  type ExperimentalThreadContextSegmentProps,
  type ExperimentalThreadInfoProps,
} from "@get-bb/plugin-sdk/app";
import { drmanhattanRpcContract, type Queue, type Run } from "./contract.js";
import { isActive, runDetail, runUrl } from "./format.js";

const BUSY_POLL_MS = 3_000;
const IDLE_POLL_MS = 20_000;
const SEGMENT_GROUP = "4_drmanhattan";

/** This thread's runs, read again every few seconds while any is running or waiting. */
export function useThreadRuns(threadId: string) {
  const query = useRpcQuery({
    contract: drmanhattanRpcContract,
    method: "threadRuns",
    input: { threadId },
  });
  const queue = query.data?.kind === "ok" ? query.data.queue : null;
  const busy = queue !== null && queue.running.length + queue.waiting.length > 0;
  const { refetch } = query;
  useEffect(() => {
    const timer = setInterval(
      () => {
        if (typeof document === "undefined" || !document.hidden) void refetch();
      },
      busy ? BUSY_POLL_MS : IDLE_POLL_MS,
    );
    return () => clearInterval(timer);
  }, [busy, refetch]);
  return { queue, result: query.data, refetch };
}

function activeRuns(queue: Queue | null): Run[] {
  return queue === null ? [] : [...queue.running, ...queue.waiting];
}

function stateIcon(run: Run): { name: string; label: string; className: string } {
  if (run.state === "waiting") return { name: "Clock", label: "Waiting", className: "text-muted-foreground" };
  if (isActive(run)) return { name: "Spinner", label: "Running", className: "animate-spin text-muted-foreground" };
  if (run.state === "succeeded") return { name: "CircleCheck", label: "Succeeded", className: "text-success" };
  if (run.state === "cancelled") return { name: "Square", label: "Cancelled", className: "text-muted-foreground" };
  return { name: "CircleX", label: "Failed", className: "text-destructive" };
}

function summaryText(queue: Queue): string {
  const parts = [];
  if (queue.running.length > 0) parts.push(`${queue.running.length} running`);
  if (queue.waiting.length > 0) parts.push(`${queue.waiting.length} waiting`);
  return parts.join(" · ");
}

export function useSegmentVisible({ threadId }: ExperimentalThreadContextSegmentInput): boolean {
  return activeRuns(useThreadRuns(threadId).queue).length > 0;
}

export function SegmentSummary({ threadId, isExpanded, onToggle }: ExperimentalThreadContextSegmentProps) {
  const { queue } = useThreadRuns(threadId);
  if (queue === null) return null;
  const text = summaryText(queue);
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-expanded={isExpanded}
      aria-label={`drmanhattan: ${text}`}
      className="inline-flex min-w-0 items-center gap-1.5 rounded px-1 text-xs text-muted-foreground hover:text-foreground"
    >
      <Icon name="Layers" className="size-3.5 shrink-0" aria-hidden />
      <span className="truncate">drmanhattan · {text}</span>
    </button>
  );
}

function CancelButton({ threadId, run, onDone }: { threadId: string; run: Run; onDone: () => void }) {
  const cancel = useRpcMutation({ contract: drmanhattanRpcContract, method: "cancelRun" });
  const [problem, setProblem] = useState<string | null>(null);
  return (
    <>
      {problem === null ? null : (
        <span role="alert" className="text-xs text-destructive">
          {problem}
        </span>
      )}
      <button
        type="button"
        disabled={cancel.isPending}
        aria-label={`Cancel ${run.label}`}
        title="Cancel this run"
        className="rounded p-0.5 text-muted-foreground hover:text-foreground disabled:opacity-50"
        onClick={() => {
          setProblem(null);
          cancel.mutate({ threadId, runId: run.runId }).then(
            (result) => {
              if (!result.ok) setProblem(result.message);
              onDone();
            },
            (error: unknown) => setProblem(error instanceof Error ? error.message : String(error)),
          );
        }}
      >
        <Icon name="Square" className="size-3.5" aria-hidden />
      </button>
    </>
  );
}

export function SegmentDetails({ threadId, isExpanded }: ExperimentalThreadContextSegmentProps) {
  const { queue, refetch } = useThreadRuns(threadId);
  const navigate = useBbNavigate();
  if (!isExpanded || queue === null) return null;
  const dashboard = queue.dashboardUrl;
  return (
    <div className="max-h-40 overflow-y-auto px-3 pb-2 pt-1 text-xs">
      <ul className="space-y-1">
        {activeRuns(queue).map((run) => {
          const icon = stateIcon(run);
          return (
            <li key={run.runId} className="flex items-center gap-2">
              <Icon name={icon.name} aria-label={icon.label} className={`size-3.5 shrink-0 ${icon.className}`} />
              {dashboard === null ? (
                <span className="min-w-0 truncate">{run.label}</span>
              ) : (
                <button
                  type="button"
                  className="min-w-0 truncate text-left hover:underline"
                  title="Open its log on the drmanhattan dashboard"
                  onClick={() => navigate.openUrl(runUrl(dashboard, run.runId))}
                >
                  {run.label}
                </button>
              )}
              <span className="ml-auto shrink-0 text-muted-foreground">{runDetail(run)}</span>
              <CancelButton threadId={threadId} run={run} onDone={() => void refetch()} />
            </li>
          );
        })}
      </ul>
      <p className="mt-1.5 text-muted-foreground">
        {queue.active} of {queue.capacity} slots in use on this machine
        {dashboard === null ? null : (
          <>
            {" · "}
            <button type="button" className="hover:underline" onClick={() => navigate.openUrl(dashboard)}>
              Open the dashboard
            </button>
          </>
        )}
      </p>
    </div>
  );
}

export function InfoRuns({ threadId }: ExperimentalThreadInfoProps) {
  const { queue, result } = useThreadRuns(threadId);
  const navigate = useBbNavigate();
  if (result?.kind === "outdated") {
    return (
      <InfoSection label="drmanhattan">
        <p className="px-1 text-xs text-muted-foreground">{result.message}</p>
      </InfoSection>
    );
  }
  if (queue === null) return null;
  const runs = [...activeRuns(queue), ...queue.finished];
  if (runs.length === 0) return null;
  const dashboard = queue.dashboardUrl;
  return (
    <InfoSection label="drmanhattan" count={runs.length}>
      <InfoList
        items={runs}
        getKey={(run) => run.runId}
        limit={5}
        renderItem={(run) => {
          const icon = stateIcon(run);
          return (
            <InfoListRow
              leading={<Icon name={icon.name} className={`size-3 ${icon.className}`} aria-hidden />}
              leadingLabel={icon.label}
              name={run.label}
              title={run.errorLine ?? run.label}
              context={runDetail(run)}
              {...(dashboard === null ? {} : { onSelect: () => navigate.openUrl(runUrl(dashboard, run.runId)) })}
            />
          );
        }}
      />
    </InfoSection>
  );
}

export default definePluginApp((app) => {
  app.slots.experimental_threadContextSegment({
    id: "runs",
    title: "drmanhattan runs",
    group: SEGMENT_GROUP,
    summary: SegmentSummary,
    details: SegmentDetails,
    useVisible: useSegmentVisible,
  });
  app.slots.experimental_threadInfoSection({
    id: "runs",
    group: INFO_SECTION_GROUPS.related,
    order: 50,
    component: InfoRuns,
  });
});
