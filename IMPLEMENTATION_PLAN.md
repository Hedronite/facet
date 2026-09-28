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
storage, and *and more*. Facet ships these independently where we already own
the layer, and contributes shared core upstream. Protocol work in `probe-core`
is written as an upstream PR; Facet TUI/CLI/Lattice adapters land in this tree
in parallel.

### Shipped (Facet)

| Probe item | Facet |
| --- | --- |
| 01 HTTP requests | Live. Same engine as Probe. Lattice records every `request run` / TUI send. |
| 05 Custom theme support | **Partial.** Graphite Honey (default) and Porcelain Honey, `:theme` toggle, `--appearance`. User-defined theme files are still open (see below). |
| 07 Secret storage | **At rest and hydrated.** OS keyring or `FACET_SECRET_KEY` XChaCha20-Poly1305; Lattice env overlay on `request run` / TUI send / `replay` (`facet env`). Probe desktop secret UX remains upstream. |
| ··· And more | **Shipped 2026-09-07** (PRs #8–#13). Sessions, recall, TUI history grid, replay, hash-diff, overlay, `doctor`, `--expect` (exit **1**), `--dry-run`. See [docs/FACET.md](docs/FACET.md#shipped-2026-09-07-and-more). |

### Why Probe 02–04 did not ship

WebSocket, GraphQL, and gRPC are **protocol engines**, not Lattice work. They
need a shared protocol-session / event abstraction in `probe-core` first
(Thread A, parked on `repos/probe-upstream`). Facet's fork rule is
cherry-pickable both ways: `probe-core` / `probe-cli` stay untouched except as
an upstream PR. A Facet-only WS/GraphQL/gRPC stack against an HTTP-only core
would fork the engine. The 2026-09-07 train ranked the week-1 HTTP loop
(send → see → compare → send again) ahead of finishing Probe's numbered list.
Unpark Thread A when Evan says; then core PR upstream, Facet JSONL + TUI
session pane + Lattice events in this tree in parallel.

### Next (Probe-aligned)

Ship in Facet; offer the shared core upstream first when it touches
`probe-core` / `probe-cli`. Thread A stays parked until unparked.

| # | Item | Facet slice | Upstream |
| --- | --- | --- | --- |
| 02 | WebSocket | TUI session pane + Lattice events + `facet` JSONL | Protocol session/event abstraction in `probe-core` (**Thread A**) |
| 03 | GraphQL | Collection item + TUI editor + history | Shared operation/variables model (**Thread A**) |
| 04 | gRPC streaming | Same session adapter as WebSocket | Streaming on the protocol session (**Thread A**) |
| 05 | Custom themes (rest) | Versioned theme files for `facet-tui` | Probe desktop theme files remain upstream; Facet TUI theme rules are in [docs/FACET.md](docs/FACET.md#theme-files) |
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
4. Theme files — Probe 05 rest
5. TUI env editor + `ctrl+u`/`ctrl+d` — Probe 07 rest + deferred Surface 2 scroll

**Probe contribution** (later, separate train): `--expect` + `--dry-run` on
`probe request run`; secret-provider hook in `probe-core` (not Lattice);
then Thread A (02–04) when unparked. Fresh branch off upstream `main`.

MCP / harness adapter over Lattice must not parse CLI output. Engines stay
rusqlite default; DuckDB ATTACH is `scripts/duckdb-attach-demo.sh`.

### Facet-only (later)

- TUI depth: collections browser, run inspector, vim-modal polish
- Public docs/brand seating and release tagging aligned with workspace version
- `history --follow --jsonl`, fixtures from blobs, watch, FTS5
- `facet-record` large-variant

### Inherited notes (still load-bearing)

#### User-defined theme files

Versioned, human-editable theme files after the semantic token model is stable.
Parsing and validation stay outside components; invalid themes fall back to
built-ins. Local presentation data, not OpenCollection content.
[docs/FACET.md](docs/FACET.md#theme-files).

#### Streaming protocols

Shared protocol session/event abstraction before WebSocket, SSE, or gRPC.
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
