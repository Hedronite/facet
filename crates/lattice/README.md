# facet-lattice

**facet-lattice** is the run-history library for [Facet](https://github.com/Hedronite/facet): bundled SQLite beside OpenCollection YAML, with an optional DuckDB analytics feature. Facet (the CLI/TUI product) depends on this crate; strangers add it directly from crates.io.

The Rust library import is `lattice` (`use lattice::…`). The Cargo package name is **`facet-lattice`** — not `lattice` (that crates.io name is unrelated).

## Install

```bash
cargo add facet-lattice
# or: cargo add facet-lattice@0.5.9
```

SQLite-only (default):

```toml
[dependencies]
facet-lattice = "0.5.9"
```

Optional in-process analytics against SQLite files:

```toml
[dependencies]
facet-lattice = { version = "0.5.9", features = ["lattice-duckdb"] }
```

## Features

| Feature | Default | Description |
| --- | --- | --- |
| *(none)* | yes | Bundled SQLite workspace + machine stores |
| `lattice-duckdb` | no | Optional analytics smoke against SQLite files (heavy native build) |

## Docs

- [Install guide](https://github.com/Hedronite/facet/blob/main/docs/install.md)
- [Facet guide](https://github.com/Hedronite/facet/blob/main/docs/FACET.md)
- [facet-lattice engines](https://github.com/Hedronite/facet/blob/main/docs/LATTICE-ENGINES.md)

## License

MIT — see [LICENSE-MIT](LICENSE-MIT).
