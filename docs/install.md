# Install

Facet is two things strangers encounter under different names:

| What | Cargo / binary name | Role |
| --- | --- | --- |
| **facet-lattice** | [crates.io package](https://crates.io/crates/facet-lattice) `facet-lattice` | Run-history library (SQLite beside OpenCollection YAML) |
| **Facet** | `facet` binary (`facet-cli` in this repo) | Terminal product: CLI + Ratatui TUI on top of Probe |

The Rust import for the library is `lattice` (`use lattice::…`). The Cargo package name is always **`facet-lattice`**.

## Wrong-name traps

| Do not use | Use instead | Why |
| --- | --- | --- |
| `cargo add lattice` | `cargo add facet-lattice` | [crates.io `lattice`](https://crates.io/crates/lattice) is an unrelated package |
| `cargo install facet` | See [facet binary](#facet-cli-binary) below | There is no `facet` crate on crates.io for the CLI |
| Package name `lattice` in `Cargo.toml` | `facet-lattice = "…"` | Published name on crates.io |
| `cargo install facet-lattice` expecting the CLI | Install `facet` binary separately | The library crate does not ship the `facet` executable |

## facet-lattice (library)

Add the run-history store to a Rust project:

```bash
cargo add facet-lattice
# or pin: cargo add facet-lattice@0.5.9
```

SQLite-only (default):

```toml
[dependencies]
facet-lattice = "0.5.9"
```

```rust
use lattice::WorkspaceStore;
```

Optional in-process analytics against SQLite files (heavy native build):

```toml
[dependencies]
facet-lattice = { version = "0.5.9", features = ["lattice-duckdb"] }
```

See also [crates/lattice/README.md](../crates/lattice/README.md) and [Lattice engines](LATTICE-ENGINES.md).

## facet (CLI binary)

The `facet` executable is **not** on crates.io as a one-line `cargo install`. Use a release archive, a git install, or a source build.

### From a GitHub release (recommended)

1. Open [Facet releases](https://github.com/VirtualMachinist/facet/releases).
2. Download the archive for your platform (`facet-cli-<version>-<platform>.tar.gz`).
3. Extract the `facet` binary.
4. Move it onto your `PATH` (e.g. `~/.local/bin` or `/usr/local/bin`).

```bash
export PATH="$HOME/.local/bin:$PATH"   # if needed
facet --version
# facet 0.7.0 (probe 0.5.9)
```

### From git (Rust toolchain required)

Builds the `facet` binary from this repository (package `facet-cli`):

```bash
cargo install --git https://github.com/VirtualMachinist/facet --package facet-cli --bin facet
facet --version
```

This compiles from `main` (or pass `--tag v0.7.0` to match a release). Expect a longer build than downloading a release archive.

### From a cloned repo (contributors)

Requires Rust **1.95** or newer:

```bash
git clone https://github.com/VirtualMachinist/facet.git
cd facet
cargo build --release -p probe-cli -p facet-cli
export PATH="$PWD/target/release:$PATH"
facet --version
```

`probe` builds alongside `facet` and can coexist on `PATH`.

## Verify install

```bash
facet --version          # human: facet 0.7.0 (probe 0.5.9)
facet --version --json   # structured version + probeVersion
facet doctor --json      # machine + workspace stores, secrets backend, env flags
```

`facet doctor` reports whether Lattice stores open, schema versions, and which `FACET_*` environment variables are set. Use it after install or when debugging a new machine.

## Versions

Facet carries three version axes. They are allowed to differ; each simply has
to be true.

| Artifact | Source | Version |
| --- | --- | --- |
| `facet` CLI / `facet-tui` / `facet-record` | `[workspace.package].version`, GitHub release tag | **0.7.0** (when tagged) |
| `facet-lattice` crate | [crates.io](https://crates.io/crates/facet-lattice) | **0.5.9** live (0.5.8 also up) |
| `probe-*` crates | vendored Probe cut, pinned per crate | **0.5.9** |

`facet --version` prints the first and third together: `facet 0.7.0 (probe 0.5.9)`.
The `facet-lattice` crate is never bumped to match the CLI, and the probe crates
are never bumped to make that parenthetical look symmetrical. **0.5.8** stays on
crates.io (no yank). `scripts/version-check.sh` is the gate that keeps the git
tag, the Cargo version, and the built binary in agreement.

## Next steps

- Product overview: [README](../README.md)
- Facet commands and JSON contracts: [FACET.md](FACET.md)
- Engine configuration: [LATTICE-ENGINES.md](LATTICE-ENGINES.md)
