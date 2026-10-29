# Facet

Facet is the terminal-native fork of Probe: the same core, CLI contract, and
OpenCollection YAML, with **`facet-lattice`** underneath to remember every run.
Upstream is `crizant/probe`; this fork lives at `VirtualMachinist/facet` and
stays cherry-pickable in both directions.

This document is the canonical reference for everything Facet adds. Everything
Probe already documents ([CLI](CLI.md), [Architecture](ARCHITECTURE.md)) holds
unchanged.

## Boundary with upstream

| Stays in Facet | Goes upstream first |
| --- | --- |
| **`facet-lattice`** (`crates/lattice`: run history, blobs, `--sql`, gc) | Agent CLI ergonomics, JSON envelope fixes |
| `crates/facet` (the `facet` binary), `crates/facet-record` (shared recording path) | Performance work in `probe-core` |
| `crates/facet-tui` (ratatui interface) | Bug fixes in any upstream crate |

Rules:

- `probe-core` and `probe-cli` are never edited in a Facet slice. Anything that
  must change there is written in upstream style and offered as a Probe PR.
- No upstream crate is renamed. New crates carry Facet names.
- The `facet` binary delegates every Probe command to `probe-cli` verbatim. Only
  `request run` (to record), `history`, `blob`, `gc`, and `tui` are Facet's.
- The legacy Probe GPUI desktop was retired from this Facet workspace in Phase F.
  The in-repo `probe` binary is CLI-only; Facet's interactive surface is the Ratatui TUI.

### License and attribution

- **Upstream-derived files stay Apache-2.0.** Probe's `LICENSE` (Apache License
  2.0) governs `crates/cli`, `crates/core`, `crates/http`,
  `crates/opencollection`, `crates/postman`, `crates/yaak`, and the docs they
  came with. They are never relicensed. The former desktop member is historical,
  not a currently shipped workspace crate. Files modified from upstream carry a
  change notice (Section 4(b)).
- **Facet-original crates are MIT:** `facet-cli` (`crates/facet`), `facet-record`,
  **`facet-lattice`** (`crates/lattice`), `facet-tui`. Each carries `LICENSE-MIT` and declares `license = "MIT"`
  in its own `Cargo.toml`. `crates/facet-tui` retains historical palette
  provenance from `crates/desktop/src/theme.rs` at audited source SHA `2c6ecfd`
  (Apache-2.0), as recorded in `NOTICE`; this does not mean the desktop crate ships here.
- **Copyright:** `Copyright 2026 VirtualMachinist` for the Facet-original work.
  Upstream copyright notices are kept as found; the root `LICENSE` text is
  upstream's and is left byte-identical.
- The root `NOTICE` file states the fork relationship and the per-crate terms.
  Distributions include `LICENSE`, `NOTICE`, and the per-crate `LICENSE-MIT`.
- The workspace `Cargo.toml` still declares `license = "MIT OR Apache-2.0"`
  for inherited upstream crates; that is upstream's own declaration and is
  left alone.

## Binaries

Install paths for strangers: [install.md](install.md) (`facet-lattice` on crates.io; `facet` from releases or source).

`facet` and `probe` build from the same workspace and coexist on `PATH`:

```bash
cargo build --release -p probe-cli -p facet-cli
facet --version   # facet 0.8.0 (probe 0.5.9)
probe --version   # probe 0.5.9
```

`facet --version --json` returns `name`, `version`, and `probeVersion`.

## facet-lattice layout

