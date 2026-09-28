# Architecture

Facet is a terminal-native API client. The CLI, Ratatui TUI, and Facet MCP
adapter are interfaces over shared application and domain operations. The legacy
Probe desktop is historical and is not a member of this workspace; see
[DESIGN.md](DESIGN.md) only for that historical context.

## Overview

```text
                 Interfaces
       ┌────────────┼────────────┐
       │            │            │
      CLI          TUI          MCP
       └────────────┼────────────┘
                    │
          ┌─────────▼─────────┐
          │    Application    │
          │ workspace         │
          │ request execution │
          │ environment       │
          │ persistence       │
          └─────────┬─────────┘
                    │
          ┌─────────▼─────────┐
          │      Domain       │
          │ workspace         │
          │ request           │
          │ environment       │
          │ response          │
          └──────┬───────┬────┘
                 │       │
       OpenCollection   HTTP
          repository    engine
                 │       │
                YAML   network
```

Portable import formats are inbound adapters. The Postman adapter reads official
Collection v2.0/v2.1 JSON, while the Yaak adapter reads official export JSON or
directory-sync models. Both produce the same domain `Collection` used by every
interface. OpenCollection remains the only canonical persistence representation:

```text
Postman JSON        Yaak export / sync directory
      ↓                         ↓
Postman adapter             Yaak adapter
      └────────────┬────────────┘
                   ↓
          Domain Collection
                   ↓
    OpenCollection repository
                   ↓
             Bundled YAML
```

Adapters do not own business logic or filesystem persistence. They invoke shared
application operations and pass domain values to the repository. Import
diagnostics live in the shared layers so strict and partial behavior remains
identical across providers and interfaces.

## Interface boundary

CLI, TUI, and MCP are interfaces. They do not own business logic. If behavior
differs between them, determine whether the difference belongs to presentation or
represents an architectural bug.

Typical paths are:

```text
CLI arguments ──┐
TUI action ──────┼→ Application → Domain / repository / HTTP → interface result
MCP tool call ──┘
```

