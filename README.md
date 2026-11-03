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
`drmanhattan status` shows the queue, `drmanhattan stop` stops it, and `drmanhattan update`
installs the latest release once the queue is empty. The queue's own terminal prints a line
for each thing that happens:

```
drmanhattan 0.6.2 running · dashboard http://127.0.0.1:8417/
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
Start servers with `manhattan submit --service --ready-port <port> -- <command>` (or
`--ready-log <regex>`), never with `--wait`: it returns once the server is ready.
Use it too for any command you can't run directly in this environment.
Don't wrap commands in `zsh -ic`/`bash -lc`; drmanhattan already gives jobs your shell environment.
If it reports that drmanhattan isn't running, ask the user to start it; don't start it yourself.
```

With that in place, several agents sharing one machine run their builds one after another
instead of all at once. When drmanhattan isn't running, `submit --wait` queues nothing and
says so at once, so an agent never waits on a job that will not start.

## What else it does

Each of these is explained in [docs/REFERENCE.md](docs/REFERENCE.md).

- **Any program can queue a job** by writing a JSON file into the inbox; the log, status
  and exit code appear beside it. [Job files](docs/REFERENCE.md#job-files),
  [Results](docs/REFERENCE.md#results), [Following one run](docs/REFERENCE.md#following-one-run).
- **Services** keep a dev server or a database running without taking a slot:
  `manhattan submit --service --ready-port 3000 -- npm run dev`.
  [Services](docs/REFERENCE.md#services-servers-that-keep-running)
- **Workers** run jobs on other machines over SSH, each running its own drmanhattan.
  [Workers](docs/REFERENCE.md#workers-running-jobs-on-other-machines)
- **Agent goals and loops**: `examples/agent-runner.sh` runs a headless coding agent as a
  job until a check passes, or on an interval.
  [Goals and loops](docs/REFERENCE.md#goals-and-loops-for-coding-agents)
- **Finer control**: prepare work while a job waits, let only one job of a group run at a
  time, fail a run on its output, and show progress steps on the dashboard.
  [Preparing while waiting](docs/REFERENCE.md#preparing-while-waiting),
  [Groups](docs/REFERENCE.md#one-at-a-time-groups),
  [Failing on output](docs/REFERENCE.md#failing-on-output),
  [Progress markers](docs/REFERENCE.md#progress-markers)
- **Settings** for slots, ports, retention and timeouts, and running it as a launchd or
  systemd service. [Configuration](docs/REFERENCE.md#configuration),
  [Running as a service](docs/REFERENCE.md#running-as-a-service),
  [Updating](docs/REFERENCE.md#updating)

## Security

Anything placed in the inbox runs as you, so a tool that can run `manhattan submit` can run
any command as you, outside its own sandbox or approval prompts. Only connect tools you would
let run commands. The dashboard listens on 127.0.0.1 only, and logs are private to you and
deleted after seven days. More in [the reference](docs/REFERENCE.md#security); to report a
problem, see [SECURITY.md](SECURITY.md).

[docs/DESIGN.md](docs/DESIGN.md) describes how the queue works inside.

## License

MIT
