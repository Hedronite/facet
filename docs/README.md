# Documentation

Read only the document relevant to the task. `AGENTS.md` contains the invariants that
apply to every code change and routes task-specific work here.

| Document | Canonical subject | Read when |
| --- | --- | --- |
| [Architecture](ARCHITECTURE.md) | Crate boundaries, data flow, workspace identity, persistence, synchronization, HTTP, imports, and current interface behavior | Changing shared behavior or boundaries |
| [CLI](CLI.md) | Commands, selectors, JSON schemas, error categories, and exit codes | Changing or consuming the CLI contract |
| [Historical desktop design](DESIGN.md) | Retired Probe GPUI presentation reference; not current Facet product guidance | Reading historical desktop context |
| [Development](DEVELOPMENT.md) | Rust ownership, async work, dependencies, tests, and completion checks | Implementing code or changing dependencies |
| [Errors and logging](ERRORS_AND_LOGGING.md) | Typed-error ownership and interface logging boundaries | Adding or mapping failures and diagnostics |
| [Performance](PERFORMANCE.md) | Benchmark commands, fixtures, measurement policy, and reference results | Measuring or optimizing performance |
| [Install](install.md) | `facet-lattice` on crates.io and the `facet` CLI from releases or source | Installing or distributing Facet |
| [Facet](FACET.md) | Facet CLI/TUI/MCP, `facet-lattice` run history, command contracts, upstream boundary, license, and the current TUI theme-file contract. Shipped 2026-09-07 under [Shipped](FACET.md#shipped-2026-09-07-and-more); remaining work under [Next slice](FACET.md#next-slice) | Working on any Facet-only command, crate, or theme |
| [Jev native transport](jev-native.md) | TypeSafe Jev System One collection: `facet request run` / MCP `request_run` 1:1, shadow semantics, `$TYPESAFE_API_KEY` via `facet env set` (never Lattice) | Adding or calling Jev Choice/Noul recipes |
| [Lattice engines](LATTICE-ENGINES.md) | `facet-lattice` storage engines, configuration, and verification | Changing or documenting run-history storage |
| [Roadmap](../IMPLEMENTATION_PLAN.md) | Implemented foundation and explicitly deferred product work | Planning scope or starting a deferred feature |

[README.md](../README.md) is the user-facing product, installation, and development
quick start. It should link to canonical documents rather than repeat their contracts.

## Documentation Rules

- State each durable rule in one canonical document and link to it elsewhere.
- Keep `AGENTS.md` limited to always-applicable constraints and task routing.
- Document current behavior in architecture, CLI, or design references; keep future
  scope in the roadmap.
- Prefer code and fixture names over copying large implementation inventories.
- Remove completed phase checklists after their behavior is captured in canonical
  documentation and tests.