The MCP surface is a typed adapter over the same functions used by the CLI. It
returns the existing `schemaVersion: 1` JSON documents as structured content; it
does not spawn `facet` or parse terminal output. The complete MCP contract is in
[Facet](FACET.md#mcp).

## Dependency rule

Dependencies point inward:

- Interfaces may depend on application and domain operations.
- Application code may depend on domain abstractions and infrastructure traits.
- Infrastructure implements capabilities required by the application.
- The domain must not depend on TUI/CLI/MCP adapter types, YAML, HTTP-client
  types, or filesystem implementations.

The CLI is a first-class automation and headless interface. Its responsibilities
are argument parsing, invoking application operations, human and structured
presentation, stdin/stdout integration, and exit-code mapping. It must not
implement domain behavior. JSON output is deterministic and versioned; automation
uses stable error categories and exit codes rather than human diagnostics.

The TUI owns terminal rendering, navigation, focus, appearance, and response
presentation. Request selection is an O(1) in-memory operation. Filesystem,
network, parsing, and highlighting work must stay outside the event/render loop.
The current theme-file and interaction contract is in
[Facet](FACET.md#theme-files).

## Application and workspace

The application layer coordinates use cases such as:

- load a workspace;
- list and select requests;
- execute a request;
- resolve an environment;
- save a request;
- validate a collection; and
- inspect or convert an imported collection.

These operations are usable from the CLI, TUI, and MCP without knowledge of an
interface. Opening a workspace follows this path:

```text
OpenCollection files
        ↓
OpenCollectionRepository
        ↓
Domain Workspace
        ↓
Application layer
        ↓
CLI, TUI, or MCP
```

OpenCollection does not define durable request or folder IDs. Each loaded
workspace assigns generational `RequestKey` and `FolderKey` values for fast,
stale-safe in-memory lookup. These keys are never serialized and are rebuilt on
reload. Repository adapters own persistence locators: workspace-relative paths
for unbundled collections and structural item paths for bundled collections.
Selectors and session references use those locators; names are presentation data,
not identity.

## CLI and request execution

The normal execution path is:

```text
CLI arguments
      ↓
Application operation
      ↓
WorkspaceRepository
      ↓
Request + Environment
      ↓
EnvironmentResolver
      ↓
HTTP engine
      ↓
Response
      ↓
CLI formatter / Lattice recorder
```

Environment selection and interpolation live in `probe-core`, so CLI, TUI, and
future interfaces share the same behavior. Parent environments are applied before
children, child variables override by name, and values may reference other
variables. Cyclic inheritance, cyclic interpolation, missing variables, and
invalid variant selection produce typed errors. Resolution returns a cloned
request and leaves the canonical parsed model unchanged.

The resolver handles method, URL, headers, query and path parameters, supported
body fields, file references, and authentication string/number values. A secret
declaration has no value in OpenCollection, so references fail until a secure
runtime value provider supplies one. The resolver does not load `dotEnvFilePath`;
the domain remains independent of filesystem APIs.

## HTTP execution and response retention

`probe-http` owns the single asynchronous HTTP implementation. It converts
resolved domain requests into network requests, substitutes enabled path
parameters, applies headers and query parameters, selects body/file variants,
implements Basic and Bearer authentication, and enforces OpenCollection timeout
and redirect settings. Interfaces do not construct HTTP requests independently.

The engine accepts a caller-provided cancellation future. Completion of that
future—or dropping the execution future—cancels the request without coupling the
engine to a terminal or interface framework. The CLI adapts Ctrl-C to this
boundary; the TUI and MCP use the same execution API.

Completed responses contain status, reason, final URL, duration, size,
deterministically sorted headers, and at most 16 MiB of in-memory body data. When
the bound is crossed, the engine keeps the leading 16 MiB as a preview and may
stream the complete body to a managed spool file. The final owner removes that
file. Callers that need the complete body provide a cache directory or drain the
remainder without retaining it. File-backed responses are read and searched in
bounded pages; formatting a single page as a complete document is not supported.

`--output` remains distinct: it streams chunks to a temporary file and replaces
the requested destination only after the complete response is written and synced.
Response retention and history policies remain outside the interface layer.
Facet's run-history behavior is documented in [Facet](FACET.md).

## Persistence and synchronization

Interfaces submit domain or repository operations. Only the OpenCollection
repository serializes YAML or mutates collection files. Writes retain unknown YAML
where practical, compare the current source with the loaded bytes under a writer
lock, write and sync a temporary file, and atomically replace the destination.
Symlinked workspaces update their canonical target without replacing the symlink.

Structural mutations are repository-owned `StructureOperation` values. Bundled
operations retain unknown YAML and replace one document atomically. Unbundled
multi-document moves and ordering changes retain rollback data and a recovery
manifest; incomplete rollback is reported as requiring recovery rather than
hidden. Every affected retained source is checked before mutation.

Filesystem notifications are invalidation hints. The application debounces them,
reloads through the repository, and reconciles the loaded baseline with local and
disk state. Non-overlapping field changes merge automatically; overlapping edits,
dirty deletion, and ambiguous rename require an explicit choice. Invalid or
partially written files never replace the last valid workspace. Successful writes
refresh the baseline, so their watcher events reconcile as no-ops.

## OpenCollection validation

Workspace loading requires the OpenCollection `1.0.0` format marker, collection
metadata, and an explicit `bundled` mode matching the source kind. It validates
environment names and the complete inheritance graph, including duplicate names,
missing parents, and cycles. This validation is shared by `collection validate`
and every operation that loads a workspace.

## Concurrency

Application-facing state has clear ownership. Slow operations execute outside the
TUI event/render path:

- HTTP and streaming network work;
- filesystem I/O and Git;
- YAML, JSON, and large-response processing; and
- expensive syntax highlighting.

Completed operations return structured results or events. Avoid shared mutable
global state.
