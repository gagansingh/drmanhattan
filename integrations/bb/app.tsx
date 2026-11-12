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
  type PluginNavPanelProps,
  type ExperimentalThreadContextSegmentInput,
  type ExperimentalThreadContextSegmentProps,
  type ExperimentalThreadInfoProps,
} from "@get-bb/plugin-sdk/app";
import {
  drmanhattanRpcContract,
  INSTALL_COMMAND,
  SERVICE_DOCS_URL,
  START_COMMAND,
  type MachineQueue,
  type Queue,
  type Run,
  type SetupAction,
} from "./contract.js";
import { isActive, runDetail, runUrl } from "./format.js";

const BUSY_POLL_MS = 3_000;
const IDLE_POLL_MS = 20_000;
const SEGMENT_GROUP = "4_drmanhattan";
/** The plugin's atom, declared in package.json's `bb.branding.experimental_icons`. */
export const ATOM = "drmanhattan/atom";
export const PAGE_PATH = "queue";

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
  if (run.state === "cancelled") return { name: "Unavailable", label: "Cancelled", className: "text-muted-foreground" };
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
      <Icon name={ATOM} fallback="Layers" className="size-3.5 shrink-0" aria-hidden />
      <span className="truncate">drmanhattan · {text}</span>
    </button>
  );
}

type CancelResult = { ok: boolean; message: string };

function CancelButton({
  run,
  cancel,
  onDone,
}: {
  run: Run;
  cancel: () => Promise<CancelResult>;
  onDone: () => void;
}) {
  const [pending, setPending] = useState(false);
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
        disabled={pending}
        aria-label={`Cancel ${run.label}`}
        title="Cancel this run"
        className="rounded p-0.5 text-muted-foreground hover:text-foreground disabled:opacity-50"
        onClick={() => {
          setProblem(null);
          setPending(true);
          cancel()
            .then(
              (result) => {
                if (!result.ok) setProblem(result.message);
                onDone();
              },
              (error: unknown) => setProblem(error instanceof Error ? error.message : String(error)),
            )
            .finally(() => setPending(false));
        }}
      >
        <Icon name="X" className="size-3.5" aria-hidden />
      </button>
    </>
  );
}

