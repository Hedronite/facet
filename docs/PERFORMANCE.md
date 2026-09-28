# Performance Baseline

The repeatable performance suite covers bundled workspaces containing 100, 1,000,
and 10,000 requests. Each
deterministic fixture contains folders of 100 requests and representative headers,
query parameters, and JSON bodies.

## Time benchmarks

Run the release-mode Criterion suite:

```bash
cargo bench -p probe-cli --bench performance
```

The benchmark groups measure separate boundaries:

- `opencollection_parsing`: YAML decoding, validation, retained document creation,
  and projection into domain models.
- `workspace_construction`: construction of generational request and folder arenas
  from an already parsed domain collection. Fixture cloning is setup work and is not
  timed.
- `request_lookup`: in-memory lookup through a session-only `RequestKey`, cycling
  across the loaded workspace.
- `cli_startup/help`: operating-system process creation and Probe startup through
  rendering `probe --help`.

Criterion stores machine-local reports under `target/criterion`. Compare results on
the same machine and build profile; absolute timings from different machines are not
directly comparable.

Criterion 0.7 remains pinned with the original baseline. The workspace minimum is
Rust 1.95; dependency upgrades should be handled separately from performance
measurement.

## Representative fixture files

Benchmarks generate fixtures in memory to avoid maintaining megabytes of repetitive
YAML. Generate the same deterministic files when an external profiler needs paths:

```bash
cargo run -p probe-cli --example generate_performance_fixtures -- \
  target/performance-fixtures
```

Generated files are named `workspace-100.yml`, `workspace-1000.yml`, and
`workspace-10000.yml`.

## Peak memory

Build Probe and generate the fixtures first:

```bash
cargo build --release -p probe-cli
cargo run -p probe-cli --example generate_performance_fixtures -- \
  target/performance-fixtures
```

On macOS, record peak resident memory while loading the 10,000-request workspace:

```bash
/usr/bin/time -l target/release/probe collection validate \
  target/performance-fixtures/workspace-10000.yml --quiet
```

Use the `maximum resident set size` line. On Linux, run the equivalent command with
`/usr/bin/time -v` and read `Maximum resident set size`. These measurements include
the process, YAML source retention, parsed document, locator index, and domain
workspace, matching the memory paid by a real workspace load.

## Baseline policy

Record the date, commit, operating system, CPU, Rust version, and Criterion estimates
when evaluating a change. This phase intentionally adds measurements without
performance thresholds or optimizations; thresholds should only follow stable data
from representative machines.

## Initial reference run

The initial baseline was recorded on 2026-08-15 using an Apple M4 MacBook Pro
(10 cores, 16 GB), macOS 26.6.1, and rustc 1.97.1. Criterion point estimates from a
release build were:

| Measurement | 100 | 1,000 | 10,000 |
| --- | ---: | ---: | ---: |
| OpenCollection parsing | 1.97 ms | 22.90 ms | 219.30 ms |
| Workspace construction | 2.73 µs | 20.46 µs | 364.21 µs |
| Request lookup | 1.23 ns | 1.44 ns | 1.83 ns |

`probe --help` process startup measured 2.58 ms. Loading and validating the generated
10,000-request fixture peaked at 222,199,808 bytes resident memory (about 212 MiB).
These values are a local reference, not regression thresholds.
