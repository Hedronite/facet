---
name: facet
description: >
  Inspect Facet HTTP request history, sessions, pins, and stored-value
  names through the facet CLI. Use when a task needs to know what requests
  ran, their status and timing, or to compare two runs. Prefer this skill over
  the facet MCP server: it adds no tool schemas to context.
---

# facet — request history skill

Read-only. Run these from this directory (`skills/facet/`).

Every command appends `--json` after the subcommand (`facet history --json`).
`facet --json history` is not a Facet command: the first token chooses Facet
or Probe, and Probe does not implement these commands.

Arguments that look like flags are refused, so a path cannot smuggle one.

## Commands

```bash
nu facet.nu history [path] [--limit 20] [--request sel] [--status 200] [--session id]
nu facet.nu last [path] [--request sel]
nu facet.nu sessions [--open]
nu facet.nu session <id|current>
nu facet.nu pins [path]
nu facet.nu env [path] [--environment e]   # names only, never values
nu facet.nu diff <idA> <idB> [path]        # hash-first; no body text
nu facet.nu doctor                         # exit 1 is warnings, still a result
```

| Skill | CLI |
| --- | --- |
| `history` | `facet history [<path>] --limit <n> --request <selector> --status <code> --session <id\|current>` |
| `last` | `facet last [<path>] --request <selector>` |
| `sessions` | `facet session list [--open]` |
| `session` | `facet session show <id\|current>` |
| `pins` | `facet pin list [<path>]` |
| `env` | `facet env list [<path>] [--environment <name>]`, then `{environment, name}` only |
| `diff` | `facet diff <idA> <idB> [<path>]` with no `--bodies` |
| `doctor` | `facet doctor` |

`history` always sends `--limit`. The skill default is 20. The CLI default, when `--limit` is omitted, is 50. `--status` is one HTTP status code (`200`), not a class (`2xx`). `--session current` reads `FACET_SESSION`.

`diff` sets `same: false` when the CLI exits 1 (the runs differ). That is a result, and the body text is not included. `doctor` sets `healthy: false` when the CLI exits 1 for warnings; the JSON document is still returned.

`env` returns `entries` of `{environment, name}` only.

## Not wrapped, on purpose

- `history --bodies`, `history --id`, `blob`, `diff --bodies`: these can print recorded request or response bodies and headers, which may carry tokens.
- `history --sql`: not wrapped. It is an open query, not a fixed metadata read.
- `request run`, including `--dry-run`: a preview resolves variables, so it can show a hydrated secret. A real run also sends traffic.
- `replay`, `env set`, `env delete`, `pin` (create or delete), `gc --yes`, `session start`, `session end`: these change state or send traffic. Run them only when a human has explicitly asked, and never pass a secret value on the command line.
