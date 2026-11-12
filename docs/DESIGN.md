# How drmanhattan works

This is for people changing the code. The README and `REFERENCE.md` cover using it. `src/lib.rs` maps the
modules.

## One process

The daemon (`drmanhattan` or `drmanhattan serve`) is one process: it watches the inbox, runs
the scheduler and serves the dashboard. A file lock (`flock`) on `daemon.lock` in the state
directory keeps it to one daemon per user. The kernel releases the lock however the process
exits, so a crash never leaves a stale lock.

The scheduler wakes on a file-system event in the inbox, or every `DRMANHATTAN_POLL_MS`. Each
pass picks up new job files, applies `.cancel` files and admits waiting jobs into free
slots. All the runs live behind one mutex, which is never held across slow work: reading
the inbox, following logs and reading settings happen outside it.

## A job's life

1. **Claim.** A job file that has stopped changing is renamed `<name>.json.claimed`. The
   rename is the claim: it is atomic, and a restart never runs the file twice. An invalid
   file becomes `<name>.json.rejected`, with the reason in `.status.json` and `2` in `.exit`.
2. **Queue.** The job gets a run id, an expected duration and a waiting limit.
   A job with a `prepare` command runs it now, on a thread of its own and outside any
   slot, at most `DRMANHATTAN_MAX_PREPARES` at once. Admission passes over the job until it
   has succeeded; if it fails, the job ends there.
3. **Run.** The command runs in its own process group, with its output going straight into
   `<name>.log`. Cancel and timeout end the whole group. A slot is freed only once every
   process in the group has gone.
4. **Finish.** The record, `.status.json` and finally `.exit` are written. Each write is a
   temporary file and a rename, so a reader never sees half a file.

## Order

A free slot goes to:

1. a priority job, oldest first;
2. a job placed by dragging it on the dashboard, in that order;
3. a job that has waited longer than its limit, oldest first;
4. the job with the shortest expected duration, with jobs that have no estimate last.

A waiter whose `group` already has a run in a slot is passed over, and a newer waiter in a
group cancels the older one when it is queued (`superseded_by`).

A quick waiter (`HistoryIndex::lane`: a command whose runs here nearly always took under 30
seconds) does not wait for a slot. In the same pass, in the same order, it starts beside the
slots while fewer than `DRMANHATTAN_QUICK` quick runs are running, and otherwise takes a free
slot like any waiter. Its lane is decided when it is queued and again for every waiter when
a run finishes or the daemon recovers; the record keeps it with its reason (`quick`,
`lane_reason`). A quick run's `DRMANHATTAN_SLOT` is the lowest number above any slot there can
be (257 up) that no run here holds, and nothing that counts slots counts it.

A light waiter (its job says `light`) is admitted the same way into a lane of its own, of
`DRMANHATTAN_LIGHT` places. The two lanes answer different questions. Quick is about time,
which history knows: a command that ends within seconds cannot hold anything up. Light is
about what a job uses, which history does not record: a job may run for minutes and leave
the machine idle. So quick is inferred and light is declared, and what bounds a wrong
declaration is the lane's size. The lane also closes while the kernel reports memory
pressure (`LightGuard`): on a machine that is swapping, one more process slows every run,
and waiting for a slot costs a light job nothing it would not have paid before the lane
existed. The pressure is read only on a pass where a light job is waiting.

With `DRMANHATTAN_BUSY_CPU`, a slot here past the first takes nothing while the machine is
busy (`BusyGuard`, in `Queue::busy_hold`). History cannot say what a command uses, and what
is free changes by the minute, so this is measured rather than inferred: the CPU's busy
share over the last `DRMANHATTAN_BUSY_SECONDS`, from the sampler's tick reading of that long
ago to one taken on the pass. A run that has just started has used nothing yet, so the slots
past the first also wait until the newest run in a slot here has had that long; three
waiters on an idle machine therefore start a window apart. The first slot is never held,
which is why nothing here can starve the queue, and a worker's slots are that worker's own
business.

Ties go to the earlier arrival. A job's waiting limit is twice its expected duration, but
never less than `DRMANHATTAN_MAX_SKIP_SECONDS`. It is what stops a long job from waiting
forever behind a stream of short ones.

## Estimates