Two SQLite stores, both via bundled `rusqlite` (the default engine). An
optional in-process DuckDB analytics feature ships behind `lattice-duckdb`
(off in a default build). DuckDB is also usable out-of-process: the `duckdb`
CLI ATTACHes the SQLite file for analytics
(see [Engines](#engines)).

| Store | Path | Holds |
| --- | --- | --- |
| Workspace | `.facet/lattice.db` beside the collection | Run history for that workspace |
| Machine | `~/.local/share/facet/lattice.db` (XDG; platform equivalent elsewhere) | Workspace registry, cross-workspace run index, sessions, environments, preferences |

`.facet/` also holds `workspace.toml` (the workspace ULID, commit it),
`config.toml` (optional, commit it), `blobs/` (content-addressed bodies), and a
`.gitignore` that excludes the database and blobs. The workspace root is the
directory for an unbundled collection or the parent directory of a bundled
file.

Override the machine paths with `FACET_DATA_DIR` and `FACET_CONFIG_DIR`.

The schema is applied as numbered SQL
migrations under `crates/lattice/migrations/{workspace,machine}/` (package **`facet-lattice`**). Every
migration inserts its own `schema_version` row. Times are Unix milliseconds
UTC; ids are ULIDs.

### Bodies

Bodies at or under `inline_body_max` (default 64 KiB) are stored in the row.
Larger bodies are written to `.facet/blobs/<sha256>` and the row keeps the
hash, length, and content type. Identical bodies share one file.

Reader rule: **a non-NULL hash means a blob file; otherwise read the inline
column. Never branch on length.** Rows written under an older threshold stay
readable. A body the transport did not retain (over Probe's 16 MiB in-memory
bound with no spool file) records its length only; `retention` is `none`.

### What is recorded

The recording path is one function, `facet_record::record`, shared by the
`facet` CLI and `facet tui` so both adapters write identical rows.

For each `facet request run`: start time, duration, selector, environment,
method, resolved URL, status (NULL on transport failure) or error text,
request and response headers, bodies, content type, actor, session, tags, and
a `requestHash` (SHA-256 of a canonical view of the resolved request; bodies
enter by hash, authentication by scheme only).

Auto-tags ride beside `--tag` in the same `tags` array, no column:
`git:<sha>` when the workspace root (the directory beside the collection) is
inside a git repository, `git:<sha>-dirty` when that tree has tracked
changes (staged or unstaged; untracked files do not count, so a fresh
`.facet/workspace.toml` does not flag every first run), and `expect:fail`
on an `--expect` miss. Local only (`git rev-parse HEAD`, `git status
--porcelain --untracked-files=no`); no `git` binary or no repository means no
tag, silently. `history --tag git:<sha>` answers "runs against this
revision"; a replay copies the source's user tags but recomputes auto-tags.

Redaction: `Authorization`, `Proxy-Authorization`, `Cookie`, `Set-Cookie`,
`X-Api-Key`, `X-Auth-Token`, `Api-Key`, and `X-Amz-Security-Token` values are
stored as `<redacted>`. URL userinfo (`user:pass@`) is redacted. Request bodies
are scrubbed of known secrets at write: every value the run knows is secret
(hydrated, passed with `--var`, or a token it sends), exactly and in its
URL-encoded forms, becomes `<redacted>` in the stored body, inline or blob.
The row's request body hash is the hash of those stored bytes; `requestHash`
still hashes the raw body, so `replay --frozen` matching is unchanged. A
value Facet does not know is secret is stored as sent. Response bodies are
stored as received.

The machine store gets one pointer row per run (best effort; `indexed: false`
in the output means the run is recorded but not indexed).

Environment: `FACET_ACTOR` names the actor (default `human`);
`FACET_SESSION` sets `sessionId`; `FACET_NO_RECORD=1` disables recording.
`FACET_NO_SUBPROCESS` (any non-empty value) starts no `gh` or `git` child;
see [Child processes](#child-processes).

`FACET_SESSION` is written onto the run row. `facet session start` mints the
parent; recording mint-if-missing if the env is set and the row is absent.

### The `facet:` key

A collection can turn recording off for itself or for one request with a
Facet-owned `facet:` mapping. Probe and other OpenCollection tools ignore
it; Facet reads it from the collection or request source.

```yaml
# Collection level (opencollection.yml, or the top of a bundled file):
# nothing in this collection is recorded.
facet:
  record: never
```

```yaml
# Item level (in the request): this request is not recorded.
facet:
  record: false
```

- `record: never` (collection) or `record: false` (item) writes no run row
  and no blob for `request run`, `replay`, the TUI and MCP, in every layout
  the loader accepts. The request is still sent and its output still
  prints, with `lattice.recorded: false`.
- No flag overrides it: `--no-record`, `FACET_NO_RECORD=1` and MCP
  `noRecord` can turn recording off, never on.
- When Facet cannot tie the selector to one request in the source, it
  records nothing: an empty or unresolved bundled selector, or one that
  names something other than a request, and an unbundled selector that
  leaves the collection (`..`, an absolute path, a symlink escape). The
  request is still sent.
- Inside `facet:`, an unknown key, a wrong type or an unknown value fails
  with `facet_key_invalid` (exit 3) before anything is sent.
  `error.details.path` names the key path (`facet`, `facet.<name>` or
  `facet.record`); the error never echoes the value.
- Outside `facet:`, unknown YAML is accepted and kept as before. A command
  that rewrites the YAML (`request set`, structure edits) keeps the
  `facet:` key.

### Shipped 2026-09-07 (*and more*)

The week-1 loop is closed on `main` (`5087ba7`, PRs #8–#13): send → see →
compare → send again, with a ULID an agent can hold. Probe's public "and
more" was blank; this is what Facet put there.

| # | Item | On `main` | PR |
| --- | --- | --- | --- |
| 1 | **Session lifecycle** | `facet session start/end/list/show`; mint-if-missing; `history --session` | [#8](https://github.com/VirtualMachinist/facet/pull/8) |
| 2 | **Recall** | `history --id`; filters `--session/--environment/--tag/--hash`; TUI `:history` grid hydrates + yank | #8 + [#9](https://github.com/VirtualMachinist/facet/pull/9) |
| 3 | **Replay** | `facet replay` = **current YAML** + recorded env; `--frozen` refuses (exit 1) | [#10](https://github.com/VirtualMachinist/facet/pull/10) |
| 4 | **Hash-first diff** | `facet diff`; exit 0/1/4/9; `durationMs`/`actor`/`tags` do not flip `equal` | #10 |
| 5 | **Secret hydration** | Lattice env overlay as `--var` before resolve; `--var` wins; `facet env` | [#11](https://github.com/VirtualMachinist/facet/pull/11) |
| 5′ | **`facet doctor`** | Preflight + `--sql` ATTACH refused | [#12](https://github.com/VirtualMachinist/facet/pull/12) |
| 6 | **`--expect` + `--dry-run`** | Miss → exit **1** `expect_failed`; transport stays 6; `--dry-run` no network | [#13](https://github.com/VirtualMachinist/facet/pull/13) |

**Calls that landed:** replay truth is current YAML; overlay secrets now (no
upstream provider hook); `--expect` is exit **1** (6 stays network); 0003 is
`replayed_from` + `var_names` (names only), not `git_head`; MCP waits until
after this train (now eligible).

CLI remains the harness contract. MCP wraps the same functions and must not
parse stdout.

### Next slice

Two trains. Finish the Facet-owned rest **before** pivoting to Probe.

#### Facet-owned rest (no `probe-core`)

| # | Item | Smallest slice | Belonging |
| --- | --- | --- | --- |
| 1 | **MCP** | `facet mcp` stdio. Tools over lattice + record + the same run function (never shell out to `facet`). v1: session start/end, request list/get/run, history list/get, blob_get, run_diff, run_replay, sql_query. Same JSON envelopes. Skills/rules before freezing names. | Facet adapter; not a second API |
| 2 | **Git HEAD auto-tag** | On record, tag `git:<sha>[-dirty]` when cwd is a repo. No column. `history --tag git:…`. | Facet-only |
| 3 | **Bells** | `facet last`; sparkline on the `:history` grid; pins. Each rides a command that is already green. | Facet-only |
| 4 | **Theme files** (Probe 05 rest) | Versioned files for `facet-tui`; invalid → Graphite/Porcelain. | Facet TUI only; the retired Probe desktop is not in this workspace |
| 5 | **TUI env editor** (Probe 07 rest) + `ctrl+u`/`ctrl+d` | Overlay UI on `facet env` / machine store. | Facet TUI |

#### Probe contribution (later, separate)

Give Probe the pieces that belong in `probe-cli` / `probe-core`. Protocol work (02–04) is built in Facet first and proposed upstream after it works here. Fresh branch off upstream `main`.

| # | Item | Notes |
| --- | --- | --- |
| A1 | `--expect` + `--dry-run` on `request run` | Exit **1** `expect_failed`; transport stays 6. Facet-only fallback already shipped. |
| A2 | Secret-provider hook | Probe still errors `secret_variable_unavailable` with no `--var`. A host trait; do not push Lattice into Probe. |
| B | Probe 02–04 | WebSocket, GraphQL, gRPC streaming, and the shared protocol session/event layer. Build them in Facet first, with Facet JSONL, the TUI session pane, and Lattice events. Keep that layer free of Facet TUI/Lattice dependencies so it can be lifted into `probe-core`, then propose it upstream to Probe. |

The shared layer stays independent of the Facet TUI and Lattice so it can move into `probe-core` without forking the engine.

**Still not this product:** collection-in-Lattice, a writer daemon,
kitchen-sink MCP writes, a JS test runner, Probe-shaped git hosting UI,
`facet-record` large-variant, `history --follow` / FTS5 (later).

### Configuration

```toml
# .facet/config.toml (workspace) | ~/.config/facet/config.toml (machine)
# Precedence: CLI flag > workspace file > machine file > built-in default.
[lattice]
inline_body_max   = "64KiB"     # KiB, MiB, GiB, or bytes; "0" means every body is a blob
history_retention = "unlimited" # or "30d", "90d"
wal               = true
busy_timeout_ms   = 5000
```

Flags: `--inline-body-max`, `--history-retention`. Agents get flags; humans
get files.

### Secrets at rest

Environment values that are secrets (tokens, keys) never sit in plaintext in
the machine store. Two backends, picked by environment:

- **OS keyring** (default, platform): the `keyring` crate (macOS Keychain,
  Windows Credential Manager, Linux Secret Service). The `environments` row
  stores a `secret_ref` of the form `kr:<user>`; `value` is NULL. The secret
  itself lives in the keyring under the `facet` service.
- **Encrypted** (headless/CI fallback): XChaCha20-Poly1305 with the master key
  derived from `FACET_SECRET_KEY` via HKDF-SHA256. `secret_ref` stores
  `enc:v1:<base64(nonce||ciphertext)>`; `value` is NULL.

Selection: `FACET_SECRET_KEY` set and non-empty → encrypted; otherwise the OS
keyring. There is never a passphrase prompt (it breaks agent use). If no
keyring backend is available and `FACET_SECRET_KEY` is unset, `set_environment`
fails with a clear error telling the user to set `FACET_SECRET_KEY`.

Reading is by the `secret_ref` prefix, so a secret written under one backend
stays readable under the other as long as its key is available. Non-secret
values stay in `value` with `secret_ref` NULL. Replacing a secret with a
plain value (or deleting the row) cleans up the old keyring entry.

`MachineStore::{set_environment, environment, environments,
delete_environment}` are the API; `environments` returns metadata only
(name, key, `secret`, `updated_at`) and never a secret value.

### Concurrency

Connections open in WAL mode with `busy_timeout` (5 s default) and use short
immediate write transactions. Readers never block. Schema creation runs inside
one immediate transaction so concurrent first opens serialize.
`crates/lattice/tests/contention.rs` is the N-writer, M-reader fixture.

### Engines

The default engine is **rusqlite** (bundled SQLite). The run-history library is
published on crates.io as [`facet-lattice`](https://crates.io/crates/facet-lattice)
0.5.9 with `default = []` and an optional `lattice-duckdb` analytics feature.

| Feature | Engine | Scope |
| --- | --- | --- |
| `lattice-duckdb` | Bundled DuckDB | Optional analytics smoke against SQLite files. A heavy native build; not part of the default installation. |

For configuration, file safety, supported SQL, and rollback, see
[Lattice engines](LATTICE-ENGINES.md). Canonical OpenCollection YAML remains
independent of all database engines.

The external DuckDB analytics path remains available for SQLite stores:

```sh
duckdb -c "INSTALL sqlite; LOAD sqlite; \
  ATTACH '/path/to/.facet/lattice.db' AS lattice (TYPE SQLITE); \
  SELECT status, count(*) FROM lattice.runs GROUP BY status;"
```

`INSTALL sqlite` downloads an extension. Do not attach a live Turso database
through SQLite/DuckDB; cross-engine access is not a supported conversion or
concurrency mechanism. Historical engine evaluation notes describe the older
libSQL smoke and do not establish compatibility of the Rust Turso backend.


## Commands

For verified h3s client-certificate requests shared by CLI, replay, TUI and MCP,
see [Cluster transport](CLUSTER-TRANSPORT.md). Configure credentials through an
explicit project kubeconfig, separately from canonical collection YAML.

```text
facet request run <path> <selector> [<probe request run flags>] [--no-record] [--no-subprocess] [--allow-file-outside-root] [--tag <tag>]... [--expect <codes>] [--dry-run] [--inline-body-max <size>] [--json]
facet history [<path>] [--limit <n>] [--request <selector>] [--status <code>] [--actor <name>] [--since <unix-ms>] [--session <id>|current] [--environment <name>] [--tag <tag>]... [--hash <sha256>] [--bodies] [--json]
facet history [<path>] --id <ulid> [--bodies] [--json]
facet last [<path>] [--request <selector>] [--status <code>] [--actor <name>] [--since <unix-ms>] [--session <id>|current] [--environment <name>] [--tag <tag>]... [--hash <sha256>] [--bodies] [--json]
facet pin <runId> --as <name> [<path>] [--json]
facet pin get <name> [<path>] [--json]
facet pin list [<path>] [--json]
facet pin delete <name> [--json]
facet history [<path>] --sql "<query>" [--json]
facet session start [--actor <name>] [--meta <json>] [--json]
facet session end [<id>|current] [--json]
facet session list [--limit <n>] [--actor <name>] [--open] [--json]
facet session show <id>|current [--json]
facet replay <runId> [<path>] [--frozen] [--environment <name>] [--var k=v]... [--tag <tag>]... [--expect <codes>] [--strict-variables] [--no-record] [--no-subprocess] [--allow-file-outside-root] [--json]
facet diff <idA> <idB> [<path>] [--bodies] [--json]
facet env set [<path>] --environment <name> --name <key> (--value <value> | --value-stdin) [--secret] [--json]
facet env list [<path>] [--environment <name>] [--json]
facet env delete [<path>] --environment <name> --name <key> [--json]
facet blob <hash> [<path>] [--output <file>] [--json]
facet gc [<path>] [--history-retention <r>] [--yes] [--json]
facet doctor [--probe] [--json]
facet theme check <path> [--json]
facet theme list [--json]
facet tui [<path>] [--appearance graphite|porcelain] [--theme <name|path>]
facet mcp
```

Graphite Honey is the TUI default. `:theme` (bare) toggles Porcelain Honey;
`:theme graphite|porcelain` sets one. `:theme <name|path>` loads a theme
file (see [Theme files](#theme-files)); an invalid file falls back to the
built-in for the current appearance and names the field in the footer.
`:history` opens the run grid
(`STARTED STATUS MS METHOD REQUEST ACTOR ID`): `j`/`k` or arrows move,
`gg`/`G` jump, Enter hydrates the response pane from Lattice (titled
`run <id> · replayed view`), `y` yanks the run id and `Y` the response
body hash (OSC 52), `/` filters by selector, `s` toggles runs from
`$FACET_SESSION` only, Esc closes. Under the header, a sparkline paints
the last ≤24 statuses of the focused row's selector (oldest → newest,
palette status buckets, stone for unrecorded) — paint, not a verb.
Bare `:env` opens the environment editor over the machine store —
metadata only (environment, name, secret flag, updated; values are never
shown): `a` adds an entry (environment → name → secret? → value, secret
input masked), `e` re-sets the selected row's value, `d` deletes after a
`y` confirm. `ctrl+u` / `ctrl+d` scroll half a page in the focused pane,
the history grid, and the env overlay. `:sql <query>` opens a read-only
overlay. `?` lists the rest.

Roadmap (Probe's public list, folded): HTTP live; user-defined theme files
live (`facet-tui`, see [Theme files](#theme-files)); next WebSocket, GraphQL,
gRPC streaming, git integration. Secret storage
is already in the Facet machine store. See [IMPLEMENTATION_PLAN.md](../IMPLEMENTATION_PLAN.md).

`<path>` for `history`, `blob`, and `gc` is any file or directory inside the
workspace (default: the current directory); Facet walks up to the nearest
`.facet/lattice.db`. `--json` and `--quiet` follow the upstream rules.

### `request run`

Executes exactly as `probe request run` (same resolution, engine, `--output`
streaming, Ctrl-C cancellation) and returns the upstream JSON with one
additive field:

```json
"lattice": {
  "recorded": true,
  "runId": "01JAB…",
  "workspaceId": "01JAA…",
  "indexed": true,
  "requestHash": "9f86d0…",
  "responseBody": { "sizeBytes": 12, "hash": null, "retention": "inline" }
}
```

When not recorded: `{"recorded": false, "reason": "disabled" | "stdin_workspace" | "error", "message": "…"}`.
A stdin (`-`) workspace has no root and is never recorded. A Lattice failure
never changes the run's exit code; it is reported in `lattice` and on stderr.
A transport failure is recorded with `status: null` and the error text, then
the upstream error envelope is returned with `error.details.lattice`.

### `--expect` and `--dry-run`

`--expect <codes>` on `request run` and `replay` is the assertion an agent
asks for after a real run: a comma list of status codes (`200,201`) and/or
class shorthands (`2xx`, `3xx`), mixable. Absent means no assertion (today's
behavior). `--expect 2xx` is what skill files should teach.

| Outcome | Exit | Category | Agent should |
| --- | ---: | --- | --- |
| status in set | 0 | — | continue |
| HTTP completed, status not in set | **1** | `expect_failed` | stop, read the run, do not retry |
| transport / timeout / cancel | 6 | `network_execution` / `request_timeout` / `request_cancelled` | retry (bounded) |
| config / env / secret missing | 5 | as today | fix inputs |
| Lattice write failed | 0 + `lattice.recorded: false` | — | continue; warn |

Why 1 and not 6: a harness mapping "6 → retry" (right for sockets) would
retry a 404 forever, and one that parses the category first has reintroduced
the JSON parse `--expect` exists to remove. Unix already gave `1` to "the
assertion you asked for is false" (`test`, `grep`, `diff`, `cmp`); upstream's
constants start at 2, so nothing collides. The same row is offered to Probe
for `request run`; if upstream declines, `--expect` stays a Facet extension.

On a miss with `--json` the **full success document** is emitted plus
`error`, so the run id is in hand without a second call:

```json
{ "schemaVersion": 1, "request": { "…": "…" }, "response": { "status": 500, "…": "…" }, "lattice": { "runId": "01K…", "…": "…" },
  "error": { "category": "expect_failed", "exitCode": 1, "message": "expected status in [200,201], got 500",
             "details": { "expected": [200, 201], "actual": 500 } } }
```

Human mode prints the response as usual and `error[expect_failed]: …` on
stderr. `--quiet` prints nothing and exits 1. Lattice records regardless and
tags the run `expect:fail`, so `history --tag expect:fail --session current`
answers "what failed this session" without `--sql`. Transport failures keep
their own envelope and exit code; `--expect` never rewrites them.

`--dry-run` resolves the request (secret hydration included) and shows what
would be sent, without sending or recording. It cannot be combined with
`--expect` or `--output`. The preview applies Lattice's redactions
(sensitive header names, URL userinfo, known secret values), so it is safe to
paste; `lattice.requestHash` is what a real run would store, so an agent can
compare it against a recorded run (`history --id`, `diff`) before firing.

```json
{ "schemaVersion": 1, "dryRun": true,
  "request": { "method": "POST", "url": "http://…/echo", "query": [ { "disabled": false, "name": "mode", "value": "cli" } ],
               "headers": [ { "disabled": false, "name": "X-Probe", "value": "…" } ],
               "body": { "content": "{\"source\":\"cli\"}", "sizeBytes": 16, "omitted": false, "omissionReason": null } },
  "lattice": { "recorded": false, "reason": "dry_run", "requestHash": "9f…", "secrets": { "hydrated": [], "source": null } } }
```

### `history`

Metadata only, newest first, default limit 50. Each run:

```json
{
  "id": "01JAB…", "startedAt": 1757160000123, "durationMs": 128,
  "requestPath": "items/0", "requestHash": "9f86d0…", "environment": "local",
  "method": "POST", "url": "http://…", "status": 200, "error": null,
  "actor": "human", "sessionId": null, "tags": [],
  "request":  { "headers": [ { "disabled": false, "name": "X-Probe", "value": "…" } ],
                "body": { "sizeBytes": 16, "hash": null, "retention": "inline" } },
  "response": { "headers": [ { "name": "content-type", "value": "application/json" } ],
                "contentType": "application/json",
                "body": { "sizeBytes": 12, "hash": null, "retention": "inline" } }
}
```

`retention` is `inline`, `blob`, or `none`. With `--bodies`, inline bodies add
the upstream body fields (`content`, `encoding`, `omitted`, `omissionReason`);
blob bodies are always omitted with `omissionReason: "blob"` (pull them with
`blob <hash>`). The envelope carries `workspace: { id, path }`, or `null` with
`runs: []` when no store exists.

Filters are query-side and AND together. `--session <id>` matches
`sessionId`; `--session current` resolves `FACET_SESSION` and fails with
`session_not_set` (exit 5) when it is unset. `--environment <name>` matches
the environment the request was resolved with. `--tag <tag>` may repeat;
every tag must be present (a run with no tags never matches). `--hash <sha256>`
is column-agnostic: it matches `requestHash`, the request body hash, or the
response body hash (case-insensitive), and each returned row gains
`"matchedHash": "request" | "requestBody" | "responseBody"` saying which
column hit. `matchedHash` is absent when `--hash` is not given.

### `history --id`

One run by id, as one object rather than a one-element list:

```json
{ "schemaVersion": 1, "workspace": { "id": "…", "path": "…" }, "run": { …same run shape as history… } }
```

`--bodies` behaves exactly as on the list. `--id` cannot be combined with
any filter or with `--sql` (`invalid_arguments`, exit 2). An unknown id, or
no store at all, is `run_not_found` (exit 4). This is the seam replay and
diff stand on: an agent keeps run ids, not bodies.

Rows carry two lineage fields from migration 0003: `replayedFrom` (the run
this one replayed, else `null`) and `varNames` (the `--var` names used at
resolve time, never values; `[]` when none, `null` on rows recorded before
0003, meaning unknown).

### `last` and `pin`

`facet last` is `history` with `--limit 1`: the same filters, the newest
match. Human mode prints the bare ULID, so
`facet replay $(facet last --status 500)` is a one-liner; `--json` is the
`history --id` shape (`{ workspace, run }`). No match, or no store, is
`run_not_found` (exit 4), so a shell substitution never yields an empty id.

`facet pin <runId> --as <name>` names a run: the first real write to the
machine store's `preferences` table, key `pin.<name>`, value
`{ runId, workspaceId, pinnedAt }`. The run must exist in the workspace
store discovered from `<path>` when pinned. Pins live in the **machine**
store so they survive `gc` of the workspace; `pin get` prints the bare id
(`facet replay $(facet pin get auth-ok)`) and its JSON reports
`dangling: true` when the run is gone, `false` when it is there, `null` when
the workspace under `<path>` is not the pin's. `pin list` returns
`{ pins: [ … ] }`; `pin delete` reports `deleted`. Names are 1-64 characters
of letters, digits, `-`, `_`, `.`. Unknown pin: `pin_not_found` (exit 4).
`SELECT key, value FROM preferences WHERE key LIKE 'pin.%'` is the query
against the machine store file.

### `history --sql`

Runs one read-only statement against the workspace store on a read-only
connection with `query_only` set:

```json
{ "schemaVersion": 1, "workspace": { "id": "…", "path": "…" },
  "columns": ["request_path", "status"], "rows": [["items/0", 200]] }
```

BLOB columns render as `{ "type": "blob", "sizeBytes": n }`. Writes fail with
`sql_read_only`; parse failures with `invalid_sql` (both exit 2). A missing
store is `lattice_not_found` (exit 9).

### `session`

Sessions are the unit a harness turns on and off around a run of work. They
live in the machine store (cross-workspace); `request run` stamps
`FACET_SESSION` on every recorded run.

`session start` prints the new ULID alone in human mode, so
`export FACET_SESSION=$(facet session start)` needs no `jq`. The actor is
`--actor`, else `FACET_ACTOR`, else `human`. `meta` is pointers only:
`tool: { workspace, tab, pane }` from host tool environment variables when set
(`*_WORKSPACE_ID`, `*_TAB_ID`, `*_PANE_ID`), `cwd`, then the `--meta` object
merged on top (it wins). Never transcripts, never secrets.

```json
{ "schemaVersion": 1,
  "session": { "id": "01K…", "actor": "agent.alpha", "startedAt": 1757250000123, "endedAt": null,
               "meta": { "tool": { "workspace": "w1", "tab": "w1:tR", "pane": "w1:pR" }, "cwd": "/…/repos/facet" },
               "runs": 0 } }
```

`runs` counts the session's runs in the workspace store discovered from the
current directory; it is omitted when no store is found there. `show` and
`end` return the same `session` object; `end` adds `"alreadyEnded": true|false`
and is idempotent (`endedAt` never moves). `list` returns
`{ "sessions": [ …session… ] }`, newest first, default limit 50; `--open`
keeps only sessions with `endedAt: null`. `<id>` may be `current`
(`FACET_SESSION`); `session end` with no id uses `FACET_SESSION` too. An
unknown id is `session_not_found` (exit 4); no `FACET_SESSION` when one is
needed is `session_not_set` (exit 5).

Mint-if-missing: when `FACET_SESSION` names a session no `session start`
created (a harness invented the id), the first recorded run inserts the row
with that run's actor and no `meta`. Later runs never rewrite it. This is
best effort like machine-store indexing and never fails the run.

### `replay`

Re-sends a recorded run. **Replay truth is the current YAML plus the recorded
environment**: the row supplies the selector and environment name (`--environment`
overrides), the collection at `<path>` (file or directory, default `.`, as for
`request run`) supplies the request, and the Lattice store is discovered by
walking up from it. Values passed with `--var` are never stored, so recorded
`--var` names are a warning (`run … was recorded with --var token; pass them
again`), never replayed.

The resolved request is hashed before any network. When it differs from the
recorded `requestHash`: without `--frozen` the run executes with a stderr
warning and `lattice.hashChanged: true`; with `--frozen` it is refused with
`replay_changed` (exit **1**), no network, no row, and
`details.{replayedFrom, recordedHash, currentHash}`. There is no
"send stored bytes" mode: stored headers are redacted, auth enters the hash by
scheme only, and request bodies are blobs that may hold secrets.

The output is `request run`'s document; `lattice` gains two fields:

```json
"lattice": { "recorded": true, "runId": "01K…", "…": "…", "replayedFrom": "01J…", "hashChanged": false }
```

The new row carries the recorded tags plus `--tag`, `replayedFrom`, and the
`--var` names. Unknown run id: `run_not_found` (exit 4), with
`details.workspaceId` / `workspacePath` when the machine index knows the run
under another workspace. A selector no longer in the YAML is upstream's
`request_not_found` (exit 4) with `details.replayedFrom`. No store: `lattice_not_found` (exit 9).

### `diff`

Hash-first comparison of two recorded runs. Metadata compares `method`,
`url`, `status`, `error`, `durationMs`, `environment`, `actor`,
`requestHash`, `request.headers`, `response.headers`, `request.body.hash`,
`response.body.hash`, `response.contentType`, and `tags`. Headers compare
as sorted `name: value` lists; they are already redacted, so a token change
shows as no change (and `requestHash` does not move either, since auth
enters it by scheme only).

```json
{ "schemaVersion": 1, "workspace": { "id": "…", "path": "…" },
  "a": { "id": "01K…A", "startedAt": 1757250000123, "status": 500 },
  "b": { "id": "01K…B", "startedAt": 1757250001456, "status": 200 },
  "equal": false,
  "changes": [ { "field": "status", "a": 500, "b": 200 }, { "field": "durationMs", "a": 41, "b": 38 } ],
  "request":  { "hash": { "a": "9f…", "b": "9f…", "equal": true },
                "body": { "a": { "hash": "…", "sizeBytes": 16, "retention": "blob" }, "b": { "…": "…" }, "equal": true, "text": null, "omissionReason": null } },
  "response": { "body": { "a": { "hash": "ad…", "sizeBytes": 812, "retention": "inline" }, "b": { "…": "…" }, "equal": false, "text": null, "omissionReason": null } } }
```

`changes` lists only differing fields, named by their `history` JSON path.
`durationMs`, `actor`, and `tags` are always compared and reported but
**never** flip `equal`: they are provenance, and `equal` is about bytes and
outcome (so a replay tagged `--tag retry` still diffs equal to its source
when the response matched). Inline bodies are read and hashed so `hash` is
present for them too. With `--bodies`, differing inline UTF-8 response
bodies add `response.body.text`, a unified diff (`--- a`, `+++ b`, three
lines of context); otherwise `omissionReason` says `blob` (pull with
`blob <hash>`), `binary`, `too_large`, or `none`. Request bodies are
hash-only and never diffed as text.

Exit **0** equal, **1** different, **4** a run missing
(`run_not_found`, `details.missing: ["01K…"]`), **9** no store. `--quiet`
keeps the exit code.

### Secret hydration and `env`

Three layers, and the order is the design:

1. OpenCollection YAML declares variables, plain or `secret: true`. Core
   refuses to interpolate a declared secret with no value:
   `secret_variable_unavailable` (exit 5). That stays the floor.
2. The Facet machine store holds values keyed `(workspace, environment, key)`,
   plain or through the secrets layer. `facet env set` writes them.
3. `--var` is the runtime override channel. Last one wins.

On `request run`, `replay`, and a TUI send, when an environment is selected,
Facet overlays layer 2 as overrides **before** the user's `--var`: only for
variable names the request references (`discover_request_variables`), never
the whole environment, never an empty value, never the name. A user `--var`
for the same name is used instead and the Lattice value is not read. YAML-only
runs (no `--environment`) are untouched and carry no `secrets` field.

`lattice.secrets` reports names only:

```json
"lattice": { "…": "…", "secrets": { "hydrated": ["token"], "source": "keyring" | "encrypted" | "plain" } }
```

`source` is the secrets backend when a secret was read, `plain` when only
plaintext rows were used, `null` when nothing was hydrated. Human:
`… · hydrated 1 secret from encrypted`. Hydrated names are **not** `varNames`
(those are the user's `--var`).

Failure rules: a referenced variable declared `secret` whose backend cannot
serve it (no keyring and no `FACET_SECRET_KEY`, empty or wrong key, keyring
error) is `secret_backend_unavailable` (exit 5, `details.variable`) before
any network. When no referenced variable is declared secret, backend
problems are silent and the YAML value applies. A Lattice row for a name the
request does not reference is ignored.

`githubToken` from `gh`: when a request's `Authorization` header references
`{{githubToken}}` and no non-empty variable or `--var` supplies it, `request
run` and `replay` ask `gh auth token` for it. The token is sent only to
`https://api.github.com` (host compared ignoring ASCII case, port 443 or
none, no userinfo). For any other rendered URL, including one that does not
parse, the command fails with `github_token_host_refused` (exit 5,
`details.host` when the URL has one) before `gh` runs and before anything is
sent; `--dry-run` refuses the same way. The error names the host, never the
token. A request carrying the token is a secret request: it is sent with the
locked transport (no redirects, no env proxy), and it is locked once it
passes the host check, before `gh` is asked. A `--var githubToken=…` is the
user's value and is sent as given.

#### Child processes

Facet starts two programs, and only through one place: `gh` (`gh auth
token` for `{{githubToken}}`, and `gh api` as a fallback when a native send
to `https://api.github.com` fails) and `git` (the `git:<sha>` auto-tag on a
recorded run).

- **Path.** `gh` is the first regular file in `/opt/homebrew/bin/gh`,
  `/usr/local/bin/gh`, `/usr/bin/gh`; `git` the first in `/usr/bin/git`,
  `/opt/homebrew/bin/git`, `/usr/local/bin/git`. `FACET_GH` / `FACET_GIT`
  replace the list with one absolute path to an existing regular file.
  `PATH` is never searched. A relative or missing override fails
  `subprocess_path_refused` (exit 5) before spawn, and the error never
  names the path. With no override and no program at a listed location,
  `{{githubToken}}` gets no token and a recorded run no `git:` tag, as
  before; `PATH` is not searched instead.
- **Environment.** Cleared, then `HOME` and `GH_CONFIG_DIR` when set, and
  `PATH=/usr/bin:/bin`. Nothing else: no `FACET_*`, no `GH_TOKEN`, no proxy
  variables, no `SSH_*`. Stdin is null, except the body of an unlocked
  `gh api --input -`. No value the run knows is secret reaches a child's
  argv, stdin or env.
- **Switch.** `--no-subprocess` on `request run` and `replay`, or a
  non-empty `FACET_NO_SUBPROCESS` (which the TUI and MCP honour too), starts
  no child at all. `{{githubToken}}` with no `--var githubToken=…` then
  fails `subprocess_disabled` (exit 5) before anything is sent, after the
  host check; the `gh api` fallback stays off, so the native error stands;
  and a recorded run has no `git:` tag. `facet doctor` shows whether the
  switch is on.
- **`gh api` fallback.** Only for an open send to `https://api.github.com`
  (the same origin rule as the token). Never for a locked send. A header
  named in the sensitive list, or one whose value holds a known secret,
  never goes on the argv of `gh`.

#### File bodies stay under the collection root

Before anything is sent, on `request run`, `--dry-run`, `replay`, the TUI
and MCP, every file Facet would send is checked:

- the selected file of a `type: file` body;
- every enabled `type: file` part of a `multipart-form` body.

Each path is resolved the way the HTTP engine resolves it: relative to the
directory beside the collection, or as given when absolute. It is then
canonicalized, so `..` and symlinks are followed. It must be a regular file
inside the canonical collection directory, compared by path component (a
sibling `/a/bc` is not inside `/a/b`). The request is then sent with that
canonical path.

Anything else fails `file_body_outside_root` (exit 5) before the send. That
covers a path outside the collection directory (through `..`, an absolute
path or a symlink), a missing file, or a collection read from stdin, which
has no directory. `details.request` names the request and `details.reason`
is `outside_root`, `missing` or `no_base`; the error never names the path.

`--allow-file-outside-root` on `request run` and `replay` lets a file
outside the collection directory through, still canonicalized; a missing
file is still refused. The TUI and MCP have no such switch and always
refuse. The recorded request and its `requestHash` keep the path as the
collection wrote it.

Residual: the check and the HTTP engine's open are two separate steps.
Someone who can write inside the collection directory can replace the
checked file, or a directory on its path, with a symlink to a file outside
between the check and the open, and that file is then sent. Facet does not
change the HTTP engine (`probe-http`) to open the checked file itself, so
this window stays. Keep collection directories writable only by people
trusted to choose what they send.

`facet env` is Facet's machine-store CLI, not Probe's `environment` (which
edits YAML and rejects secrets). `set` stores one value (`--secret` routes it
through the secrets layer and fails with `secret_backend_unavailable` when no
backend can hold it); it creates the workspace store beside `<path>` when
none exists.

`--value-stdin` reads the value from a pipe instead of argv, where other
processes on the machine can read it: `pass show api/token | facet env set
--environment local --name token --value-stdin --secret`. One trailing `\n`
or `\r\n` is stripped. It fails `invalid_arguments` (exit 2), with nothing
it read in the error, when stdin is a terminal (there is no echo-off
prompt), when the value is empty or not UTF-8, and when `--value` is given
too. `--value` with `--secret` still works and prints one `warning:` line on
stderr that names `--value-stdin`; the warning never holds the value.

`list` and `set` return metadata only:

```json
{ "schemaVersion": 1, "workspace": { "id": "…", "path": "…" },
  "entries": [ { "environment": "local", "name": "token", "secret": true, "updatedAt": 1757250000123 } ] }
```

There is no field for the value and there never will be. **Never in
`history --json`, `--sql`, `session`, or `env list`:** Lattice environment
values, `--var` values for declared secrets, `FACET_SECRET_KEY`, or `kr:` /
`enc:v1:` references. Facet knows the values of declared secrets at record
time (hydrated or passed with `--var`), so it scrubs them from the stored
URL, request and response header values, request bodies, and transport
error text before the row is written (`<redacted>`; values shorter than 4 bytes are left
alone). The live `request run` and `replay` documents show what was
sent, with secrets and sensitive headers redacted: the same known values
(plus any token the run sends, such as `githubToken` from `gh`) and their
URL-encoded forms become `<redacted>` in the request and final URL, every
header value and body, error envelopes and `error[...]` lines, human and
`--json` alike; URL userinfo is stripped; and the values of the sensitive
headers (`Authorization`, `Proxy-Authorization`, `Cookie`, `Set-Cookie`,
`X-Api-Key`, `X-Auth-Token`, `Api-Key`, `X-Amz-Security-Token`) are always
`<redacted>`. `--dry-run` previews through the same redaction. There is no
flag to reveal them.
`--sql` opens the workspace store only; the machine store (where
`environments` lives) is never attached. Request bodies are scrubbed of
known secrets at write, like the other stored text; response bodies are
stored as received and may hold what the server sent. `facet blob` is the
one place a human knowingly opens a body.

Secret values in memory: Facet keeps the values it hydrates, the `--var`
overrides, the values it redacts, the `githubToken` from `gh`, the values
the machine store decrypts, and the `--value-stdin` buffer in buffers that
are zeroed when they are dropped. Residual: to render a request, Facet
hands `probe-core` plain copies of the merged variables, and the resolved
request holds rendered values (URL, headers, body) in `probe-core` types
that are not zeroed. The redactor that scrubs output keeps its own plain
copies of the known values. Neither are the copies the HTTP client, TLS
and the operating system make while sending zeroed. Zeroing is best effort: it shortens
how long a value stays in freed memory, and it does not protect against a
process that can read Facet's memory while it runs. `probe-core` is not
changed to hold zeroed values.

### `doctor`

Preflight. Reports, never fixes. Exit **0** healthy, **1** warnings, **9**
when the machine store cannot be opened. `--probe` round-trips a throwaway
secret through the secrets layer and sets `secrets.usable`; without it
usability is `null`. No value, `secret_ref`, or `FACET_SECRET_KEY` is
printed. Human lists warnings and a `child processes: on|off
(FACET_NO_SUBPROCESS)` line; JSON is `machine` / `workspace` / `secrets` /
`env` (presence of `FACET_*` only; `env.FACET_NO_SUBPROCESS` is true when
the variable is non-empty, which turns child processes off).

### `blob`

Writes the raw bytes to stdout. With `--json`:

```json
{ "schemaVersion": 1, "blob": { "hash": "…", "sizeBytes": 12, "contentType": "application/json",
  "body": { "content": "…", "encoding": "utf8", "omitted": false, "omissionReason": null, "outputPath": null } } }
```

`--output <file>` writes the bytes to a file and returns `outputPath`. Bodies
over 16 MiB are omitted from JSON with `too_large`. Unknown hashes are
`blob_not_found` (exit 4).

### `gc`

Mark-and-sweep, manual, dry run by default. Runs older than
`history_retention` expire; blob files no surviving run references are
orphans. Nothing is deleted without `--yes`.

```json
{ "schemaVersion": 1, "workspace": { "id": "…", "path": "…" }, "applied": false,
  "retention": "unlimited", "runsExpired": 0,
  "orphans": [ { "hash": "…", "sizeBytes": 3 } ], "registryOrphans": 0, "bytesReclaimable": 3 }
```

### Theme files

`facet tui` reads versioned, human-editable theme files (TOML) from
`<config dir>/themes/<name>.toml` — the config dir is `FACET_CONFIG_DIR`
or the platform config dir (`~/.config/facet` on Linux). Theme files are
local presentation configuration: never collection data, never in Lattice.

```toml
version = 1                   # required; this facet reads version 1 only
name = "midnight-honey"       # optional; defaults to the file stem
extends = "graphite"          # required built-in base: graphite | porcelain

[colors]
accent = "#e7821b"            # any subset of tokens; "#rrggbb" only
window_bg = "#101012"
```

Rules (the Theme files contract in this document): missing tokens merge with the
built-in base; unknown top-level keys are tolerated so the format can grow
additively inside a version; an unsupported `version`, an unknown
`[colors]` token, or an invalid value rejects the **whole file** — the TUI
keeps (or restores) the complete built-in for the current appearance and
names the file and field in the footer, and `facet theme check` exits 1
(`theme_invalid`) with `details.{path, field}`. Built-ins are always
available as `graphite` / `porcelain`. At 256-color and 16-color depths a
custom theme resolves through the built-in role channels, so method and
status families stay distinct.

Apply one with `facet tui --theme <name|path>` or `:theme <name|path>` in
the TUI (a bare name resolves in the themes directory; anything with a
path separator or a `.toml` suffix is read as a path). `facet theme check
<path>` validates without a terminal; `facet theme list` shows built-ins
plus every discovered file, invalid ones listed with their error.

The `[colors]` tokens, grouped as in `Palette`:

| Group | Tokens |
| --- | --- |
| Surfaces | `window_fg` `window_bg` `sidebar_fg` `sidebar_bg` `editor_fg` `editor_bg` `raised_fg` `raised_bg` `overlay_fg` `overlay_bg` |
| Text | `text_primary` `text_secondary` `text_muted` `text_placeholder` `text_inverse` |
| Borders | `border_subtle` `border_standard` `border_strong` `border_focused` |
| Accent | `accent` `accent_hover` `accent_pressed` `accent_disabled` `accent_disabled_fg` |
| Selection | `selection_active_bg` `selection_active_fg` `selection_inactive_bg` `selection_inactive_fg` |
| Status | `status_success` `status_warning` `status_error` `status_informational` |
| HTTP methods | `method_get` `method_post` `method_put` `method_patch` `method_delete` `method_other` |
| Response status | `response_informational` `response_success` `response_redirect` `response_client_error` `response_server_error` |
| Syntax | `syntax_property` `syntax_string` `syntax_number` `syntax_boolean` `syntax_null` `syntax_punctuation` |

### Agent skill

The recommended way for a coding agent to use Facet is
[`.agents/skills/facet`](../.agents/skills/facet/SKILL.md): `SKILL.md` plus
`facet.nu`, which calls this CLI with `--json` after the subcommand. The
same bytes are copied to `.claude/skills/facet/`, `.grok/skills/facet/`, and
`.cursor/skills/facet/` by `scripts/sync-skills`. The wrapper is read-only.
It resolves a collection (directory, `opencollection.yml` / `opencollection.yaml`,
or a path inside it; otherwise it walks up for those files or `.facet/lattice.db`),
`cd`s there, and returns `ok: false` without calling `facet` when that fails.
It exports `FACET_ACTOR`, using the CLI default `human` when the variable is
unset or empty. On failure the envelope includes `exit` and `stderr`. It
covers `request list`, `request get`, `history` (no `--bodies`), `history --id`
(no `--bodies`), `history --sql` (read-only; secret columns are NULL; writes fail with `sql_read_only`),
`last`, `session list`, `session show`, `pin list`, `env list` (names only),
`diff` (no `--bodies`), `ncl check`, `ncl export`, and `doctor`. It does not
wrap `request run` (including `--dry-run`), `replay`, `ncl apply`,
`session start`, `session end`, `blob`, `env set`, `env delete`, or pin create
and delete. `facet mcp` is unchanged; see below.

### `mcp`

`facet mcp` serves the Facet commands as Model Context Protocol tools over
stdio (JSON-RPC 2.0, one message per line; no HTTP transport in v1). It is a
typed adapter over the **same functions** the CLI calls (`lattice`,
`facet-record`, and the command functions themselves); it never spawns
`facet` and never parses terminal output. Every tool result is the existing
`schemaVersion: 1` document, byte for byte what `facet <command> --json`
prints, delivered as `structuredContent` (and as pretty text in `content`).
Errors are the same `error.{category, exitCode, message, details}` envelope
with `isError: true`; a success document that carries `error` (an `--expect`
miss) is also `isError: true`. There are no MCP-only fields.
`request_run` and `run_replay` return the same redacted document as the CLI.
`history_get`, `run_diff` and `blob_get` return store data unchanged: they
are store-equivalent, showing exactly what Lattice holds, which was
redacted when the run was recorded (response bodies are stored as
received; see above).

| Tool | Arguments | Same as |
| --- | --- | --- |
| `session_start` | `actor?`, `meta?` | `session start` |
| `session_end` | `id?` (or `current`) | `session end` |
| `request_list` | `path` | `request list` (Probe, in-process) |
| `request_get` | `path`, `selector`, `environment?`, `strictVariables?` | `request get` (Probe, in-process) |
| `request_run` | `path`, `selector`, `environment?`, `var?`, `tag?`, `expect?`, `dryRun?`, `strictVariables?`, `noRecord?` | `request run` |
| `history_list` | `path?`, `limit?`, `request?`, `status?`, `actor?`, `since?`, `session?`, `environment?`, `tag?`, `hash?`, `bodies?` | `history` |
| `history_get` | `id`, `path?`, `bodies?` | `history --id` |
| `blob_get` | `hash`, `path?` | `blob` |
| `run_diff` | `a`, `b`, `path?`, `bodies?` | `diff` |
| `run_replay` | `id`, `path?`, `environment?`, `var?`, `tag?`, `expect?`, `frozen?` | `replay` |
| `sql_query` | `sql`, `path?` | `history --sql` (read-only) |

`expect` is a string spec (`"2xx"`, `"200,201"`) or an array like
`[200, "3xx"]`. `var` is an object of overrides. **MCP never takes secret
values as arguments:** a `var` whose name is stored in the Facet machine
store for that workspace is refused (`invalid_arguments`) before anything
runs; hydration supplies it, `facet env set` changes it. Not in v1: collection
writes, `env set`, `gc --yes`, resources. Auth inherits the process
environment (`FACET_SECRET_KEY`, keyring, `FACET_ACTOR`, `FACET_SESSION`).

Claude Code (`.mcp.json`):

```json
{ "mcpServers": { "facet": { "command": "facet", "args": ["mcp"], "env": { "FACET_ACTOR": "agent.alpha" } } } }
```

The CLI remains the contract. Prefer the skill in
[`.agents/skills/facet`](../.agents/skills/facet/SKILL.md); it does not add tool schemas to
context. Use `mcp` when a harness defers or filters tool schemas, or when the
client has no shell. MCP still returns `structuredContent` identical to
`facet <command> --json`.

TypeSafe / Jev recipes use this same `request_run` path (1:1 with
`facet request run`). Hydrate `typesafeApiKey` with `facet env set --secret`
from `$TYPESAFE_API_KEY`; never Lattice bodies. `jevShadow` empty / `ask` /
`hold` / `escalate` ≠ approve. See [Jev native transport](jev-native.md).

## Exit codes and error categories

Facet extends the upstream table; it never renumbers it.

| Code | Category |
| ---: | --- |
| 0–8 | As in [CLI](CLI.md#exit-codes) |
| 1 | Assertion failed (`expect_failed`, `replay_changed`; `diff` found differences) |
| 9 | Lattice store failure (`lattice_error`, `lattice_not_found`) |

Additional stable categories: `facet_key_invalid` (exit 3, a bad
[`facet:` key](#the-facet-key)); `blob_not_found`, `run_not_found`,
`session_not_found`, and `pin_not_found` (exit 4, "not found"); `session_not_set`,
`secret_backend_unavailable` and `github_token_host_refused` (exit 5,
configuration); `invalid_sql` and
`sql_read_only` (exit 2). Every JSON document carries `schemaVersion: 1`;
fields are added compatibly and never removed or retyped within a version.

## Tests

- `crates/facet/tests/facet_commands.rs`: one golden file per command under
  `crates/facet/tests/golden/` (`UPDATE_GOLDEN=1` rewrites them), plus
  recording, reader-rule, read-only SQL, gc, session lifecycle,
  mint-if-missing, `history --id`, session / environment / tag / hash
  filters, replay, diff, secret overlay, `env`, `doctor`, `--expect`, and
  `--dry-run` through the binary.
- `crates/lattice/tests/store.rs`: schema, threshold placement, reader rule
  across threshold changes, gc, machine index, environments (plain + secret).
- `crates/lattice/tests/contention.rs`: concurrency fixture.
- `crates/lattice/src/secrets.rs` (unit): encrypted round-trip, wrong-key
  failure, ref-prefix classification, empty/unknown refs.

Facet is compiled and tested on standard CI hosts (Linux and macOS).
