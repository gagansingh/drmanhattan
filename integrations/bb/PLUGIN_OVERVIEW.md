Queue your threads' builds and tests through [drmanhattan](https://github.com/gagansingh/drmanhattan), a local job queue, and see each thread's runs where you follow the thread.

## What you get

- **Agents that queue heavy commands.** On machines where drmanhattan is installed, threads are told to run builds and tests with `manhattan submit --wait`, so agents working side by side take turns instead of overloading the machine. Commands that usually finish in seconds still start at once.
- **A thread's runs beside its composer.** While a thread has runs going, the row above its composer says how many are running and waiting. Expand it to see each run's slot, place in line and time against its usual time, cancel one, or open its log on the dashboard.
- **A history in the Info tab.** The thread's recent runs, passed or failed, with a link to each log.
- **Links back.** On the drmanhattan dashboard, each run from a bb thread links to that thread.

## For agents and scripts

- `bb drmanhattan runs` lists a thread's runs (the current thread by default).
- `bb drmanhattan status` shows the whole queue on a machine.
- `bb drmanhattan cancel <run-id>` cancels one of the thread's runs.

Add `--json` for machine-readable output.

## Settings

**Tell agents to run heavy commands through drmanhattan** (on by default) adds the usage to threads' instructions. The plugin's skill stays available either way.

## Requirements

drmanhattan 0.8.0 or later on each machine whose threads should use it, started once with `drmanhattan --yes &` or as a service. Machines without it are left alone.
