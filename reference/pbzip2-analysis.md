# pbzip2 reference notes

Source cloned from the Launchpad Ubuntu source-package git mirror into
`reference/pbzip2` at commit `b7fa130` (`1.1.13-1build2`, patches unapplied).

## What pbzip2 does for speed

- It does not implement a faster block-sorting codec in `pbzip2.cpp`.
  Compression workers call libbz2's `BZ2_bzBuffToBuffCompress` for each input
  chunk with `workFactor=30`.
- It uses a producer/consumer/file-writer pipeline:
  - the producer reads fixed-size file chunks into heap buffers and pushes them
    into a bounded queue;
  - consumer threads compress chunks independently;
  - a writer thread drains completed chunks in original block order.
- Its output buffer is a fixed-size circular buffer. Workers can finish out of
  order, but the writer only writes `NextBlockToWrite`, so output stays ordered.
- Memory pressure is bounded by `NumBufferedBlocksMax`, derived from max memory,
  block size, and CPU count. If the buffer is too small for the requested CPU
  count, pbzip2 raises it unless the user set `-m`.
- It has separate concepts for file chunk size and bzip2 BWT block size:
  - `-b#` controls file chunk size in 100k steps;
  - `-1..-9` controls the BWT block size given to libbz2.
- `--read` can read a whole input file and set chunk size to `file_size / cpus`
  so large files are split evenly across workers.
- It skips the threaded path for one-block/small inputs to avoid thread overhead.
- It gains parallel decompression mostly from pbzip2-style files, where output is
  concatenated independent bzip2 streams. Single-stream bzip2 files have much
  less parallelism.

## What maps to this project

- Do not copy or link pbzip2/libbz2 code. The useful transferable ideas are
  scheduling and buffering, not the codec internals.
- Add a streaming encoder pipeline:
  - one producer reads chunks;
  - worker threads encode blocks;
  - one ordered writer emits blocks as soon as the next block is available.
  This should reduce peak memory, latency, and serial tail work versus collecting
  all encoded blocks before writing.
- Keep the current one-stream default for compression ratio. Switching the
  default to pbzip2-style concatenated streams would reduce final bit-splice and
  combined-CRC work, but it adds per-stream headers/trailers and loses ratio on
  small/repetitive inputs.
- Consider an explicit `--set stream_mode=pbzip2` or benchmark-only mode later
  if we want maximum wall-time at the cost of slightly larger output.
- Add a bounded reorder buffer for compression and decompression instead of
  collecting all blocks in a `Vec`.
- Add adaptive job splitting for multicore:
  - keep 900k BWT blocks for ratio;
  - split scheduling into enough independent block tasks to cover all workers;
  - avoid smaller BWT blocks by default because the local benchmark showed that
    `-3`/smaller blocks are faster but hurt compression ratio.
- Add a `--read`-style file path for seekable files only if it improves balance;
  it is not useful for stdin and may increase latency before first output.

## Current state after local optimization

The in-tree encoder now beats stock `bzip2` in focused single-core checks and
keeps smaller output. On enwik8 with 16 workers it is now in the same timing
band as `pbzip2` and ahead by mean in the latest local 30-run audit, while
still producing smaller output than both stock `bzip2` and `pbzip2`.
The largest tradeoff is memory: the retained `mimalloc` allocator improves wall
time but raises peak RSS materially.

The remaining likely wins are reducing BWT allocation pressure without losing
the allocator speedup, and replacing more of the comparison-heavy BWT exact sort
with a lower-overhead in-tree strategy.
