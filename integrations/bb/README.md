# drmanhattan for bb

A [bb](https://github.com/get-bb/bb) plugin for [drmanhattan](../../README.md): bb's agents
queue their builds and tests through drmanhattan, and each thread shows its runs.
[PLUGIN_OVERVIEW.md](PLUGIN_OVERVIEW.md) is what bb shows on the plugin's page;
[docs/REFERENCE.md](../../docs/REFERENCE.md#bb) describes it from drmanhattan's side.

## Install

Find drmanhattan in bb's plugin catalog (the Community marketplace, under Command Line), or:

```
drmanhattan bb install
```

which runs `bb plugin install drmanhattan`.

## How it fits together

- `host.ts` runs on each bb machine. It finds `drmanhattan` (`DRMANHATTAN_BIN`, `PATH`,
  `~/.local/bin`, `~/.cargo/bin`) and runs `drmanhattan status --json [--session <thread>]`
  and `drmanhattan cancel <run>`.
- `server.ts` looks for drmanhattan on each machine when it connects and every five minutes;
  threads on machines that have it get the `drmanhattan` skill and, unless the setting is
  off, instructions to submit heavy commands. It also serves the app's RPC, checking that a
  run belongs to the thread before cancelling it, and the `bb drmanhattan` command.
- `server.ts` also stops a thread's services (`stopServices` on each machine) when bb
  announces that the thread was archived or deleted.
- `app.tsx` adds the segment above a thread's composer, the section in its Info tab, and
  the drmanhattan page in bb's navigation (`machines` and `cancelMachineRun` RPCs).
- `submit` itself records the thread: bb gives each thread's shell `BB_THREAD_ID`.

## Develop

```
npm install
npm test             # vitest: host entry against a stand-in drmanhattan, server via bb's fake host, app in jsdom
npm run typecheck
npm run build        # bb plugin build
bb plugin install path:../.. --plugin drmanhattan   # into a running bb, from this checkout
```

The version follows drmanhattan's: bb installs from the `vX.Y.Z` tags.

## The marketplace listing

The listing is [`entries/drmanhattan.json`](https://github.com/get-bb/marketplace/blob/main/entries/drmanhattan.json)
in [get-bb/marketplace](https://github.com/get-bb/marketplace), with its screenshots in
`screenshots/drmanhattan/` and a copy of [PLUGIN_OVERVIEW.md](PLUGIN_OVERVIEW.md) in
`overview/drmanhattan.md`. Its source is this repository's `integrations/bb` at the tags
`>=0.8.1 <1.0.0`.

- Every `vX.Y.Z` release tag below 1.0.0 is a plugin release: bb users see it as an update
  once it is tagged, with no marketplace change. So each tag must build here
  (`npm ci && npm run build`), and a tag must never move or be deleted.
- The listing's text, screenshots, overview and tags change only through a pull request to
  get-bb/marketplace (from a fork; `author.github` there names who may change it). Changes to
  the short description, tags, icon or source also need its maintainers' `v1-change` label.
  Copy PLUGIN_OVERVIEW.md to `overview/drmanhattan.md` when it changes.
- A 1.0.0 release falls outside the range: widen it there first, or users stay on 0.x.
- The marketplace checks every listed source on each of its pull requests, so this
  repository and its tags must stay public.
