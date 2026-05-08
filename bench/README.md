# Benchmarks

The benchmark harness is intentionally dependency-light. Large corpora are
downloaded into `bench/cache` and results are written to `bench/results`; both
paths are ignored by git.

Day-one corpus tiers:

- Smoke: generated empty, small text, repeated bytes, random bytes, and
  boundary-sized fixtures.
- Standard: Canterbury Corpus, Silesia Corpus, and `enwik8`.
- Release: `enwik9`, Pizza&Chili repetitive corpus, Govdocs subsets, fixed
  GH Archive or Common Crawl slices, logs, JSON/NDJSON, CSV, source trees,
  database dumps, and already-compressed files.

Metrics to record per run:

- command, tool version, git revision, corpus, input bytes, compressed bytes.
- wall time, CPU time, peak RSS, thread count, block size, integrity check.
- decompressed SHA-256 to prove round-trip correctness.

`bench/run.rs` is the current std-only runner. JSONL is the default mode for
agents, scripts, dashboards, and repeatable result capture:

```sh
bench/run target/release/compress bench/cache/enwik8 > bench/results/enwik8.jsonl
bench/run --jsonl target/release/compress bench/cache/enwik8
bench/run --mode jsonl target/release/compress bench/cache/enwik8
```

It also has an ANSI TUI mode for local inspection:

```sh
bench/run --tui target/release/compress bench/cache/enwik8
bench/run --mode tui target/release/compress bench/cache/enwik8
```

JSONL rows contain:

- tool name, command line, detected tool version, git revision, corpus path,
  and corpus name.
- compression level and thread label (`t1`, `t0`, or `auto`) for matrix cases.
- input bytes, compressed output bytes, compression wall time, and decompression
  wall time.
- decompressed SHA-256 and `roundtrip_ok` when `sha256sum`, `shasum`, or
  `openssl` is installed.

Competitor commands should be version-pinned in result rows and optional at
runtime. The default matrix includes `compress-xz` and `xz` at levels
`1, 3, 6, 9` in `t1` and `t0` modes, `zstd` at levels `1, 3, 10, 19` in `t1`
and `t0` modes, `gzip`/`pigz` and `bzip2`/`pbzip2` at levels `1, 6, 9`, plus
`lz4`, `brotli`, and `7z` level sweeps when installed. Compression commands are
fed through stdin in TUI mode so the progress line can show input-feed progress
and MiB/s while each compressor runs.
