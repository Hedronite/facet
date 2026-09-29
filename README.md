<p align="center">
  <a href="https://github.com/VirtualMachinist/facet">
    <img src="assets/facet-logo.jpeg" alt="Facet" width="220">
  </a>
</p>

<h1 align="center">Facet</h1>

<p align="center">
  <strong>Native, local-first API client.</strong><br>
  Terminal CLI and TUI on Probe, with persistent run history.
</p>

<p align="center">
  <a href="https://github.com/VirtualMachinist/facet/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/VirtualMachinist/facet/ci.yml?branch=main&style=flat&colorA=1A1A1A&colorB=C9A227&label=ci" alt="CI"></a>
  <a href="https://github.com/VirtualMachinist/facet/releases/tag/v0.6.0"><img src="https://img.shields.io/badge/Facet-v0.6.0-C9A227?style=flat&colorA=1A1A1A" alt="Facet v0.6.0"></a>
  <a href="https://crates.io/crates/facet-lattice"><img src="https://img.shields.io/crates/v/facet-lattice?style=flat&colorA=1A1A1A&colorB=C9A227" alt="facet-lattice on crates.io"></a>
  <a href="https://rustup.rs"><img src="https://img.shields.io/badge/Rust-1.95-F46623?style=flat&colorA=1A1A1A&logo=rust&logoColor=white" alt="Rust 1.95"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT%20%2F%20Apache--2.0-C9A227?style=flat&colorA=1A1A1A" alt="License"></a>
</p>

<p align="center">
  <a href="#quick-start">Quick start</a> ·
  <a href="docs/install.md">Install</a> ·
  <a href="#what-you-get">What you get</a> ·
  <a href="#how-it-works">How it works</a> ·
  <a href="#status">Status</a> ·
  <a href="IMPLEMENTATION_PLAN.md">Roadmap</a> ·
  <a href="#contributing">Contributing</a>
</p>

<p align="center">
  Fork of <a href="https://github.com/crizant/probe">Probe</a>
</p>

---

> **Status:** In production use as daily-driver / dogfood local-first API client (CLI/TUI + facet-lattice). Hardening: 0.6.0 on main; WebSocket/GraphQL/gRPC on the roadmap. Not a toy reference.


**Facet** is the terminal product: a `facet` CLI and Ratatui TUI on the same OpenCollection YAML as [Probe](https://github.com/crizant/probe). **[facet-lattice](https://crates.io/crates/facet-lattice)** is the run-history library underneath — SQLite for runs, bodies, timings, and sessions beside your YAML on disk.

Collections stay YAML; Git stays the sync layer. No account or hosted control plane. The `facet` binary coexists with `probe` on `PATH`.

**0.6.0** · OpenCollection YAML · Ratatui TUI · SQLite run history

## Quick start

### Library — `facet-lattice` on crates.io

```bash
cargo add facet-lattice
```

```toml
[dependencies]
facet-lattice = "0.5.9"
```

```rust
use lattice::WorkspaceStore;
```

The Cargo package is **`facet-lattice`** (not `lattice` — that name on crates.io is unrelated). The Rust import name is `lattice`.

### CLI — the `facet` binary

The CLI is **not** `cargo install facet`. Pick one:

**Release archive** (fastest): download from [GitHub Releases](https://github.com/VirtualMachinist/facet/releases), extract `facet`, add to `PATH`.

**From git** (Rust required):

```bash
cargo install --git https://github.com/VirtualMachinist/facet --package facet-cli --bin facet
```

**From a clone** (contributors): `cargo build --release -p probe-cli -p facet-cli` — see [docs/install.md](docs/install.md).

Verify: `facet --version` and `facet doctor --json`.

### Run a request and query history

```bash
facet request run ./api/users/list-users.yml --environment development --json
facet history --json
facet history --sql "SELECT status, count(*) FROM runs GROUP BY status"
facet blob <sha256>
facet tui
```

Full install guide (wrong-name traps, PATH, versions): **[docs/install.md](docs/install.md)**.

TypeSafe / Jev System One recipes (Choice + Noul, shadow, `$TYPESAFE_API_KEY` via `facet env set`): **[docs/jev-native.md](docs/jev-native.md)** and [`docs/examples/typesafe/`](docs/examples/typesafe/).


### TUI quick reference

| Key or command | Action |
|---|---|
| `:open <path>` | Mount a collection without quitting |
| `:recent` | Open recently recorded collections |
| `?` | Show the complete keymap and command hints |

## What you get

| | Upstream Probe | Facet (this repository) |
|---|---|---|
| Core Engine | Rust, filesystem-first | Same core, cherry-pickable |
| Collections | OpenCollection YAML | Same YAML, 100% compatible |
| Interface | GPUI Desktop + CLI (upstream) | Ratatui TUI (`facet tui`) + agent CLI; the in-repo `probe` binary is CLI-only |
| History | In-memory session | **facet-lattice** (SQLite workspace & machine store) |
| Binaries | `probe` | `facet` (coexists with `probe`) |
| Output | Human-readable & JSON | Deterministic, versioned JSON for automation |

Facet rebuilds the terminal and history layers; Probe owns the core domain and HTTP execution. Boundary: [docs/FACET.md](docs/FACET.md).

## How it works

**YAML holds the collection; facet-lattice holds the history.**

- `.facet/lattice.db` beside your collection stores run history; a machine store holds secrets, sessions, and a cross-workspace index.
- Agent commands (`history`, `blob`, `gc`) emit deterministic JSON. Large bodies are content-addressed.
- Secrets use the OS keyring or XChaCha20-Poly1305 via `FACET_SECRET_KEY`.

<details>
<summary><strong>Repository map</strong></summary>

```
crates/facet/             # facet CLI binary (facet-cli)
crates/lattice/           # facet-lattice library (crates.io)
crates/facet-tui/         # Ratatui interface
crates/cli/               # upstream probe-cli
crates/core/              # upstream probe-core
docs/install.md           # install guide
docs/FACET.md             # Facet contracts
```

</details>

## Status

**0.6.0** on `main` (next release). The `facet-lattice` crate is a separate version axis and does not follow the CLI: [`facet-lattice` 0.5.9](https://crates.io/crates/facet-lattice) is live on crates.io, with 0.5.8 still up (no yank).

- **facet-lattice**: SQLite default. Scrubbed crate package published as **0.5.9**.
- **CLI + TUI**: shipped. Deterministic JSON, SQL over history, Ratatui interface.
- **Roadmap**: WebSocket, GraphQL, gRPC, theme files — [IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md).

## Contributing

Issues and pull requests welcome. Start with [AGENTS.md](AGENTS.md) and [docs/FACET.md](docs/FACET.md).

## License

Upstream lineage: [Probe](https://github.com/crizant/probe) (Apache-2.0). Facet-original crates (`facet-cli`, `facet-lattice`, `facet-tui`): MIT. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
