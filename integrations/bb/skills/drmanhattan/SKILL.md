---
name: drmanhattan
description: "Run builds, tests and other heavy commands through the drmanhattan queue on this machine, and look up this thread's queued runs."
---

# drmanhattan

drmanhattan is a local job queue. A command submitted to it waits for a free slot, so
builds and tests from different threads never overlap; a command that nearly always
finishes in seconds here starts at once.

## Run a command

```sh
manhattan submit --wait -- cargo test
```

It prints the output and exits with the command's exit code. The run is linked to this
thread (drmanhattan reads `BB_THREAD_ID`), so it shows in the thread's context row and
Info tab, and on the drmanhattan dashboard with a link back here.

- Your tool timed out while waiting: don't submit again. `manhattan follow <name>` (the
  name after `as` in the line `submit` printed) prints the output and exits with the
  run's exit code.
- Dev servers and databases: `manhattan submit --service --ready-port 3000 -- npm run dev`
  returns once the port answers. Submitting it again returns the running one;
  `--restart` replaces it; `manhattan stop <name>` stops it.
- Deploys and other commands that must not overlap, where only the latest run matters:
  add `--group <name>`.
- Jobs get the user's login shell environment, not this session's variables: pass those
  with `--env KEY=VALUE`. Jobs get no stdin.
- `drmanhattan isn't running`: ask the user to start it. Don't start it yourself.

## Look at runs

```sh
bb drmanhattan runs [--thread <id>] [--limit N] [--json]   # this thread's runs
bb drmanhattan status [--machine <host-id>] [--json]       # the whole queue
bb drmanhattan cancel <run-id> [--thread <id>]             # cancel one of this thread's runs
manhattan status                                           # the whole queue, from the shell
```
