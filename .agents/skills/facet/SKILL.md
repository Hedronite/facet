---
name: facet
description: >
  Read a Facet collection, its requests, run history, sessions, pins, and
  stored-value names through the facet CLI. Use when a task needs request
  metadata, a recorded run, a read-only SQL query, a Nickel check or export,
  or a comparison of two runs. Prefer this skill over the facet MCP server:
  it adds no tool schemas to context.
---

# facet — request history skill

Read-only. Run these from the directory that contains this file.

Every command appends `--json` after the subcommand (`facet history --json`).
`facet --json history` is not a Facet command: the first token chooses Facet
or Probe, and Probe does not implement these commands.

Arguments that look like flags are refused, so a path cannot smuggle one.

Each command resolves a collection, `cd`s into that directory, and then runs
`facet`. A path may be the collection directory, `opencollection.yml`,
`opencollection.yaml`, or a file or directory inside the collection. With no
path, the skill walks up from the working directory looking for those files
or `.facet/lattice.db`. If none is found, the envelope is `ok: false` and
`facet` is not run.

`FACET_ACTOR` is exported for attribution. When it is unset or empty, the
skill sets `human`, the CLI default (`actor_from_env`). It does not invent
an actor name.

## Commands

On failure the envelope is `ok: false` and includes `exit` and `stderr`.

```bash
nu facet.nu requests [path]
nu facet.nu request <selector> [path] [--environment e] [--strict-variables]
nu facet.nu history [path] [--limit 20] [--request sel] [--status 200] [--session id]
nu facet.nu history-get <id> [path]          # history --id, no bodies
nu facet.nu last [path] [--request sel]
nu facet.nu sql <query> [path]               # history --sql, read-only
nu facet.nu sessions [path] [--open]
nu facet.nu session <id|current> [path]
nu facet.nu pins [path]
nu facet.nu env [path] [--environment e]     # names only, never values
nu facet.nu diff <idA> <idB> [path]          # hash-first; no body text
nu facet.nu ncl-check <module> [path]
nu facet.nu ncl-export <module> [path]
nu facet.nu doctor [path]                    # exit 1 is warnings, still a result
```

| Skill | CLI |
| --- | --- |
| `requests` | `facet request list .` after `cd` |
| `request` | `facet request get . <selector> [--environment <name>] [--strict-variables]` after `cd` |
| `history` | `facet history --limit <n> --request <selector> --status <code> --session <id\|current>` after `cd` |
| `history-get` | `facet history --id <ulid>` after `cd`, with no `--bodies` |
| `last` | `facet last --request <selector>` after `cd` |
| `sql` | `facet history --sql <query>` after `cd` |
| `sessions` | `facet session list [--open]` after `cd` into the collection |
| `session` | `facet session show <id\|current>` after `cd` into the collection |
| `pins` | `facet pin list` after `cd` into the collection |
| `env` | `facet env list [--environment <name>]` after `cd`, then `{environment, name}` only |
| `diff` | `facet diff <idA> <idB>` after `cd`, with no `--bodies` |
| `ncl-check` | `facet ncl check <module>` after `cd` |
| `ncl-export` | `facet ncl export <module>` after `cd` |
| `doctor` | `facet doctor` after `cd` into the collection |

`history` always sends `--limit`. The skill default is 20. The CLI default, when `--limit` is omitted, is 50. `--status` is one HTTP status code (`200`), not a class (`2xx`). `--session current` reads `FACET_SESSION`.

`history --id` returns the recorded run with body size and hash, not body text. `history --sql` is read-only: the CLI opens the workspace store on a read-only connection and rejects a statement that is not read-only (`sql_read_only`). The machine store is not attached.
Body and header columns (`req_body`, `res_body`, `req_headers`, `res_headers`) come back as `{redacted: true, bytes: N}`; other blob values come back as a size.

`diff` sets `same: false` when the CLI exits 1 (the runs differ). That is a result, and the body text is not included. `doctor` sets `healthy: false` when the CLI exits 1 for warnings; the JSON document is still returned.

`env` returns `entries` of `{environment, name}` only.

## Not wrapped, on purpose

- `request run`, including `--dry-run`: sends traffic, and dry-run can hydrate secrets.
- `replay`: sends traffic, and dry-run can hydrate secrets.
- `ncl apply`: writes.
- `session start` and `session end`: writes.
- `blob`: bodies may hold secrets.
- `env set` and `env delete`: writes.
- `pin` create and `pin delete`: writes.
- `history --bodies` and `diff --bodies`: bodies may hold secrets.
