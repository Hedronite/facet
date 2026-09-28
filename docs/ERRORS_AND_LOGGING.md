# Errors and Logging

Facet keeps errors structured until they reach an interface boundary.

- Library crates define typed errors for their own operations when those operations
  are introduced.
- The core/application layer coordinates errors without depending on CLI, TUI, or
  MCP presentation types.
- The CLI maps error categories to stable exit codes. Human diagnostics go to
  stderr; versioned JSON command output goes to stdout.
- The TUI and MCP adapters present the same structured errors without
  reimplementing their meaning.
- Libraries do not initialize global logging. Interfaces configure logging and
  send diagnostics to stderr or their interface-specific sink.

Repository loading and saving, environment resolution, and HTTP execution expose typed
library errors. Persistence distinguishes stale-source conflicts, read-only in-memory
sources, invalid retained documents, serialization failures, and filesystem failures.
Interfaces map these types without requiring callers to parse diagnostic messages.
