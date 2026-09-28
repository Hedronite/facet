# Project Instructions

Facet is a native, local-first API client built with Rust and OpenCollection YAML.
The CLI and Ratatui TUI are adapters over shared application/domain operations;
the Facet MCP surface uses the same application layer.

## Read Only What the Task Needs

After this file, inspect the affected code and read only the matching reference:

| Work | Reference |
| --- | --- |
| Domain, repositories, persistence, HTTP, imports | [Architecture](docs/ARCHITECTURE.md) |
| CLI commands, JSON, selectors, exit codes | [CLI](docs/CLI.md) |
| TUI themes and interaction | [Facet themes](docs/FACET.md) |
| Rust workflow, dependencies, tests | [Development](docs/DEVELOPMENT.md) |
| Errors or logging | [Errors and logging](docs/ERRORS_AND_LOGGING.md) |
| Benchmarks or optimization | [Performance](docs/PERFORMANCE.md) |
| Facet: `facet` binary, Lattice, session/recall/replay/diff/env/`--expect`, fork boundary | [Facet](docs/FACET.md) |
| Future scope | [Roadmap](IMPLEMENTATION_PLAN.md) |

Do not read every document by default. The [documentation index](docs/README.md)
identifies the canonical source for each topic. README is product onboarding, not
required implementation context. For unfamiliar external APIs, inspect the exact pinned source and examples; pinned
revisions are authoritative.

## Priorities

Resolve conflicts in this order: data compatibility, correctness, data safety,
programmatic/API stability, UI responsiveness, cross-platform compatibility,
maintainability, memory efficiency, visual polish.

## Invariants

- Business logic belongs in application/core code, never a frontend. CLI, TUI, MCP,
  and future interfaces must share OpenCollection parsing, environment resolution,
  request construction and execution, authentication, and persistence operations.
- The domain must not depend on frontend frameworks, CLI/MCP adapter types, YAML,
  HTTP-client types, or filesystem APIs. Keep crate dependencies directed inward.
- OpenCollection YAML is canonical. Do not duplicate collection structure in a
  proprietary database or invent format extensions without approval.
- Preserve unknown YAML where practical. Writes must be atomic, report failures,
  and refuse to overwrite externally modified sources.
- Runtime RequestKey and FolderKey values are session-only. Persistent interface references use repository locators.
- Request selection in the TUI is an O(1) in-memory operation with no filesystem,
  parsing, database, or network work.
- Filesystem, network, and expensive parsing or highlighting work must not block
  the TUI event/render loop.
- The CLI is non-interactive by default. Structured stdout is deterministic,
  versioned JSON; diagnostics go to stderr. Keep stable error categories and exit
  codes and bound large output.
- Facet TUI theme and interaction contracts are documented in `docs/FACET.md`; keep
  visual constants out of automated numeric gates.

## Data Safety and Tests

Every newly supported OpenCollection feature needs fixture-based tests, including
load → modify → save → reload where relevant. Core behavior must be testable without
launching a UI or invoking the CLI binary. Preserve unrelated worktree changes and
never use unsafe merely to bypass ownership problems.

Before completing a code change, run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features
cargo test
```

Do not report completion while a required check fails.

## Scope

Implement only the requested feature. Do not add cloud sync, accounts, telemetry,
analytics, plugins, GraphQL, streaming protocols, Git provider APIs, or MCP without
explicit scope. Avoid unrelated refactors, inspect source instead of guessing, and
make the smallest coherent change.
