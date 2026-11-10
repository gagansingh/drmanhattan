# drmanhattan reference

Everything drmanhattan does, in detail. The [README](../README.md) is the short version.

- [Job files](#job-files)
- [Results](#results)
- [Following one run](#following-one-run)
- [Services: servers that keep running](#services-servers-that-keep-running)
- [Preparing while waiting](#preparing-while-waiting)
- [Quick commands](#quick-commands)
- [Light jobs](#light-jobs)
- [One at a time: groups](#one-at-a-time-groups)
- [Failing on output](#failing-on-output)
- [Progress markers](#progress-markers)
- [The jobs' environment and terminal](#the-jobs-environment-and-terminal)
- [Workers: running jobs on other machines](#workers-running-jobs-on-other-machines)
- [Dashboard](#dashboard)
- [bb](#bb)
- [Configuration](#configuration)
- [Running as a service](#running-as-a-service)
- [Updating](#updating)
- [Disk use](#disk-use)
- [Security](#security)
- [Uses](#uses)

## Job files

`submit` is a convenience: any program can queue a job by writing a JSON file into the inbox
(`~/.drmanhattan/inbox` by default). Write it under a temporary name, then rename it, so the
queue never reads half a file:

```
cat > ~/.drmanhattan/inbox/tests.json.tmp <<'EOF'
{"command": ["npm", "test"], "cwd": "~/project", "timeout": 600}
EOF
mv ~/.drmanhattan/inbox/tests.json.tmp ~/.drmanhattan/inbox/tests.json
```

| field | meaning |
| --- | --- |
| `command` | required: a list of arguments, or a string run with `/bin/sh -c` |
| `cwd` | where it runs: absolute, `~/…`, or relative to the inbox (default: the inbox) |
| `label` | its name on the dashboard (default: the file name) |
| `category` | what it is grouped with for time estimates (default: the program's name) |
| `variant` | a finer grouping for estimates, such as `"cold"` for a fresh checkout |
| `timeout` | seconds of running time allowed |
| `priority` | `true` runs it ahead of every job without priority |
| `env` | extra environment variables, as an object of strings |
| `source` | who submitted it, shown on the dashboard: a name, or `{"tool", "session", "url"}` |
| `activity` | patterns for this job's progress lines (see below) |
| `prepare` | a command run while the job waits, outside any slot (see below) |
| `group` | a concurrency group: one of its jobs runs at a time, and only the newest waits (see below) |
| `fail_patterns` | regular expressions that fail the job when a line of its output matches (see below) |
| `tty` | `true` runs it under a pseudo-terminal (see below) |
| `light` | `true`: it leaves the machine mostly free and may start beside the slots (see below) |

Unknown fields are an error, so a typo is reported rather than ignored.

A job runs with `DRMANHATTAN_RUN_ID` (its run id), `DRMANHATTAN_LOG` (its log) and
`DRMANHATTAN_SLOT` (the slot it holds, from 1; for a quick or light run, a number from 257 up)
set, besides its `env`. No two jobs running here at once share a `DRMANHATTAN_SLOT` (a
service gets 0), so it can name a per-slot cache or scratch directory.

## Results

When the queue picks up `tests.json`, it renames it `tests.json.claimed` and writes:

| file | contents |
| --- | --- |
| `tests.log` | the command's output, as it runs |
| `tests.status.json` | the job's state (`queued`, `running`, `succeeded`, `failed`, `timed_out`, `cancelled`, `abandoned` or `rejected`), its times, slot and exit status |
| `tests.exit` | the exit code, written last: once it exists, the job is over |

To cancel a job, queued or running, create an empty `tests.cancel` next to it.

`.exit` holds the command's own exit code, or: 124 timed out; 130 cancelled before it
started; 128 + the signal when cancelled while running; 1 abandoned (the queue stopped while
it ran); 2 rejected (not a valid job; the reason is in `.status.json`).

Results are removed seven days after the job finishes.

## Following one run

The queue's terminal prints a line for each thing that happens, with the run's short id:

```
drmanhattan 0.6.2 running · dashboard http://127.0.0.1:8417/
watching ~/.drmanhattan/inbox · 1 slot

14:52:34  queued     make test   #094a70  make · ~/project
14:52:34  started    make test   #094a70  slot 1 · waited 0s
14:53:20  succeeded  make test   #094a70  46s
```

A run can be named by its run id, that short id, or its job's name:

```
drmanhattan status tests --json          # its record, as tests.status.json has it, plus state and exit
drmanhattan wait tests --until started   # 0 once it has started (or its exit code if it never will)
drmanhattan wait tests                   # until it finishes; exits with its exit code
drmanhattan follow tests                 # its log as it grows; exits with its exit code
drmanhattan follow tests --from 18734    # carry on from 18734 bytes of output in
drmanhattan cancel '#094a70'             # as tests.cancel does
```

One session's runs, as a tool shows them beside the session (bb's plugin does):

```
drmanhattan status --session thr_7f3a9k2m4p --json   # its running and waiting runs, and its 10 newest finished
drmanhattan status --session thr_7f3a9k2m4p --limit 3
```

The session is the one `submit` recorded in the job's `source`. The first line and each
waiter's position still describe the whole queue; `finished` (`finished run_id=… state=…
exit=…` lines in text) lists the session's finished runs, newest first. Without `--session`,
`status --json` gives each running and waiting run's `source` too, and `--limit N` adds the
whole queue's newest N finished runs.

`wait --timeout SECS` gives up with exit 124. `follow` prints the log's bytes unchanged, so
a reader that loses its place (a dropped connection) resumes with `--from` set to the bytes
it has. These read the files, so they work while the queue is stopped too, and `wait` and
`status` know a job file that has not been picked up yet (`submitted`).

## Services: servers that keep running

A dev server or a database never exits, so it cannot wait for a slot or be waited for. Submit it
as a service instead:

```
manhattan submit --service --ready-port 3000 -- npm run dev
manhattan submit --service --ready-log "Listening on" -- ./bin/api
```

- `submit` returns once the service is ready: `--ready-port N` when 127.0.0.1:N accepts a
  connection, `--ready-log RE` when a line of its output matches, at once with neither. It
  prints its name, pid, port and log, and exits 0. Not ready within `--ready-timeout` (120 s),
  or ended first, it is stopped, the end of its log is printed, and `submit` exits non-zero.
- Services take none of the build slots; up to `DRMANHATTAN_SERVICES` (4) run at once, and
  more wait for one to stop.
- A service's name is its label's. Submitting one that is already running returns the running
  one; `--restart` replaces it.
- `manhattan stop <name>` stops it (its whole process group). It is stopped anyway after
  `--ttl` (8 hours), and when the queue stops; a restart of the queue never leaves one behind.
- The dashboard lists services apart, with their port, uptime, time left, and stop and restart
  buttons; their logs open in the same pane.
- In a job file: `"service": true`, `"ready": {"port": 3000}` or `{"log": "…"}`,
  `"ready_timeout"` and `"ttl"` in seconds.

## Preparing while waiting

Some work a job needs can happen before it gets a slot: fetching dependencies, copying
inputs into place. `prepare` is a command (a list, or a string run with `/bin/sh -c`) that
runs as soon as the job is queued, in the job's `cwd` with its `env`, while the job waits:

```json
{"command": ["cargo", "test"], "prepare": "cargo fetch"}
```

- The job takes a slot only once `prepare` has succeeded. A free slot is not held for it:
  jobs behind it start meanwhile.
- If `prepare` fails or times out (it has the job's `timeout` too), the job fails without
  taking a slot, with `prepare`'s exit code. Cancelling the job stops it.
- Its output starts the job's log; the run's output follows. It runs with
  `DRMANHATTAN_PREPARE=1` set.
- At most four run at once (`DRMANHATTAN_MAX_PREPARES`), best-placed jobs first. One cut
  short by the queue stopping runs again on the next start.
- `.status.json` has `prepare_status` (`pending`, `running`, `succeeded` or `failed`), and
  the dashboard marks the job while it prepares. `submit --prepare CMD` sets it.

## Quick commands

Slots keep long jobs from overlapping. A command that has nearly always finished within
seconds here does not need one: it starts at once, beside the slots, like a service. On a
machine where an agent runs everything through the queue, its `git status` and `rg` no
longer wait behind a 20-minute build.

Whether a job is quick is decided from this machine's own history:

1. Its own runs (same label and variant, else same label), when there are at least 3: it is
   quick if 9 of its last 10 took under 30 seconds (all of them, while it has fewer than 10).
   Failed runs count; a cancelled or timed out one counts only if it ran longer than that.
2. Otherwise its category's runs, when there are at least 3: it is quick only if every one
   of the last 20 took under 30 seconds. A new `cargo build` among many quick `cargo fmt`s
   is not.
3. Otherwise a short list of read-only tools (`git`, `gh`, `rg`, `grep`, `ls`, `cat`, `head`,
   `tail`, `wc`, `jq`, `diff`, `which`) is quick until one of their runs here takes longer.
4. Anything else is new here and waits for a slot. After 3 quick runs it is quick too.

A shell script is judged as its first program only when it is plain commands and the rest
are read-only tools: `cd repo && git log | head` is `git`, while `git pull && cargo build`,
or a script with a `$(...)`, a loop or a sourced file, waits for a slot. Services, jobs that
may run on a worker, and jobs sent from another machine are never quick.

- Up to `DRMANHATTAN_QUICK` quick jobs run at once (the number of cores); more wait for a
  quick place, or take a free slot. `DRMANHATTAN_QUICK=0` turns the quick lane off.
- A group still runs one job at a time, quick or not.
- `status` shows `quick=N/M`, `lane=quick` for a quick run, and `lane=` with a `reason=` for
  each waiter; `.status.json` has `quick` and `lane_reason`. The dashboard marks quick runs
  and gives each waiter's reason on hover. A quick run's log starts
  `… started at once (quick)`.

## Light jobs

A slot is for a job that uses the machine: a build, a test suite. Some jobs take minutes
and use almost none of it, because they wait on something else: tests that drive a phone or
an emulator, a deploy that waits on a remote build, a sync over the network. Behind one
slot they queue for nothing. The queue cannot tell such a job from a build, since history
records how long a command took and not what it used, so the job says it itself:

```json
{"command": ["./run-device-tests.sh"], "light": true}
```

- A light job starts beside the slots, in the light lane, while fewer than
  `DRMANHATTAN_LIGHT` light jobs (2) are running there. When the lane is full it waits for
  a place in it, or takes a free slot. `DRMANHATTAN_LIGHT=0` turns the lane off: `light`
  then changes nothing.
- The lane takes nothing while the machine is short of memory: the kernel's memory
  pressure is `warn` or `critical` (macOS; elsewhere there is no reading and the lane stays
  open). Light jobs then take slots in turn like any other.
  `DRMANHATTAN_LIGHT_PRESSURE=critical` closes it only at `critical`, and `off` never. The
  daemon's log says when the lane closes and opens (`light closed`, `light open`).
- Light jobs keep their place in the order: priority, overdue and shortest-first apply
  among them as among the rest. A group still runs one job at a time.
- It is the submitter's word, and the limit is what bounds a wrong one: at most
  `DRMANHATTAN_LIGHT` jobs ever run beside the slots this way. Mark only commands that
  leave the cores and memory free, not ones that are merely short: two builds side by side
  are what slots exist to prevent.
- A job that history shows to be quick is quick, whatever it says. A service, a job that
  may run on a worker, and a job sent from another machine are never light.
- `submit --light` sets it. `status` shows `light=N/M`, `lane=light` for a light run and
  for a waiter, and `light_closed=…` while the lane is closed; `.status.json` has
  `light_lane`. A light run's log starts `… started beside the slots (light)`.

## One at a time: groups

Jobs that must never overlap, and where only the latest request matters, such as deploys
of one service, share a `group`:

```json
{"command": ["./deploy.sh", "staging"], "group": "deploy-staging"}
```

- At most one job of a group runs at a time, whatever the free slots. Jobs outside the
  group use the slots meanwhile.
- At most one waits. A newer job in the group replaces the one waiting, which is
  `cancelled` (exit 130) with `superseded_by` set to the newer run's id in its
  `.status.json`. The running job is never touched.
- `submit --group NAME` sets it; `status` and the dashboard show a waiter held by its group.

## Failing on output

Some commands report a problem in their output but still exit 0: a test suite that skips
itself when a service is missing, a linter that only warns. `fail_patterns` turns such a run
into a failure:

```json
"fail_patterns": ["^Skipped \\d+ suites?", "WARNING: deprecated"]
```

When the command exits 0 and a line of its output matches one of the patterns, the job is
`failed` with exit code 1. The first matching line is its `error_line` in `.status.json`,
and `failure_reason` names the pattern. A run that failed anyway keeps its own exit code.
`submit --fail-pattern RE` (repeatable) sets them from the command line.

## Progress markers

A job can print marker lines to show where it is. The dashboard shows the current step and
each command in flight, with its usual time once it has run before:

```
::step 2/3 Building the server
::start cargo build
::done cargo build
```

When the number of steps is not known, leave it out: `::step 4 Running`.

A job can also put a short badge on its run, which stays on the dashboard and in
`.status.json` (`notes`) after the run ends:

```
::note built on the spare machine
```

Each distinct note is shown once, up to eight per run and 60 characters each.

A tool that prints its own progress can be matched instead: give the job an `activity`
object of regular expressions with named groups (`n`, and optionally `of` and `text`, for the step, `label`
for start and done, `text` for a note).

```json
"activity": {"step": "^\\[(?P<n>\\d+)/(?P<of>\\d+)\\] (?P<text>.*)$"}
```

## The jobs' environment and terminal

A daemon started by launchd or systemd has a bare environment. So when it starts, drmanhattan
asks your login shell for its environment (`$SHELL -ilc`, else zsh, else bash), under a
terminal and with up to 10 seconds to answer, and every job starts from that: the PATH your
shell sets up (Homebrew, nvm, pyenv, `~/.local/bin`) and your exported variables, but not the
shell's own (`SHLVL`, `PWD`, prompts). A job's `env` overrides them. If the shell fails or does
not answer, the daemon log says so once and jobs get the daemon's environment with
`/opt/homebrew/bin`, `/usr/local/bin`, `~/.local/bin` and the system directories at the front
of PATH. `drmanhattan paths` shows which, and when it was read (`shell_env=`);
`DRMANHATTAN_SHELL_ENV=0` turns it off.

It is read once, so a new `export` in your shell files, or an nvm default switched to another
version, reaches jobs only after `drmanhattan reload-env`, which reads it again for the jobs
that start afterwards (running jobs and services keep theirs), or after a restart. If the
shell then fails or does not answer, jobs keep the environment they had and the command says so.

Jobs run with stdin at EOF and their output going to the log, not to a terminal. Input a
command needs goes in a file or an argument, not through a pipe into `submit`:
`manhattan submit -- sh -c 'psql < dump.sql'` or `git commit -F msg.txt`; keep the file under
the job's working directory, which workers mirror by default (`sync` entries replace it).
`submit` warns when input is already waiting in a pipe. For a command that needs a terminal, set `"tty": true` (`submit --tty`): it runs in its own session with a
pseudo-terminal as its controlling terminal, stdout and stderr, 120 columns by 40 rows and
`TERM=xterm-256color`; stdin stays at EOF, and cancel and timeout still end its whole process
group.

## Workers: running jobs on other machines

drmanhattan can hand jobs to other machines over SSH. A worker is just another
drmanhattan: each one runs its own daemon, started and confirmed by a person at that
machine, and takes jobs through its own inbox. Workers add slots; they are never needed.
When one is gone, its jobs run here.

On this machine, `config.json` in the data directory lists them:

```json
{
  "workers": [
    { "name": "air", "ssh": "air", "slots": 1, "tags": ["xcode-27"],
      "check": "pg_isready -q",
      "args_path_map": { "/Volumes/Disk/derived": "~/Library/Caches/derived" } }
  ]
}
```

| field | meaning |
| --- | --- |
| `ssh` | an alias from `~/.ssh/config`, or `user@host`; key-based auth only |
| `name` | its name on the dashboard and in run records (default: the alias) |
| `slots` | how many of its slots this machine may use (default 1); 0 pauses it |
| `tags` | what it offers; a job's `requires` must all be among them |
| `check` | a command run on it before each hand-over: exit 0 means ready, else its first line of output says why not |
| `args_path_map` | argument prefixes rewritten for it, for paths that exist only here |
| `min_free_gb` | free space it must keep on the disk holding its home (default 20) |
| `mirror_days` | its mirrors unused this long are removed (default 7) |
| `drmanhattan` | the command on it, when `drmanhattan` is not on its non-interactive `PATH` |

`drmanhattan workers add <ssh-alias> [--slots N] [--tag T]` checks the machine (it answers
with its key alone, speaks the same worker protocol, confirmed and running) and adds it.
`drmanhattan workers` lists them with their state. Machines work together whatever their
versions as long as their protocols match; the protocol changes only when the hand-off does,
and a worker whose protocol differs is listed with the machine to update.

A job says whether it may go to a worker:

| field | meaning |
| --- | --- |
| `remote` | `"never"` (the default), `"allowed"` (only while every slot here is busy) or `"prefer"` (a free worker before a slot here) |
| `requires` | tags the worker must have |
| `sync` | paths to mirror to the worker, at the same absolute paths: strings, or `{"path": …, "exclude": [rsync patterns]}`; default: the job's `cwd` |
| `sync_git` | `true` also mirrors the git metadata of the synced checkouts, so `HEAD`, branches and commits resolve there |
| `env_forward` | the daemon's environment variables, by name or `PREFIX_*`, that go too; the job's `env` always does |

`submit` takes `--remote`, `--requires`, `--sync` and `--sync-git`.

What happens:

- **Admission.** The order is the usual one. A worker slot goes to the best-placed waiter
  that may use it: `remote` allows it, the worker has its tags, it is not paused, and it is
  not out of admission after a failure.
- **Hand-over.** Dispatches to one worker go one at a time. This machine reaches the
  worker, checks its protocol, that it holds fewer of its own slots than this machine may
  use, its free disk and its `check`; mirrors the `sync` paths with `rsync -a --delete`
  (leaving out what git ignores there, `.git` itself and `.DS_Store`; the worker keeps its
  own copies of those); writes the job into the worker's inbox; and waits up to two minutes
  for it to start. Paths must lie inside the home directory, and both machines must use the
  same user and home path. A job's `prepare` runs here while it waits and again on the worker
  before its command.
- **Fallback.** If any of that fails, the job runs here, with one line in its log
  (`worker air unavailable: <reason>; running here`) and `remote_fallback` in its status. A
  failure of the worker's keeps it out of admission for two minutes, with the reason on the
  dashboard.
- **While it runs.** The worker's log streams into the job's own log here, so `follow`,
  `wait`, the dashboard and the error line work as usual. Cancel and timeout here stop it
  there. A dropped connection is retried every five seconds; a worker silent for three
  minutes fails the run with `worker lost`. The worker also gets the job's timeout plus two
  minutes, in case this machine is gone. Stopping or restarting this machine's daemon leaves
  the job running there, and the next start follows it again.
- **Results.** The job's `.status.json` here has `machine` (the worker) and `remote_run`
  (its job there). Estimates are kept per machine.
- **Housekeeping.** The worker records each mirror it receives. On each hand-over it
  removes the mirrors this machine no longer has and those unused for `mirror_days`, never
  one a job there is using, and never a path it had before any mirror arrived.

The dashboard then shows the machines as a tree. Each machine is a node that folds away,
with whether it answers, its slots with − and + (down to 0, which pauses a worker), its CPU,
load, memory, temperature and battery, and why it is not taking work while that is so; under it, one
leaf per slot, holding its running job or free. `DRMANHATTAN_SSH` and `DRMANHATTAN_RSYNC` name other `ssh` and `rsync` programs; `DRMANHATTAN_WORKER_POLL_SECONDS`
(15) sets how often each worker is asked for its readings.

## Dashboard

`http://127.0.0.1:8417/` (`drmanhattan paths` prints it if that port was busy) shows each
slot's job with a progress bar against its usual time, quick runs beside them, the waiting
jobs with estimated start times, recent results with their logs, and the machine's CPU,
memory, temperature and battery (amber at 20% on battery, when it says to plug in). The − and + buttons set the number of slots, as
`drmanhattan slots N` does, and it stays until changed (unless `DRMANHATTAN_SLOTS` is set
for the daemon, which then decides); drag a waiting job to reorder it.

Finished runs are listed 20 at a time, newest first, with "‹ newer" and "older ›" to step
through every run still kept (the newest 400, for at most `DRMANHATTAN_LOG_RETENTION_DAYS`). The filters (all, failed,
passed, cancelled) and the search box (a run's name, command, folder or worker) count and page
through all of them, not only the runs on screen. The newest page follows runs as they finish,
except while the pointer is over the section, the keyboard is in it, or one of its runs' logs is
open: then it holds still, and "↑ N new" shows the runs that finished meanwhile. Older pages
always hold still. `/api/state?filter=failed&search=cargo&from=<run id>` asks for such a page;
`finished` says where it is.

Commands are shown as a person reads them: the program and its subcommand stand out, wrappers
(`env`, `xcrun`, `nice`, `time`, a script that runs `-- <command>`) are dimmed or folded into a
`via` tag, the job's folder is `.` and the home folder `~`, and flags, settings, quoted strings and
shell operators each have a colour; hover a job's name to see its command.

A run's source links back to the session that submitted it: a bb thread, a Claude Code
session, a Codex thread, or the folder in Cursor or VS Code. `http://127.0.0.1:8417/#run=<run
id>` opens the page with that run's log showing.

## bb

[bb](https://github.com/get-bb/bb) is an editor that runs coding agents in threads. A run
submitted from a bb thread records the thread (bb gives each thread's shell `BB_THREAD_ID`
and `BB_SERVER_URL`), and the dashboard shows it as `bb thread <id>`, linking to the thread.
That holds even when the thread's agent is Claude Code or Codex, whose own markers are also
set there.

bb's plugin for drmanhattan, in [`integrations/bb`](../integrations/bb), goes further:

- Threads on a machine with drmanhattan 0.8.0 or later are told to run builds and tests
  with `manhattan submit --wait`, and get a `drmanhattan` skill. A setting on the plugin's page
  turns the instructions off; machines without drmanhattan are left alone.
- While a thread has runs going, the row above its composer says how many are running and
  waiting. Expanded, it lists them with their slot, place in line and time against their usual
  time, a button to cancel each, and a link to its log on the dashboard. The thread's Info tab
  lists its recent runs, passed or failed.
- A drmanhattan page in bb's navigation shows the queue on every connected machine:
  slots in use, running, waiting and the 15 newest finished runs (drmanhattan 0.9.0 or
  later lists those), each with a link to its log, the thread that queued it and, while
  active, a button to cancel it. It also opens the dashboard, which stays at its own address:
  it refuses to be embedded in another page.
- Archiving or deleting a thread stops the services it started (`--service`: dev servers,
  databases) on every machine. A setting on the plugin's page turns this off.
- `bb drmanhattan runs` lists a thread's runs, `bb drmanhattan status` the whole queue on a
  machine, and `bb drmanhattan cancel <run id>` cancels one of the thread's runs; `--json`
  for each.

Install it with:

```
drmanhattan bb install
```

which runs `bb plugin install git:https://github.com/gagansingh/drmanhattan.git@0.x --plugin
drmanhattan` (bb asks before installing; `drmanhattan bb` prints the command). bb installs
the newest release tag and `bb plugin update` follows later releases of the same major
version. The `bb` command must be on `PATH`: in bb, Settings → General → Command line →
Install. The plugin calls the `drmanhattan` it finds on `PATH`, in `~/.local/bin` or
`~/.cargo/bin` (`DRMANHATTAN_BIN` names another) on the thread's machine, so each machine
with threads needs its own drmanhattan running.

## On your phone

`drmanhattan lan on` serves the dashboard on this machine's network address too, and prints a
link to open on a phone on the same Wi-Fi. The link holds a secret key; opening it once leaves
a cookie on the phone and drops the key from the address bar. Without the key the network
address answers only 403. The phone can do everything the page can (reorder, slots, stop a
service). The running daemon picks the change up within seconds, without a restart, and
follows the machine to a new network address. `drmanhattan lan` shows the link again,
`drmanhattan lan new-key` makes the old link and every phone's cookie stop working, and
`drmanhattan lan off` stops it.

It is plain HTTP: anyone who can watch the network's traffic (an untrusted or open Wi-Fi) can
read the pages and the key. Use it on a network you trust, and keep it off elsewhere.

## Configuration

Commands: `drmanhattan [serve]`, `submit`, `status [<run>] [--json]`, `wait`, `follow`,
`cancel`, `workers`, `machine-stats`, `update [--now]`, `reload-env`, `stop [<name>]`, `slots [N]`, `lan [on|off|new-key]`,
`paths` and `bb [install]`. Each has `--help`.

| option | default | meaning |
| --- | --- | --- |
| `--inbox DIR`, `DRMANHATTAN_INBOX` | `~/.drmanhattan/inbox` | the watched folder |
| `--port N` | 8417 | the dashboard's port |
| `--bind ADDR` | 127.0.0.1 | the dashboard's address |
| `--no-dashboard` | | run without the dashboard |
| `--open` | | open the dashboard in the browser |
| `--no-session-links` | | do not read Claude Code or Codex session files to link runs to them |
| `--yes`, `DRMANHATTAN_ACCEPT=1` | | confirm the first start without asking |
| `DRMANHATTAN_SLOTS` | 1 | long jobs that may run at once, up to 256 (also `drmanhattan slots N`) |
| `DRMANHATTAN_QUICK` | cores | quick jobs that may run at once, beside the slots; `0` turns them off |
| `DRMANHATTAN_LIGHT` | 2 | jobs that say `light` that may run at once, beside the slots; `0` turns them off |
| `DRMANHATTAN_LIGHT_PRESSURE` | warn | the memory pressure that closes the light lane: `warn`, `critical` or `off` |
| `DRMANHATTAN_SHELL_ENV` | 1 | `0`: jobs get the daemon's environment, not your login shell's |
| `DRMANHATTAN_SHELL_ENV_TIMEOUT_SECONDS` | 10 | how long the login shell may take to print its environment |
| `DRMANHATTAN_SERVICES` | 4 | services that may run at once (they take no slots) |
| `DRMANHATTAN_MAX_PREPARES` | 4 | `prepare` commands that may run at once |
| `DRMANHATTAN_SHUTDOWN_GRACE_SECONDS` | 30 | how long `stop` lets running jobs finish |
| `DRMANHATTAN_TERM_GRACE_SECONDS` | 5 | between asking a job to stop (TERM) and forcing it (KILL) |
| `DRMANHATTAN_MAX_SKIP_SECONDS` | 600 | the longest a job can be passed over by shorter ones |
| `DRMANHATTAN_LOG_RETENTION_DAYS` | 7 | how long results and logs are kept |
| `DRMANHATTAN_MAX_LOG_MB` | 1024 | the most one job's log may hold; past it the earlier output is dropped |
| `DRMANHATTAN_MAX_FINISHED` | 400 | finished runs kept for `status` and the dashboard, newest first (1 to 499) |
| `DRMANHATTAN_ARCHIVE_DAYS` | 90 | how long finished runs' records are kept |
| `DRMANHATTAN_POLL_MS` | 200 | how often the inbox is checked when no change is noticed |
| `DRMANHATTAN_HEARTBEAT_SECONDS` | 30 | how often a running job's load is sampled |
| `DRMANHATTAN_STATE_DIR` | `/tmp/drmanhattan-<uid>` | run records and the lock (Linux: `$XDG_RUNTIME_DIR/drmanhattan`) |
| `DRMANHATTAN_DATA_DIR` | `~/Library/Application Support/drmanhattan` | timing history and settings (Linux: `~/.local/share/drmanhattan`) |

Waiting jobs start in this order: priority jobs, jobs you dragged, jobs that have waited past
their limit, then the shortest expected job first. `submit` records who submitted a job
(`--source`, or detected for bb, Claude Code, Codex, Cursor, VS Code and Copilot).

## Running as a service

`packaging/launchd/` (macOS) and `packaging/systemd/` (Linux) have a service file each, with
how to install it at the top. A service has no terminal to answer the first start's question,
so run `drmanhattan` once in a terminal first, or add `--yes`.

## Updating

```
drmanhattan update        # between jobs: holds new starts until the running ones finish, then restarts
drmanhattan update --now  # does not wait; running jobs end when the daemon restarts
```

`update` installs the latest release over the running one (both commands, in the directory
the running one lives in), after checking the archive's SHA-256, and restarts the daemon:
through launchd or systemd when it runs as a service, else by starting the new version itself.
A restart ends the jobs running on this machine, so once the new version is downloaded and
checked, `update` holds new starts until those jobs have finished (`status` says
`hold='update: …'`, and the dashboard's log `updating: new jobs wait while the running ones
finish`), then restarts, which takes about a second. Jobs submitted meanwhile are queued as
usual and keep their places, and `submit --wait` carries on waiting through the restart, so
the queue pauses at most as long as the jobs already running take, not until it empties.
Services and runs on workers don't hold the update up: services start again with the new
daemon, and workers' runs carry on there. A hold whose `update` was killed holds nothing.
A failed download or checksum leaves the installed version as it was; a directory it cannot
write to gets a hint to rerun the installer. The daemon looks for a newer release every 4
hours by the wall clock, so a laptop that slept past a check looks soon after it wakes, and a
minute after it starts when the last look is older than that
(`DRMANHATTAN_NO_UPDATE_CHECK=1` turns that off); when there is one, `status` says so and the
dashboard's footer, which shows the version, offers an Update button.
Until the daemon restarts into a newly installed version, `status` says `installed=0.6.2
running=0.6.1 restart=pending` and the footer offers a Restart button; `drmanhattan update`
then restarts it, waiting for the queue as usual.

`drmanhattan update --auto on` (or the footer's toggle) lets the daemon install newer
releases of the same major version by itself, as soon as it finds one, the same way: between
jobs, never mid-job, however busy the queue is. A new major version only shows the notice. A
failed update leaves the running version and is tried again an hour later. `status`
(`auto_update_state=`) and the footer toggle's tooltip say where it stands: `up to date
(checked 2:10 PM, next ~6:10 PM)`, `updating to 0.6.1 between jobs` or `last attempt failed:
…`. A daemon older than 0.9.1 waits for an empty queue instead, and doesn't know the hold;
on a queue that is never empty, update it once with `drmanhattan update --now`, at a moment
when ending the running jobs is fine. From then on updates happen between jobs.
After an update the daemon's log says `auto-updated 0.6.1 → 0.6.2`, and the dashboard shows
a note you can dismiss. It is off by default; `status` shows the setting.

On macOS, privacy grants such as access to removable volumes belong to a binary's signature,
so an unsigned update asks for them again. With `sign` in `config.json`, every update is signed
before it replaces the installed commands, and one that cannot be signed leaves them as they
were:

```json
"sign": {"identity": "Apple Development: Your Name (TEAMID)", "identifier": "com.example.drmanhattan"}
```

`identity` is a code-signing identity in the login keychain (`security find-identity -p
codesigning`); `identifier` is optional. Keep both the same from one update to the next.

## Disk use

drmanhattan cleans up after itself, so it can run for months:

- A job's `.log`, `.status.json`, `.exit` and job file are deleted seven days after it
  finishes (`DRMANHATTAN_LOG_RETENTION_DAYS`).
- A running job's log is capped at 1 GB (`DRMANHATTAN_MAX_LOG_MB`). Past that, the earlier
  output is dropped and the newest kept, since errors are usually at the end.
- The daemon keeps its own output in `drmanhattan.log` in the data directory, rotated at 5 MB
  with one older copy.
- `status` and the dashboard know the newest 400 finished runs
  (`DRMANHATTAN_MAX_FINISHED`), so that reading them stays quick however busy the queue
  gets: every `status` reads each one. An older run's own files still answer `status
  <run>`, `wait` and `follow` until they are deleted.
- Records of finished runs are kept in the archive for 90 days (`DRMANHATTAN_ARCHIVE_DAYS`),
  and the timing history, which expected times come from, keeps at most its newest 30,000
  runs.

## Security

- An assistant or tool that can run `manhattan submit`, or write into the inbox, can run any
  command as you, outside its own sandbox or approval prompts. Only connect tools you would
  let run commands.
- Anything placed in the inbox runs as you. The inbox is created private (`0700`); the queue
  refuses an inbox others can write to and skips files owned by other users.
- The dashboard listens on 127.0.0.1, and other websites cannot read it or change it. With
  `--bind` set to another address it cannot be changed from there either, but anyone who can
  reach it can read your jobs' logs.
- Logs hold whatever your commands print. They are private to you and deleted after seven
  days.

## Uses

- **Coding agents sharing one machine.** With the README's snippet in each one's instructions,
  or bb's plugin, their builds run one after another instead of all at once.
- **A nightly test run that waits its turn.** In `crontab -e`:
  `0 2 * * * $HOME/.local/bin/manhattan submit --label nightly --cwd $HOME/app -- make test`
- **An editor task that deploys and reports the result.** Run
  `manhattan submit --wait --label deploy -- ./deploy.sh staging`; the task fails if the
  deploy does.
