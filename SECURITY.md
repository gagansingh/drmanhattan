# Security

## The model

drmanhattan runs, as you, whatever command a job file in its inbox names. Its job is to make
sure that only you can put a job there, and that nothing else on the machine, or in your
browser, can change what it does.

- **Starting it is a choice.** The first start says what drmanhattan does and asks once;
  `--yes` or `DRMANHATTAN_ACCEPT=1` answers for a service.
- **The inbox** is created `0700`. The daemon refuses to start on an inbox that other users
  can write to, and skips any job file that another user owns.
- **State and data** live in `0700` directories; records, logs and the lock are `0600`. Files
  are written through a temp file and a rename, and a symlink planted where the daemon writes
  is refused rather than followed. A job's `env` is kept out of the status file beside it.
- **The dashboard** binds to `127.0.0.1`. Every request must carry a loopback `Host` header,
  which defeats DNS rebinding, and every response forbids framing. Its two writes (the queue
  order and the capacity override) also need a loopback `Origin`, a custom header and a JSON
  content type, which no other site can send from your browser. Bound to another address
  with `--bind`, it is read-only, and anyone who can reach it can read your logs.
- **Links** on the dashboard never use `javascript:`, `data:`, `vbscript:` or `file:`.

Logs contain whatever the commands print. They are removed seven days after a job finishes
(`DRMANHATTAN_LOG_RETENTION_DAYS`).

## Reporting a vulnerability

Please report it privately, through GitHub's "Report a vulnerability" button on the
repository's Security tab, rather than in a public issue. Include what you did, what
happened, and the version (`drmanhattan --version`). You will get a reply within a week.
