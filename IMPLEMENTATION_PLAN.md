# Implementation Status and Roadmap

This document records Facet's current scope and future product work. It is not
required reading for ordinary implementation tasks; use the task-specific
references in [AGENTS.md](AGENTS.md) and the [documentation index](docs/README.md).

Upstream Probe's planning history informed the inherited core; this file is the
Facet-owned roadmap going forward.

## Current Foundation

### Inherited core (Probe)

The workspace retains Probe-derived shared core and CLI foundations; the legacy
GPUI desktop crate was retired from this Facet tree in Phase F:

- a Rust workspace with separate core, OpenCollection, HTTP, CLI, Postman, and
  Yaak crates; Facet adds its Ratatui TUI and MCP adapter;
- bundled and unbundled OpenCollection loading, validation, retained YAML, atomic
  persistence, external-change detection, and recovery-aware structural writes;
- an indexed in-memory workspace with repository-owned persistent selectors;
- shared environment resolution and management;
- one asynchronous HTTP engine used by both interfaces, with cancellation and bounded
  response handling;
- a deterministic, non-interactive CLI with versioned JSON, stable exit codes, request
  and workspace editing, and Postman and Yaak import;
- performance fixtures and benchmarks for workspaces up to 10,000 requests.

The public CLI contract is documented in [docs/CLI.md](docs/CLI.md). Shared architecture remains in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).
Retired desktop design is historical in [docs/DESIGN.md](docs/DESIGN.md); the
current Facet TUI theme contract is in [docs/FACET.md](docs/FACET.md).
Code and tests remain authoritative when a document falls behind.

### Facet today

Facet-original surface on top of that core (see [docs/FACET.md](docs/FACET.md)):

- the `facet` binary beside `probe` on `PATH` (packages `facet-cli` / `probe-cli`);
- **Lattice** — workspace `.facet/lattice.db` plus a machine store for run history,
  bodies, sessions, and cross-workspace index;
- agent CLI: `history`, `blob`, `gc`, and recording on `request run`;
- Ratatui TUI (`facet tui`) with Graphite Honey / Porcelain Honey; vim-modal keys
  (default); `probe-desktop` was retired from this workspace in Phase F;
- Facet-original crates (`facet`, `lattice`, `facet-tui`) under MIT; upstream-derived
  crates remain Apache-2.0.

## Planned Work

