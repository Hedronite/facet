# Changelog

Notable changes to the Facet CLI. The version is `[workspace.package].version`
and the git tag `vX.Y.Z`. `facet-lattice` and the vendored `probe-*` crates are
separate version axes and do not move with this release.

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
