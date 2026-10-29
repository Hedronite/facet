# Changelog

Notable changes to the Facet CLI. The version is `[workspace.package].version`
and the git tag `vX.Y.Z`. `facet-lattice` and the vendored `probe-*` crates are
separate version axes and do not move with this release.

## [Unreleased]

### Added

- A Facet-owned `facet:` key in the collection YAML turns recording off.
  `facet: { record: never }` on the collection, or `facet: { record: false }`
  on a request, writes no run row and no blob for `request run`, `replay`,
  the TUI and MCP, whatever `--no-record`, `FACET_NO_RECORD` or MCP
  `noRecord` say; the request is still sent. Flags can turn recording off,
  never on. Inside `facet:`, an unknown key, a wrong type or an unknown
  value fails with `facet_key_invalid` (exit 3) before anything is sent;
  the error names the key path and never the value. Unknown YAML outside
  `facet:` is still accepted and kept, and so is `facet:` when a command
  rewrites the file.

### Security

- `request run` and `replay` output, their error envelopes and `error[...]`
  lines, and MCP `request_run` and `run_replay` now redact what the run
  knows is secret. Known secret values of 4 bytes or more (hydrated, passed
  with `--var`, or a token the run sends) become `<redacted>`, exactly and
  in their URL-encoded forms, in the request and final URL, header values,
  bodies and error text. The values of sensitive headers such as
  `Authorization` and `Set-Cookie`, URL userinfo, and query values that
  match a known value are `<redacted>` too. `--dry-run` uses the same
  redaction. There is no flag to turn it off. MCP `history_get`, `run_diff`
  and `blob_get` return store data as stored.
- `replay` now sends the `githubToken` it resolves from `gh`, as `request
  run` does, and redacts it.
- Request bodies are scrubbed of known secret values at write, inline or
  as a blob, exactly and in their URL-encoded forms. The stored request
  body hash is the hash of the scrubbed bytes; `requestHash` still hashes
  the raw request, so `replay --frozen` matches as before. Response bodies
  are stored as received. Bodies recorded before this change keep what
  they were stored with.
- A request that references a variable hydrated from a machine-store row
  stored with `--secret` is now treated as secret even when the collection
  YAML does not declare that variable secret. With an environment
  selected, it is sent with the locked transport (no redirects, no
  `HTTP_PROXY` / `HTTPS_PROXY`), and a store row that would move its host
  fails with `host_override_refused` before anything is sent. Earlier, such
  a request was sent unlocked and the host-override check skipped it.
- The `facet:` recording rule fails closed. In a bundled collection, an
  empty selector, one that does not resolve, or one that resolves to
  something other than a request records nothing. In an unbundled
  collection, so does a selector that leaves the collection (`..`, an
  absolute path, a symlink escape). The request is still sent; no run row
  or blob is written.
- The `githubToken` that `request run` and `replay` take from `gh` is sent
  only to `https://api.github.com`, port 443 or none, with no userinfo. Any
  other URL fails with `github_token_host_refused` (exit 5) before `gh` runs
  and before anything is sent, in `--dry-run` too; the error names the host,
  never the token. A request carrying that token is now sent with the locked
  transport (no redirects, no `HTTP_PROXY` / `HTTPS_PROXY`). Earlier, the
  token went to whatever host the collection named, with redirects and the
  env proxy allowed. A `--var githubToken=…` is sent as before.
- Facet starts `gh` and `git` only from a fixed list of absolute paths, or
  from `FACET_GH` / `FACET_GIT` set to an absolute path to an existing
  regular file, and never searches `PATH`. A relative or missing override
  fails `subprocess_path_refused` (exit 5) before spawn, without naming the
  path. With no override and no program at a listed location, the run
  gets no token or no `git:` tag, as before. Each child gets a cleared
  environment with only `HOME`, `GH_CONFIG_DIR` (when set) and
  `PATH=/usr/bin:/bin`. Earlier, both came from `PATH` with the whole
  environment.
- `--no-subprocess` on `request run` and `replay`, and `FACET_NO_SUBPROCESS`
  (any non-empty value, honoured by the TUI and MCP too), start no child:
  no `gh auth token`, no `gh api` fallback, and no `git:` tag on a recorded
  run. `{{githubToken}}` with no `--var githubToken=…` then fails
  `subprocess_disabled` (exit 5) before anything is sent; the host check
  still runs first. `facet doctor` shows the switch.
- The `gh api` fallback runs only for an open send to exactly
  `https://api.github.com`, never for a locked send, and puts no sensitive
  header and no known secret value on the argv of `gh`. Earlier, a URL that
  only began with `https://api.github.com` (such as `api.github.com.invalid`)
  could fall back too. A `{{githubToken}}` request that passes the host
  check is locked before `gh` is asked.