Folded from Probe's public roadmap
([rusty-probe.pages.dev](https://rusty-probe.pages.dev)): HTTP is live; next is
WebSocket, GraphQL, gRPC streaming, custom themes, git integration, secret
storage, and *and more*. Facet ships these in this tree first, including the
shared protocol layer, and proposes the shared pieces to Probe upstream after
they work here.

### Shipped (Facet)

| Probe item | Facet |
| --- | --- |
| 01 HTTP requests | Live. Same engine as Probe. Lattice records every `request run` / TUI send. |
| 05 Custom theme support | **Live.** Graphite Honey (default) and Porcelain Honey, `:theme` toggle, `--appearance`, plus user-defined theme files (`facet theme check\|list`, [docs/FACET.md](docs/FACET.md#theme-files)). |
| 07 Secret storage | **At rest and hydrated.** OS keyring or `FACET_SECRET_KEY` XChaCha20-Poly1305; Lattice env overlay on `request run` / TUI send / `replay` (`facet env`). Probe desktop secret UX remains upstream. |
| ··· And more | **Shipped 2026-09-07** (PRs #8–#13). Sessions, recall, TUI history grid, replay, hash-diff, overlay, `doctor`, `--expect` (exit **1**), `--dry-run`. See [docs/FACET.md](docs/FACET.md#shipped-2026-09-07-and-more). |

### Why Probe 02–04 did not ship

WebSocket, GraphQL, and gRPC are **protocol engines**, not Lattice work. They
need a shared protocol-session / event abstraction, which does not exist yet.
The 2026-09-07 train ranked the week-1 HTTP loop
(send → see → compare → send again) ahead of finishing Probe's numbered list.
Plan: build the protocol session/event layer and the WS/GraphQL/gRPC engines
in Facet first, together with Facet JSONL, the TUI session pane, and Lattice
events. Keep the layer free of Facet TUI/Lattice dependencies so it stays
cherry-pickable into `probe-core`, then propose it upstream as a Probe PR.

### Next (Probe-aligned)

Ship in Facet first. Protocol work (02–04) lands here and is proposed to Probe
afterwards; other shared-core changes are still offered upstream first when they
touch `probe-core` / `probe-cli`.

| # | Item | Facet slice | Upstream |
| --- | --- | --- | --- |
| 02 | WebSocket | TUI session pane + Lattice events + `facet` JSONL | After Facet: propose the protocol session/event layer to `probe-core` |
| 03 | GraphQL | Collection item + TUI editor + history | After Facet: propose the shared operation/variables model |
| 04 | gRPC streaming | Same session adapter as WebSocket | After Facet: propose streaming on the protocol session |
| 06 | Git integration | Auto-tag `git:<sha>[-dirty]` on record; filesystem stays the Git boundary. No lazygit, no host UI. | No provider coupling in core |
| 07 | Secret storage (rest) | TUI env editor on `facet env` / machine store; offer the resolve hook upstream | Probe desktop |

### Facet-only — next slice

Canonical write-up: [docs/FACET.md](docs/FACET.md#next-slice). Product notes
for the closed train stay in the Lapis vault
(`agents/fullstack/notes/2026-09-07-and-more.md` and the explore / deep-dive
passes).

**Facet-owned rest** (ranked; no `probe-core`; finish this train first):

1. MCP — `facet mcp` stdio; tools over lattice + record + run; same JSON; no stdout parse
2. Git HEAD auto-tag — `git:<sha>[-dirty]` on record; no column
3. Bells — `facet last`, `:history` sparkline, pins
4. ~~Theme files — Probe 05 rest~~ (done: `facet theme check|list`)
5. TUI env editor + `ctrl+u`/`ctrl+d` — Probe 07 rest + deferred Surface 2 scroll

**Probe contribution** (later, separate train): `--expect` + `--dry-run` on
`probe request run`; secret-provider hook in `probe-core` (not Lattice);
then the protocol layer (02–04) once it works in Facet. Fresh branch off upstream `main`.

MCP / harness adapter over Lattice must not parse CLI output. Engines stay
rusqlite default; DuckDB ATTACH is `scripts/duckdb-attach-demo.sh`.

### Facet-only (later)

- TUI depth: collections browser, run inspector, vim-modal polish
- Public docs/brand seating and release tagging aligned with workspace version
- `history --follow --jsonl`, fixtures from blobs, watch, FTS5
- `facet-record` large-variant

### Inherited notes (still load-bearing)

#### User-defined theme files

**Shipped.** Versioned, human-editable theme files (TOML) for `facet-tui`.
Parsing and validation stay outside components; invalid themes fall back to
built-ins. Local presentation data, not OpenCollection content.
[docs/FACET.md](docs/FACET.md#theme-files).

#### Streaming protocols

Shared protocol session/event abstraction before WebSocket, SSE, or gRPC, built
in Facet first and designed so it can be upstreamed to Probe later.
Implementations remain independent of stdin/stdout and any UI framework; the CLI
adapts events to JSONL and the TUI renders sessions.

#### Git

Filesystem remains the primary Git boundary. Optional built-in status, diff,
branch, commit, pull, and push later, without coupling collections to a host.

#### MCP

Another adapter over the shared application layer. Must not duplicate business
logic or depend on parsing CLI output.

## Planning Rules

- Add a roadmap item only when it expresses product scope not already documented by
  current behavior or tests.
- Move implemented behavior to its canonical product or architecture document instead
  of retaining a completed phase checklist here.
- Do not use historical phase numbers as dependencies. Describe concrete prerequisites
  and affected architectural boundaries.
- Keep speculative provider integrations, cloud services, accounts, telemetry,
  analytics, plugins, and unsupported protocols out of scope until explicitly approved.
- Facet-only work stays in Facet crates; shared-core fixes are offered upstream first.
  Protocol work (WebSocket, GraphQL, gRPC) is the exception: Facet first, upstream after.
