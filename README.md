# drmanhattan

drmanhattan is a local job queue. It watches a folder for JSON job files and runs each
command when a slot is free, one or a few at a time. It writes the log, the status and the
exit code next to the job file. A dashboard on localhost shows what is running and waiting.

![The dashboard: two running jobs, three waiting, recent results with one failure, and the log of a running test job open below, its numbered output under a strip saying where and when it started](docs/dashboard.png)

```
$ drmanhattan --yes &                        # start the queue in the background
$ manhattan submit --wait -- echo hello      # queue a command and wait for it
drmanhattan · queued echo hello as echo_hello-1793647588-31235-3799f0 in ~/.drmanhattan/inbox
drmanhattan · echo hello started in slot 1 at 14:52:34 · run 1793647588-30510-16fb20
drmanhattan · in ~: echo hello
hello
drmanhattan · succeeded in 0s, exit 0
$ echo $?
0
```

## Install

macOS (universal) or Linux (x86_64), from the latest release:

```
curl -fsSL https://github.com/gagansingh/drmanhattan/releases/latest/download/install.sh | sh
```

This puts `drmanhattan` and its short alias `manhattan` in `~/.local/bin`. To install
somewhere else, pass a directory: `… | sh -s -- /usr/local/bin`.

With Rust 1.75 or later, `cargo install --git https://github.com/gagansingh/drmanhattan`, or
from a clone, `cargo build --release` and copy both binaries from `target/release` onto
your `PATH`.

## Quick start

```
drmanhattan --open                           # start the queue and open the dashboard
manhattan submit --wait -- make test         # in another terminal
```

The first start asks you once to confirm that drmanhattan may run what is placed in its
inbox; `--yes` confirms without asking.

`submit --wait` prints the job's output and exits with its exit code; Ctrl-C cancels the job.
`drmanhattan status` shows the queue and `drmanhattan stop` stops it. The queue's own terminal
says where it is, then prints a line for each thing that happens, with the run's short id to
follow it by:

```
drmanhattan 0.1.0 running · dashboard http://127.0.0.1:8417/
watching ~/.drmanhattan/inbox · 1 slot

14:52:34  queued     make test            #094a70  make · ~/project · run 1793649754-96609-094a70
14:52:34  started    make test            #094a70  slot 1 · waited 0s
14:53:20  succeeded  make test            #094a70  46s
```

## Using it with coding agents

To have a coding agent run its builds and tests through drmanhattan, add this to the
instructions file it reads (`AGENTS.md`, `CLAUDE.md`, `.cursorrules` or similar):

```markdown
## Running commands
Run builds, tests and other long or heavy commands through drmanhattan:

    manhattan submit --wait -- <command>

It waits for a free slot, prints the output and exits with the command's exit code.
Use it too for any command you can't run directly in this environment.
If it reports that drmanhattan isn't running, ask the user to start it; don't start it yourself.
```

When drmanhattan isn't running, `submit --wait` queues nothing and says so at once, so the agent
does not wait on a job that will never start.

## Goals and loops for coding agents

[`examples/agent-runner.sh`](examples/agent-runner.sh) runs a headless coding agent as a
drmanhattan job, either until a goal is met or on an interval. Copy it onto your `PATH`.

```
# Rounds of: the agent works on the goal, then the check runs. Stops when the check passes.
manhattan submit --label "fix tests" -- agent-runner.sh goal \
  --goal "Make the test suite pass without changing the tests" --check "cargo test" --max-rounds 5

# The same prompt every 10 minutes, until the pull request's checks pass.
manhattan submit --label "watch CI" -- agent-runner.sh loop \
  --prompt "Look at the failing CI checks on this branch's pull request and fix them" \
  --every 10m --until "gh pr checks" --max-minutes 120
```

Goal mode stops after 8 rounds or 60 minutes unless `--max-rounds` and `--max-minutes` say
otherwise; loop mode runs until its `--until` check passes, `--max-runs` or `--max-minutes`
is reached, or it is cancelled. Each round shows as a step on the dashboard, and cancelling
the job stops the agent and anything it started. The agent is `claude -p --continue` unless
`--agent` names another; any agent CLI that takes the prompt as its last argument works. The
rounds share one conversation of their own, or each starts afresh with `--fresh`. The agent
must be able to run headless on this machine: signed in, and allowed to edit files and run
commands without asking. `agent-runner.sh --help` lists the options.

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
| `fail_patterns` | regular expressions that fail the job when a line of its output matches (see below) |

Unknown fields are an error, so a typo is reported rather than ignored.

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