- Security: every GitHub Action in CI and release is pinned to a full commit SHA, and scripts/check-action-pins.sh enforces this in CI. Release builds and tests run with --locked. Each release asset and SHA256SUMS carries a keyless build provenance attestation; verify a download with gh attestation verify <asset> --repo Hedronite/facet.
- A file body or multipart file part is sent only from inside the
  collection directory. Before anything is sent, on `request run`,
  `--dry-run`, `replay`, the TUI and MCP, its path is canonicalized and
  compared by component against the canonical collection directory, and
  the request is sent with the canonical path. A path outside it (`..`, an
  absolute path or a symlink), a missing file, or a collection read from
  stdin fails `file_body_outside_root` (exit 5) without naming the path.
  `--allow-file-outside-root` on `request run` and `replay` lets an outside
  file through; the TUI and MCP always refuse. Earlier, any path the
  collection named was read and sent. A file swapped inside the collection
  directory between the check and the open is still sent; see
  `docs/FACET.md`.
- `facet env set --value-stdin` reads the value from a pipe instead of
  argv, where other processes on the machine can see it, and strips one
  trailing newline. It fails `invalid_arguments` (exit 2), echoing nothing
  it read, when stdin is a terminal, when the value is empty or not UTF-8,
  and when `--value` is given too. `--value` with `--secret` still works
  and now prints one stderr warning that points at `--value-stdin`; the
  warning never holds the value.
- Hydrated values, `--var` values of declared secrets, the values a run
  redacts, the `githubToken` from `gh`, the values the machine store
  decrypts, and the `--value-stdin` buffer are held in memory that is
  zeroed on drop, and the debug form of a hydration shows names and counts
  only. Plain copies remain where `probe-core` renders the request, in the
  output redactor, and in the HTTP stack; see `docs/FACET.md`.
- `facet doctor --probe` no longer prints the value it read back when the
  secret backend round-trip mismatches.

### Fixed

- A request that carries no secret uses the normal transport again. A CLI
  request run without `--environment` follows redirects and connects through
  `HTTP_PROXY` / `HTTPS_PROXY` when it references no variable supplied by
  `--var`. A TUI send with no active environment does the same, and TUI
  sends that reference a secret stay locked. This removes the 0.7.1 known
  issue. The locked transport (no redirects, no env proxy) still applies
  when the selected environment declares a referenced variable secret, from
  any source; when no environment is selected and the request references a
  `--var`; and when secret discovery fails. `facet`, `probe` and the TUI
  decide this with one shared rule.
- Fixed: the TUI no longer drops file, form and multipart bodies on send and on save. It used to send them empty and delete them from the collection YAML. TUI sends now confine file bodies to the collection folder like the CLI (`file_body_outside_root`).

## [0.7.0] - 2026-10-25

Changes since v0.6.0 (`7547ebe`, 2026-09-29).

### Breaking

- `facet history --sql` returns NULL for secret columns. The read-only SQL
  authorizer substitutes NULL for `runs.req_body`, `runs.res_body`,
  `runs.req_headers`, `runs.res_headers`, `environments.value`, and
  `environments.secret_ref`, including through aliases, expressions, and
  `length()` of those columns. In 0.6.0 those `runs` columns came back as
  stored text (`SELECT *` included inline response bodies and header JSON).
  The JSON document adds `redactedColumns`, a list of `table.column` for each
  secret column the statement read. Schema version stays 1. Other blob values
  still render as a size. The authorizer also denies pragma writes,
  `load_extension`, and the raw page virtual tables `sqlite_dbpage` and
  `sqlite_dbdata`. `ATTACH` and `DETACH` were already denied.
  0.6.0 returned secret column text through `history --sql`, so users who
  exported or shared `history --sql` output from 0.6.0 should treat it as
  potentially containing secrets and rotate as needed. Upgrade to 0.7.0.

### Added

- Agent skill published to the harness paths this repository ships:
  `.agents/skills/facet/`, `.claude/skills/facet/`, `.grok/skills/facet/`, and
  `.cursor/skills/facet/`. The Nushell body wraps the read-only CLI commands:
  `request list`, `request get`, `history` (no bodies), `history --id`,
  `history --sql`, `last`, `session list`, `session show`, `pin list`,
  `env list` (names only), `diff` (no bodies), `ncl check`, `ncl export`, and
  `doctor`. `request run` (including `--dry-run`), `replay`, `ncl apply`,
  `session start`, `session end`, `blob`, `env set`, `env delete`, and pin
  create and delete stay unwrapped. `facet mcp` is unchanged.

### Security

- Advisory lockfile bumps: `ctutils` 0.4.4, `rustls` 0.23.46,
  `rustls-webpki` 0.103.16, `webpki-root-certs` 1.0.10, and `tokio` 1.53.3.
- `cargo-deny` ignores, dated in `deny.toml`, for unmaintained `unsafe-libyaml`
  (RUSTSEC-2024-0449) and the zbus 5 crates `zvariant`, `zbus_names`, and
  `zvariant_derive` (RUSTSEC-2026-0370, RUSTSEC-2026-0371, RUSTSEC-2026-0372).
  No small upgrade drops those crates.

### Documentation

- README refreshed for the already released 0.6.0 CLI (repository URL and
  command index).
- Roadmap text now plans WebSocket, GraphQL, and gRPC in Facet first, then
  upstream. Those protocols do not ship in 0.7.0.