export function SegmentDetails({ threadId, isExpanded }: ExperimentalThreadContextSegmentProps) {
  const { queue, refetch } = useThreadRuns(threadId);
  const navigate = useBbNavigate();
  const cancel = useRpcMutation({ contract: drmanhattanRpcContract, method: "cancelRun" });
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
              <CancelButton
                run={run}
                cancel={() => cancel.mutate({ threadId, runId: run.runId })}
                onDone={() => void refetch()}
              />
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

/** Every connected machine's queue, read again every few seconds while any is busy. */
export function useMachines() {
  const query = useRpcQuery({ contract: drmanhattanRpcContract, method: "machines", input: {} });
  const machines = query.data?.machines ?? null;
  const busy = (machines ?? []).some(
    (machine) =>
      machine.status.kind === "ok" &&
      machine.status.queue.running.length + machine.status.queue.waiting.length > 0,
  );
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
  return { machines, error: query.error, refetch };
}

function PageRun({
  run,
  hostId,
  dashboard,
  onChanged,
}: {
  run: Run;
  hostId: string;
  dashboard: string | null;
  onChanged: () => void;
}) {
  const navigate = useBbNavigate();
  const cancel = useRpcMutation({ contract: drmanhattanRpcContract, method: "cancelMachineRun" });
  const icon = stateIcon(run);
  return (
    <li className="flex min-h-7 items-center gap-2 text-sm">
      <Icon name={icon.name} aria-label={icon.label} className={`size-3.5 shrink-0 ${icon.className}`} />
      {dashboard === null ? (
        <span className="min-w-0 truncate" title={run.errorLine ?? run.label}>
          {run.label}
        </span>
      ) : (
        <button
          type="button"
          className="min-w-0 truncate text-left hover:underline"
          title={run.errorLine ?? "Open its log on the drmanhattan dashboard"}
          onClick={() => navigate.openUrl(runUrl(dashboard, run.runId))}
        >
          {run.label}
        </button>
      )}
      <span className="ml-auto shrink-0 text-xs text-muted-foreground">{runDetail(run)}</span>
      {run.threadId === null ? null : (
        <button
          type="button"
          aria-label={`Open the thread that queued ${run.label}`}
          title="Open the thread that queued it"
          className="rounded p-0.5 text-muted-foreground hover:text-foreground"
          onClick={() => navigate.toThread(run.threadId ?? "")}
        >
          <Icon name="MessageSquare" className="size-3.5" aria-hidden />
        </button>
      )}
      {isActive(run) ? (
        <CancelButton run={run} cancel={() => cancel.mutate({ hostId, runId: run.runId })} onDone={onChanged} />
      ) : (
        <span className="w-[18px] shrink-0" aria-hidden />
      )}
    </li>
  );
}

function RunGroup({ title, runs, ...rest }: { title: string; runs: Run[]; hostId: string; dashboard: string | null; onChanged: () => void }) {
  if (runs.length === 0) return null;
  return (
    <div className="space-y-1">
      <h3 className="text-xs font-medium text-muted-foreground">
        {title} · {runs.length}
      </h3>
      <ul className="space-y-0.5">
        {runs.map((run) => (
          <PageRun key={run.runId} run={run} {...rest} />
        ))}
      </ul>
    </div>
  );
}

const BUTTON =
  "inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium disabled:opacity-50";
const PRIMARY = `${BUTTON} bg-primary text-primary-foreground hover:bg-primary/90`;
const SECONDARY = `${BUTTON} border border-border text-foreground hover:bg-muted`;

/** Runs one setup action on a machine, and says how it went. */
function SetupButton({
  hostId,
  action,
  label,
  busyLabel,
  onDone,
}: {
  hostId: string;
  action: SetupAction;
  label: string;
  busyLabel: string;
  onDone: () => void;
}) {
  const setup = useRpcMutation({ contract: drmanhattanRpcContract, method: "setupMachine" });
  const [pending, setPending] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; message: string } | null>(null);
  return (
    <>
      <button
        type="button"
        className={PRIMARY}
        disabled={pending}
        onClick={() => {
          setPending(true);
          setResult(null);
          setup
            .mutate({ hostId, action })
            .then(setResult, (error: unknown) =>
              setResult({ ok: false, message: error instanceof Error ? error.message : String(error) }),
            )
            .finally(() => {
              setPending(false);
              onDone();
            });
        }}
      >
        {pending ? <Icon name="Spinner" className="size-3.5 animate-spin" aria-hidden /> : null}
        {pending ? busyLabel : label}
      </button>
      {result === null ? null : (
        <p role={result.ok ? "status" : "alert"} className={`basis-full text-xs ${result.ok ? "text-success" : "text-destructive"}`}>
          {result.message}
        </p>
      )}
    </>
  );
}

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className={SECONDARY}
      onClick={() => {
        void navigator.clipboard?.writeText(text).then(() => setCopied(true), () => {});
      }}
    >
      <Icon name="Copy" className="size-3.5" aria-hidden />
      {copied ? "Copied" : "Copy commands"}
    </button>
  );
}

/**
 * A machine without drmanhattan: what it is, exactly what installing it runs, and a button
 * that does it, or the commands to run by hand.
 */
function SetupCard({ machine, single, onChanged }: { machine: MachineQueue; single: boolean; onChanged: () => void }) {
  const navigate = useBbNavigate();
  const commands = `${INSTALL_COMMAND}\n${START_COMMAND}`;
  return (
    <section className="space-y-3 rounded-lg border border-border p-4" aria-label={`Set up drmanhattan on ${machine.name}`}>
      <div className="flex items-center gap-2">
        <Icon name={ATOM} fallback="Layers" className="size-4 shrink-0" aria-hidden />
        <h2 className="text-sm font-semibold">Set up drmanhattan {single ? "on this machine" : `on ${machine.name}`}</h2>
      </div>
      <p className="text-xs leading-relaxed text-muted-foreground">
        drmanhattan is a free, open-source queue for builds and tests. With it, bb's agents take turns on heavy
        commands instead of all compiling at once, and this page shows what each machine is running. It is not
        installed {single ? "here" : `on ${machine.name}`} yet.
      </p>
      <p className="text-xs leading-relaxed text-muted-foreground">
        Installing runs drmanhattan's official installer, which puts <code>drmanhattan</code> and{" "}
        <code>manhattan</code> in <code>~/.local/bin</code>, then starts the queue in the background:
      </p>
      <pre className="overflow-x-auto rounded-md bg-muted px-3 py-2 text-xs">
        <code>{commands}</code>
      </pre>
      <p className="text-xs leading-relaxed text-muted-foreground">
        Anything placed in its inbox runs as you, so only tools you would let run commands should use it. It runs
        until this machine restarts;{" "}
        <button type="button" className="underline hover:text-foreground" onClick={() => navigate.openUrl(SERVICE_DOCS_URL)}>
          run it as a service
        </button>{" "}
        to start it with your login.
      </p>
      <div className="flex flex-wrap items-center gap-2">
        <SetupButton hostId={machine.hostId} action="install" label="Install and start" busyLabel="Installing…" onDone={onChanged} />
        <CopyButton text={commands} />
      </div>
    </section>
  );
}