A job's expected duration is the median of past successful runs like it, taken from the
first of these that has history (an index over all of `history.jsonl`, each key keeping its
own last 20 runs, so a build's history survives any number of small commands):

1. same category, label and variant, if there are at least 3 such runs (the last 10);
2. same category and label (the last 10);
3. same category (the last 20).

With none of these there is no estimate.

Once there are at least 20 successful runs that recorded the machine's load, spread across
different loads, a linear fit of duration against load per core scales the estimates for
the current load. The scale is held between 0.6× and 2.5×.

## When the daemon dies

Next to each running job, the daemon starts a guard: the same binary, run as
`drmanhattan __guard <group> <grace ms> <daemon pid>`.

- If the daemon dies in any way, the guard's parent pid changes. The guard then ends the
  job's group, with TERM and then KILL.
- When a job ends normally, the daemon writes `done` to the guard and it exits.
- The guard lives no longer than its job, so it never signals a group id that has been
  reused by another program.

On the next start, recovery:

- queues the waiting jobs again;
- records the jobs that were running as `abandoned`;
- queues again any claimed file that no record accounts for;
- writes any `.exit` that went missing.

If a guard was killed along with the daemon, recovery ends what is left of that group, but
only when it can show the group is the run's. It checks the leader's age, or the members'
ages against the daemon's last heartbeat (`state/heartbeat`). Records identify their claimed
file by device, inode and modification time, so a later job that reuses a name is never
mistaken for an earlier one.

## Files

```text
state (volatile; DRMANHATTAN_STATE_DIR)
    runs/<run id>.json        one record per run
    daemon.lock               the single-instance lock, holding the daemon's pid
    dashboard.json            the running daemon's pid, inbox and dashboard address
    heartbeat                 rewritten every few seconds while the daemon runs
    manual-order              the order set by dragging
data (durable; DRMANHATTAN_DATA_DIR)
    history.jsonl             one line per finished run, which the estimates learn from
    steps.jsonl               one line per finished ::start/::done command
    archive/<YYYY-MM>/        finished runs' records
    config.json               the standing slot count and default progress patterns
    acknowledged.json         when the first start was confirmed
    drmanhattan.log           the daemon's own output, rotated at 5 MB to .1
```

A job's log past `DRMANHATTAN_MAX_LOG_MB` is cut in place: it becomes one note line,
`drmanhattan · the log passed 1024 MB, so its first N bytes were dropped; …`, and the newest
quarter of the cap. N counts every byte dropped by every cut so far, so readers count in
bytes of output rather than file positions. `submit --wait` and the activity scanner keep
their place across a cut, and a reader that fell behind what was kept starts again at the note
(`src/joblog.rs`).

## Services

A job with `service: true` is admitted against its own limit (`DRMANHATTAN_SERVICES`), not the
slots, and holds no slot number. Its runner checks readiness every 250 ms (`runner::ready`: a
TCP connect to the port, or the log against the regex) and records `ready_at`; past
`ready_timeout`, or at `ttl`, it stops the group as a timeout would. `submit --service` waits
for `ready_at` or `.exit`. A restart (from the page) marks the entry and cancels it; when the run
ends, its job is queued again. The first stop signal cancels every service, since none would
ever finish in the grace period.

## Workers

A worker is another machine running its own drmanhattan. The job-file contract is the
protocol: the dispatcher writes a job into the worker's inbox over SSH
(`__dispatch-submit`) and reads its results the way any tool does (`wait`, `follow`,
`status`, `cancel`, run there over SSH). So the worker's own capacity, recovery and
process-group handling apply unchanged.

- Admission places a run on a worker (`queue::admit::place`); the run's record says which
  (`machine`), and its slot number is that worker's.
- Its runner (`runner::remote`) dispatches (`workers::dispatch`), holding the worker's
  dispatch lock: probe and housekeeping (`__dispatch-probe`), `check`, mirror, submit, wait
  for the start. Just before the submit it records `remote_run`, so a restart knows where
  the job went. A failure puts the run back in the queue with `remote_fallback` set.
- Then it follows the worker's log into the local one, counting bytes so a reconnect
  resumes with `follow --from`, until the worker's status says the job is over.
- Recovery leaves a running run with `remote_run` running and starts a runner for it that
  follows it again; one still being handed over (no `remote_run`) is queued again.
- A poller thread asks each worker for `machine-stats` every 15 s (`workers::poll`) and
  keeps the answer, or the reason it is unavailable and until when, in `workers::pool`,
  mirrored to `state/workers/<name>.json`.

## Dashboard

A small HTTP server bound to 127.0.0.1 serves the page and `GET /api/state`, which the page
polls every two seconds. Every request must name a loopback host, which defeats DNS
rebinding. Responses forbid framing. The two writes (`POST /api/queue-order` and
`POST /api/capacity`) also need a loopback `Origin`, an `X-Drmanhattan` header and a JSON
content type.