## Dashboard

`http://127.0.0.1:8417/` (`drmanhattan paths` prints it if that port was busy) shows each
slot's job with a progress bar against its usual time, the waiting jobs with estimated start
times, recent results with their logs, and the machine's CPU, memory and temperature. The −
and + buttons change the number of slots for four hours; drag a waiting job to reorder it.

## Uses

- **Coding agents sharing one machine.** With the snippet above in each one's instructions,
  their builds run one after another instead of all at once.
- **A nightly test run that waits its turn.** In `crontab -e`:
  `0 2 * * * $HOME/.local/bin/manhattan submit --label nightly --cwd $HOME/app -- make test`
- **An editor task that deploys and reports the result.** Run
  `manhattan submit --wait --label deploy -- ./deploy.sh staging`; the task fails if the
  deploy does.

## Configuration

Commands: `drmanhattan [serve]`, `submit`, `status [--json]`, `stop`, `slots [N]` and
`paths`. Each has `--help`.

| option | default | meaning |
| --- | --- | --- |
| `--inbox DIR`, `DRMANHATTAN_INBOX` | `~/.drmanhattan/inbox` | the watched folder |
| `--port N` | 8417 | the dashboard's port |
| `--bind ADDR` | 127.0.0.1 | the dashboard's address |
| `--no-dashboard` | | run without the dashboard |
| `--open` | | open the dashboard in the browser |
| `--no-session-links` | | do not read Claude Code or Codex session files to link runs to them |
| `--yes`, `DRMANHATTAN_ACCEPT=1` | | confirm the first start without asking |
| `DRMANHATTAN_SLOTS` | 1 | jobs that may run at once (also `drmanhattan slots N`) |
| `DRMANHATTAN_SHUTDOWN_GRACE_SECONDS` | 30 | how long `stop` lets running jobs finish |
| `DRMANHATTAN_TERM_GRACE_SECONDS` | 5 | between asking a job to stop (TERM) and forcing it (KILL) |
| `DRMANHATTAN_MAX_SKIP_SECONDS` | 600 | the longest a job can be passed over by shorter ones |
| `DRMANHATTAN_LOG_RETENTION_DAYS` | 7 | how long results and logs are kept |
| `DRMANHATTAN_MAX_LOG_MB` | 1024 | the most one job's log may hold; past it the earlier output is dropped |
| `DRMANHATTAN_ARCHIVE_DAYS` | 90 | how long finished runs' records are kept |
| `DRMANHATTAN_DASHBOARD_MAX_SLOTS` | 4 | the highest number the + button goes to |
| `DRMANHATTAN_POLL_MS` | 200 | how often the inbox is checked when no change is noticed |
| `DRMANHATTAN_HEARTBEAT_SECONDS` | 30 | how often a running job's load is sampled |
| `DRMANHATTAN_STATE_DIR` | `/tmp/drmanhattan-<uid>` | run records and the lock (Linux: `$XDG_RUNTIME_DIR/drmanhattan`) |
| `DRMANHATTAN_DATA_DIR` | `~/Library/Application Support/drmanhattan` | timing history and settings (Linux: `~/.local/share/drmanhattan`) |

Waiting jobs start in this order: priority jobs, jobs you dragged, jobs that have waited past
their limit, then the shortest expected job first. `submit` records who submitted a job
(`--source`, or detected for Claude Code, Codex, Cursor, VS Code and Copilot).

## Running as a service

`packaging/launchd/` (macOS) and `packaging/systemd/` (Linux) have a service file each, with
how to install it at the top. A service has no terminal to answer the first start's question,
so run `drmanhattan` once in a terminal first, or add `--yes`.

## Disk use

drmanhattan cleans up after itself, so it can run for months:

- A job's `.log`, `.status.json`, `.exit` and job file are deleted seven days after it
  finishes (`DRMANHATTAN_LOG_RETENTION_DAYS`).
- A running job's log is capped at 1 GB (`DRMANHATTAN_MAX_LOG_MB`). Past that, the earlier
  output is dropped and the newest kept, since errors are usually at the end.
- The daemon keeps its own output in `drmanhattan.log` in the data directory, rotated at 5 MB
  with one older copy.
- Records of finished runs are kept for 90 days (`DRMANHATTAN_ARCHIVE_DAYS`), and the timing
  history keeps at most its newest 30,000 runs.

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

See [SECURITY.md](SECURITY.md) to report a problem, and [docs/DESIGN.md](docs/DESIGN.md) for
how the queue works inside.

## License

MIT
