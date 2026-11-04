# drmanhattan

drmanhattan is a local job queue. It watches a folder for JSON job files and runs each
command when a slot is free, one or a few at a time, writing the log, status and exit
code beside the job file. A dashboard on localhost shows what is running and waiting.

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
stops it, and `drmanhattan update` installs the latest release once the queue is empty.

## With coding agents

Add this to the instructions file your agent reads (`AGENTS.md`, `CLAUDE.md`,
`.cursorrules` or similar):

```markdown
## Running commands
Run builds, tests and other long or heavy commands through drmanhattan:

    manhattan submit --wait -- <command>

It waits for a free slot, prints the output and exits with the command's
exit code. If your own tool times out while waiting, don't submit again:
`manhattan wait <name>`, with the name `submit` printed, waits for the
same run. `manhattan status` shows what is running and waiting.

Start dev servers and databases as services, never with `--wait`:

    manhattan submit --service --ready-port <port> -- <command>

It returns once the port accepts connections (or a line matches
`--ready-log <regex>`) and prints the service's name and log. Submitting it
again returns the running one: add `--restart` to replace it, and run
`manhattan stop <name>` when you are done with it.

For commands that must never overlap and where only the latest run matters,
such as deploys, add `--group <name>`.

Jobs already get your shell environment: don't wrap commands in `zsh -ic`
or `bash -lc`. Jobs get no stdin: pass input as a file or an argument,
never by piping into `manhattan submit`. Use drmanhattan too for any command
you can't run directly in this environment. If it reports that drmanhattan
isn't running, ask the user to start it; don't start it yourself.
```

Agents sharing a machine then run their builds one after another. When the queue is not
running, `submit --wait` says so at once instead of queueing a job that would never start.

## What else it does

Each item links to its section of [docs/REFERENCE.md](docs/REFERENCE.md).

- **[Job files](docs/REFERENCE.md#job-files)**: any program can queue a job by writing a
  JSON file into the inbox. The log, status and exit code appear beside it, and `wait`,
  `follow` and `cancel` work on any run.
- **[Services](docs/REFERENCE.md#services-servers-that-keep-running)**: a dev server or a
  database runs outside the slots. `manhattan submit --service --ready-port 3000 -- npm
  run dev` returns once it is listening.
- **[Workers](docs/REFERENCE.md#workers-running-jobs-on-other-machines)**: jobs can run on
  other machines over SSH, each running its own drmanhattan.
- **[Agent goals and loops](docs/REFERENCE.md#goals-and-loops-for-coding-agents)**:
  `examples/agent-runner.sh` runs a headless coding agent as a job until a check passes,
  or on an interval.
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
