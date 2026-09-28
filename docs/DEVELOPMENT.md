# Development Practices

Read this document for implementation workflow, dependency changes, and test
design. Project-wide architectural invariants remain in `AGENTS.md`.

## Rust and Async Work

- Prefer normal ownership, message passing, task results, and immutable shared
  data before `Arc`, `Mutex`, `RwLock`, `RefCell`, or global mutable state.
- Keep shared networking and application APIs asynchronous where the TUI needs them.
  Do not create duplicate synchronous business logic for the CLI.
- Run filesystem I/O, HTTP, large YAML or JSON parsing, Git work, and expensive
  highlighting away from the TUI event/render loop.
- Never use `unsafe` solely to bypass ownership problems. Any unsafe code requires
  explicit justification.

## Dependencies

Before adding a crate, check the standard library and existing dependencies. Prefer
actively maintained, cross-platform crates and avoid large dependencies for trivial
work. Do not replace dependencies without a task-specific reason.

The legacy GPUI desktop has been retired from this Facet workspace. Do not
reintroduce a GUI framework or change the remaining product's dependency policy
without task-specific approval.

Do not introduce Electron, Tauri, WebView, React, Flutter, or another GUI framework
without explicit approval.

## Tests

Keep shared fixtures under `tests/fixtures/`. CLI integration tests cover command
behavior, JSON output, and exit codes.

Do not automate visual constants such as spacing, radii, typography sizes, palette
values, or contrast ratios. Review TUI theme behavior against [FACET.md](FACET.md). TUI tests may cover
behavior such as theme selection, navigation, focus, and response highlighting;
extend an existing TUI test when it already covers the same surface.

Before completing a code change, run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features
cargo test
```

Do not report completion while any required check fails.

## Working Style

- Inspect relevant architecture and pinned dependency source instead of guessing.
- Preserve unrelated user changes in a dirty worktree.
- Make the smallest coherent change and add or update tests.
- Summarize architectural decisions and remaining limitations.
