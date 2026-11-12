# drmanhattan

drmanhattan is a local job queue. It watches a folder for JSON job files and runs each
command when a slot is free, one or a few at a time, writing the log, status and exit
code beside the job file. Commands that nearly always finish in seconds here start at
once instead of waiting behind a build. A dashboard on localhost shows what is running
and waiting.

![The dashboard: two running jobs, three waiting, recent results with one failure, and the log of a running test job open below](docs/dashboard.png)

```
$ drmanhattan --yes &                      # start the queue in the background
$ manhattan submit --wait -- echo hello    # queue a command and wait for it
drmanhattan · queued echo hello as echo_hello-1793647588-31235-3799f0
drmanhattan · echo hello started in slot 1 at 14:52:34
drmanhattan · in ~: echo hello
hello
drmanhattan · succeeded in 0s, exit 0
```

## Install

macOS (universal) or Linux (x86_64):

```
curl -fsSL https://github.com/gagansingh/drmanhattan/releases/latest/download/install.sh | sh
```

This puts `drmanhattan` and its alias `manhattan` in `~/.local/bin`; for another
directory, `… | sh -s -- /usr/local/bin`. With Rust 1.75 or later,
`cargo install --git https://github.com/gagansingh/drmanhattan` works too, as does
`cargo build --release` in a clone.

## Quick start

```
drmanhattan --open                     # start the queue and open the dashboard
manhattan submit --wait -- make test   # in another terminal
```

The first start asks once whether drmanhattan may run what is placed in its inbox;
`--yes` answers for you. `submit --wait` prints the job's output and exits with its exit
code, and Ctrl-C cancels the job. `drmanhattan status` shows the queue, `drmanhattan stop`
stops it, and `drmanhattan update` installs the latest release between jobs: new jobs wait
while the running ones finish, then the queue restarts on the new version in a second.

## With coding agents

In [bb](#with-bb), install the plugin instead: it gives every thread these instructions,
so there is nothing to copy. For other agents (Claude Code, Codex, Cursor and the like),
add this to the instructions file your agent reads (`AGENTS.md`, `CLAUDE.md`,
`.cursorrules` or similar):

```markdown
## Running commands
Run builds, tests and other long or heavy commands through drmanhattan:

    manhattan submit --wait -- <command>

Long commands wait for a free slot, so they never overlap; commands that
usually finish in seconds here start at once. It prints the output and exits
with the command's exit code. If your own tool times out while waiting,
don't submit again: the run carries on, and `manhattan follow <name>` (the
name after `as` in the line `submit` printed) prints its output and exits
with its exit code. `manhattan status` shows what is running and waiting.

Start dev servers and databases as services, never with `--wait`:

    manhattan submit --service --ready-port <port> -- <command>

It returns once the port accepts connections (or a line matches
`--ready-log <regex>`) and prints the service's name and log. Submitting it
again returns the running one: add `--restart` to replace it, and run
`manhattan stop <name>` when you are done with it.

For commands that must never overlap and where only the latest run matters,
such as deploys, add `--group <name>`.

Jobs already get the user's login shell environment: don't wrap commands
in `zsh -ic` or `bash -lc`. They don't get variables set in this session;
pass those with `--env KEY=VALUE`. Jobs get no stdin: pass input as a file
or an argument, never by piping into `manhattan submit`.

Use drmanhattan too for any command you can't run directly in this
environment. If it reports that drmanhattan isn't running, ask the user to
start it; don't start it yourself.
```

Agents sharing a machine then run their builds one after another, and their quick commands
do not wait behind those builds. When the queue is not running, `submit --wait` says so at
once instead of queueing a job that would never start.

## With bb

[bb](https://github.com/get-bb/bb) runs coding agents in threads. drmanhattan has a plugin
for it in bb's Community marketplace: find **drmanhattan** in bb's plugin catalog (under
Command Line), or install it from a terminal:

```
drmanhattan bb install
```

Threads then queue their builds and tests through drmanhattan without the snippet above:
the plugin gives every thread the instructions. Then:

- the row above each thread's composer shows its runs (running, waiting, and a button to
  cancel), and the Info tab lists its recent results;
- a drmanhattan page in bb's navigation shows the queue on each machine, each run linked to
  the thread that queued it; on a machine without drmanhattan it offers to install and start
  it, showing the commands it runs;
- archiving or deleting a thread stops the dev servers and other services it started;
- `bb drmanhattan runs` prints a thread's runs, and each run on the dashboard links back to
  its thread.

[More](docs/REFERENCE.md#bb).

## What else it does

Each item links to its section of [docs/REFERENCE.md](docs/REFERENCE.md).

- **[Quick commands](docs/REFERENCE.md#quick-commands)**: a command whose runs here nearly
  always took under 30 seconds starts beside the slots, without waiting.
- **[Light jobs](docs/REFERENCE.md#light-jobs)**: a job that waits on a device or a network
  says `"light": true` and starts beside the slots too, a couple at a time, unless the
  machine is short of memory.
- **[Job files](docs/REFERENCE.md#job-files)**: any program can queue a job by writing a
  JSON file into the inbox. The log, status and exit code appear beside it, and `wait`,
  `follow` and `cancel` work on any run.
- **[Services](docs/REFERENCE.md#services-servers-that-keep-running)**: a dev server or a
  database runs outside the slots. `manhattan submit --service --ready-port 3000 -- npm
  run dev` returns once it is listening.
- **[Workers](docs/REFERENCE.md#workers-running-jobs-on-other-machines)**: jobs can run on
  other machines over SSH, each running its own drmanhattan.
- **[Prepare](docs/REFERENCE.md#preparing-while-waiting),
  [groups](docs/REFERENCE.md#one-at-a-time-groups),
  [fail patterns](docs/REFERENCE.md#failing-on-output) and
  [progress markers](docs/REFERENCE.md#progress-markers)**: fetch inputs while a job
  waits, keep deploys from overlapping, fail a run on what it prints, and show its steps
  on the dashboard.
- **[Configuration](docs/REFERENCE.md#configuration)**,
  [running as a service](docs/REFERENCE.md#running-as-a-service) and
  [updating](docs/REFERENCE.md#updating).

## Security

Anything in the inbox runs as you. A tool that can run `manhattan submit` can therefore
run any command as you, outside its own sandbox or approval prompts, so connect only tools
you would let run commands. The dashboard listens on 127.0.0.1, and logs are private to
you and deleted after seven days. [More](docs/REFERENCE.md#security); to report a problem,
see [SECURITY.md](SECURITY.md). [docs/DESIGN.md](docs/DESIGN.md) describes the inside.

## License

MIT