function MachineSection({ machine, single, onChanged }: { machine: MachineQueue; single: boolean; onChanged: () => void }) {
  const navigate = useBbNavigate();
  const { status } = machine;
  const heading = single ? "This machine" : machine.name;
  if (status.kind !== "ok") {
    return (
      <section className="space-y-1">
        <h2 className="text-sm font-semibold">{heading}</h2>
        <p className="text-xs text-muted-foreground">{status.message}</p>
      </section>
    );
  }
  const { queue } = status;
  const dashboard = queue.dashboardUrl;
  const idle = queue.running.length + queue.waiting.length === 0;
  const stopped = queue.state === "stopped";
  return (
    <section className="space-y-3">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <h2 className="text-sm font-semibold">{heading}</h2>
        <span className="text-xs text-muted-foreground">
          {stopped
            ? `drmanhattan ${queue.version} is not running`
            : `${queue.active} of ${queue.capacity} slots in use · drmanhattan ${queue.version}`}
        </span>
        {dashboard === null ? null : (
          <button
            type="button"
            className="ml-auto inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
            onClick={() => navigate.openUrl(dashboard)}
          >
            Open the dashboard
            <Icon name="ExternalLink" className="size-3" aria-hidden />
          </button>
        )}
      </div>
      {idle && !stopped ? <p className="text-xs text-muted-foreground">Nothing running or waiting.</p> : null}
      {stopped ? (
        <div className="flex flex-wrap items-center gap-2">
          <p className="text-xs text-muted-foreground">Queued commands wait until it runs again.</p>
          <SetupButton hostId={machine.hostId} action="start" label="Start drmanhattan" busyLabel="Starting…" onDone={onChanged} />
        </div>
      ) : null}
      <RunGroup title="Running" runs={queue.running} hostId={machine.hostId} dashboard={dashboard} onChanged={onChanged} />
      <RunGroup title="Waiting" runs={queue.waiting} hostId={machine.hostId} dashboard={dashboard} onChanged={onChanged} />
      <RunGroup title="Finished" runs={queue.finished} hostId={machine.hostId} dashboard={dashboard} onChanged={onChanged} />
    </section>
  );
}

/** The drmanhattan page in bb's navigation: the queue on every connected machine. */
export function QueuePage(_props: PluginNavPanelProps) {
  const { machines, error, refetch } = useMachines();
  const onChanged = () => void refetch();
  const installed = (machines ?? []).filter((machine) => machine.status.kind !== "missing");
  const missing = (machines ?? []).filter((machine) => machine.status.kind === "missing");
  return (
    <div className="h-full w-full overflow-y-auto">
      <div className="mx-auto w-full max-w-3xl space-y-8 px-4 pb-10 pt-4 md:px-5 md:pt-5">
        <header className="space-y-1">
          <h1 className="text-base font-semibold">drmanhattan</h1>
          <p className="text-xs text-muted-foreground">
            The build and test queue on each machine: what is running, what is waiting, and what finished lately.
          </p>
        </header>
        {error ? <p role="alert" className="text-xs text-destructive">{error.message}</p> : null}
        {machines !== null && machines.length === 0 ? (
          <p className="text-sm text-muted-foreground">No machine is connected to bb.</p>
        ) : null}
        {installed.map((machine) => (
          <MachineSection key={machine.hostId} machine={machine} single={(machines ?? []).length === 1} onChanged={onChanged} />
        ))}
        {missing.map((machine) => (
          <SetupCard key={machine.hostId} machine={machine} single={(machines ?? []).length === 1} onChanged={onChanged} />
        ))}
      </div>
    </div>
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
  app.slots.navPanel({
    id: "queue",
    title: "drmanhattan",
    icon: ATOM,
    path: PAGE_PATH,
    component: QueuePage,
  });
  app.slots.experimental_threadInfoSection({
    id: "runs",
    group: INFO_SECTION_GROUPS.related,
    order: 50,
    component: InfoRuns,
  });
});
