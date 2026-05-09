# Benchmarks

The benchmark harness is intentionally small and uses focused CLI/progress
dependencies for argument parsing and multi-progress rendering. Large corpora
are downloaded into `bench/cache` and results are written to `bench/results`;
both paths are ignored by git.

Day-one corpus tiers:

- Smoke: generated empty, small text, repeated bytes, random bytes, and
  boundary-sized fixtures.
- Standard: Canterbury/Calgary, Large Canterbury, Silesia, `enwik8`,
  Pizza&Chili 50 MiB text-family prefixes, and a couple of Pizza&Chili
  repetitive/log samples.
- Release: `enwik9`, larger Pizza&Chili prefixes, Govdocs subsets, fixed GH
  Archive or Common Crawl slices, logs, JSON/NDJSON, CSV, source trees,
  database dumps, and already-compressed files.

Fetch practical benchmark corpora into the ignored cache with:

```sh
bench/fetch-corpora.sh standard
```

The fetcher downloads archives into `bench/cache/downloads`, extracts corpora
under `bench/cache`, writes prepared single-file inputs to `bench/cache/inputs`,
and records byte counts plus SHA-256 values in `bench/cache/manifest.tsv`.
Multi-file corpora are packed into deterministic `.tar` files because
`bench/run` benchmarks one input file at a time. For strict corpus reporting,
run the extracted files individually and aggregate the JSONL rows externally.

Useful fetch groups:

```sh
bench/fetch-corpora.sh canterbury-suite
bench/fetch-corpora.sh pizzachili-50mb
bench/fetch-corpora.sh pizzachili-repetitive
bench/fetch-corpora.sh release
```

Metrics to record per run:

- command, tool version, git revision, corpus, input bytes, compressed bytes.
- wall time, CPU time, peak RSS, thread count, block size, integrity check.
- decompressed SHA-256 to prove round-trip correctness.

`bench/run.rs` is built as the Cargo binary `bench-run`. Rebuild the local
`bench/run` executable with:

```sh
cargo build --release --bin bench-run
cp target/release/bench-run bench/run
```

JSONL is the default mode for agents, scripts, dashboards, and repeatable result
capture:

```sh
bench/run target/release/compress bench/cache/enwik8 > bench/results/enwik8.jsonl
bench/run target/release/compress bench/cache/inputs/silesia.tar > bench/results/silesia.jsonl
bench/run --jsonl target/release/compress bench/cache/enwik8
bench/run --mode jsonl target/release/compress bench/cache/enwik8
```

It also has an `indicatif` multi-progress TUI mode for local inspection:

```sh
bench/run --tui target/release/compress bench/cache/enwik8
bench/run --mode tui target/release/compress bench/cache/enwik8
```

JSONL rows contain:

- tool name, command line, detected tool version, git revision, corpus path,
  and corpus name.
- compression level and thread label (`t1`, `t2`, `t4`, `t8`, `t16`, `t0`, or
  `auto`) for matrix cases. Fixed thread labels are included only when the
  detected physical-core count can run them; for example `t16` is omitted on
  machines with fewer than 16 physical cores.
- input bytes, compressed output bytes, compression wall time, and decompression
  wall time.
- decompressed SHA-256 and `roundtrip_ok` when `sha256sum`, `shasum`, or
  `openssl` is installed.

Competitor commands should be version-pinned in result rows and optional at
runtime. The runner detects physical cores and schedules cases by core budget,
so independent cases run concurrently when enough physical cores are free. The
`t0` label uses the physical-core count instead of each tool's logical-core
auto mode; auto competitors such as `pigz`, `pbzip2`, and `7z` are also pinned
to physical-core counts where their CLIs support it.

The default matrix includes `compress-xz` and `xz` at levels `1, 3, 6, 9`,
`compress-bzip2` at levels `1, 6, 9`, and `zstd` at levels `1, 3, 10, 19`
across the detected thread labels, plus `gzip`/`pigz`, `bzip2`/`pbzip2`, `lz4`,
`brotli`, and `7z` level sweeps when installed. Compression commands are fed
through stdin in TUI mode so active parallel cases can each show feed progress,
core allocation, throughput, and wall time.
