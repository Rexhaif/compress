use crate::algorithms::bzip2::bitstream::{BitReader, BitWriter};
use crate::algorithms::bzip2::{block, crc};
use crate::error::{Error, Result};

use rayon::prelude::*;

use std::io::{Read, Write};

#[derive(Clone, Debug)]
pub struct Bzip2Options {
    pub block_size_100k: u8,
    pub threads: u32,
}

#[derive(Debug)]
pub struct StreamInfo {
    pub blocks: u64,
    pub compressed_size: u64,
    pub streams: u64,
    pub uncompressed_size: u64,
}

struct RawBlock {
    start: usize,
    end: usize,
}

pub fn encode_reader_to_writer<R: Read, W: Write>(
    reader: R,
    writer: W,
    options: &Bzip2Options,
) -> Result<()> {
    encode_reader_to_writer_with_capacity(reader, writer, options, 0)
}

pub fn encode_reader_to_writer_with_capacity<R: Read, W: Write>(
    mut reader: R,
    mut writer: W,
    options: &Bzip2Options,
    capacity: usize,
) -> Result<()> {
    let mut input = Vec::with_capacity(capacity);
    reader.read_to_end(&mut input)?;

    validate_options(options)?;
    let threads = options.threads;
    if let Some(streams) = try_encode_fixed_chunks_as_streams_parallel(&input, options, threads)? {
        for stream in streams {
            writer.write_all(&stream)?;
        }
        return Ok(());
    }

    let output = encode_stream_without_fixed_chunks(&input, options, threads)?;
    writer.write_all(&output)?;

    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn encode_stream(input: &[u8], options: &Bzip2Options) -> Result<Vec<u8>> {
    validate_options(options)?;

    let threads = options.threads;
    if let Some(streams) = try_encode_fixed_chunks_as_streams_parallel(input, options, threads)? {
        return Ok(assemble_streams(streams));
    }

    encode_stream_without_fixed_chunks(input, options, threads)
}

fn try_encode_fixed_chunks_as_streams_parallel(
    input: &[u8],
    options: &Bzip2Options,
    threads: u32,
) -> Result<Option<Vec<Vec<u8>>>> {
    if threads <= 1 {
        return Ok(None);
    }

    let fixed_block_len = fixed_chunk_len(options.block_size_100k);
    if input.len() <= fixed_block_len {
        return Ok(None);
    }

    match encode_fixed_chunks_as_streams_parallel(input, fixed_block_len, options, threads) {
        Ok(streams) => Ok(Some(streams)),
        Err(Error::Format("bzip2 RLE block exceeds configured size")) => Ok(None),
        Err(error) => Err(error),
    }
}

fn encode_stream_without_fixed_chunks(
    input: &[u8],
    options: &Bzip2Options,
    threads: u32,
) -> Result<Vec<u8>> {
    let raw_blocks = split_raw_blocks(input, options.block_size_100k);
    if threads > 1 && raw_blocks.len() > 1 {
        let streams = encode_raw_blocks_as_streams_parallel(input, &raw_blocks, options, threads)?;
        let output = assemble_streams(streams);
        return Ok(output);
    }

    let encoded_blocks = if threads <= 1 || raw_blocks.len() <= 1 {
        raw_blocks
            .iter()
            .map(|range| block::encode(&input[range.start..range.end], options.block_size_100k))
            .collect::<Result<Vec<_>>>()?
    } else {
        encode_raw_blocks_parallel(input, &raw_blocks, options, threads)?
    };

    let encoded_size: usize = encoded_blocks.iter().map(|block| block.bytes.len()).sum();
    let mut output = Vec::with_capacity(4 + encoded_size + 16);
    output.extend_from_slice(b"BZh");
    output.push(b'0' + options.block_size_100k);

    let mut writer = BitWriter::with_capacity(encoded_size + 16);
    let mut combined_crc = 0u32;

    for encoded in &encoded_blocks {
        writer.write_bit_slice(&encoded.bytes, encoded.bit_len);
        combined_crc = crc::update_combined_crc(combined_crc, encoded.crc);
    }

    writer.write_bits(block::EOS_MAGIC, 48);
    writer.write_bits(u64::from(combined_crc), 32);
    output.extend_from_slice(&writer.finish());

    Ok(output)
}

fn assemble_streams(streams: Vec<Vec<u8>>) -> Vec<u8> {
    let total_size: usize = streams.iter().map(Vec::len).sum();
    let mut output = Vec::with_capacity(total_size);
    for stream in streams {
        output.extend_from_slice(&stream);
    }
    output
}

fn fixed_chunk_len(block_size_100k: u8) -> usize {
    let max_block_len = block::max_block_len(block_size_100k);
    let margin = if block_size_100k == 9 { 2_000 } else { 944 };
    max_block_len.saturating_sub(margin)
}

fn encode_fixed_chunks_as_streams_parallel(
    input: &[u8],
    chunk_len: usize,
    options: &Bzip2Options,
    threads: u32,
) -> Result<Vec<Vec<u8>>> {
    let pool = parallel_pool(threads)?;
    pool.install(|| {
        input
            .par_chunks(chunk_len)
            .map(|chunk| {
                let encoded = block::encode_with_huffman_passes(
                    chunk,
                    options.block_size_100k,
                    huffman_refinement_passes_for_fixed_chunk(options.block_size_100k),
                )?;
                Ok(wrap_single_block_stream(&encoded, options.block_size_100k))
            })
            .collect::<Result<Vec<_>>>()
    })
}

fn huffman_refinement_passes_for_fixed_chunk(block_size_100k: u8) -> usize {
    if block_size_100k == 9 { 0 } else { 3 }
}

fn encode_raw_blocks_as_streams_parallel(
    input: &[u8],
    raw_blocks: &[RawBlock],
    options: &Bzip2Options,
    threads: u32,
) -> Result<Vec<Vec<u8>>> {
    let pool = parallel_pool(threads)?;
    pool.install(|| {
        raw_blocks
            .par_iter()
            .map(|range| {
                let encoded =
                    block::encode(&input[range.start..range.end], options.block_size_100k)?;
                Ok(wrap_single_block_stream(&encoded, options.block_size_100k))
            })
            .collect::<Result<Vec<_>>>()
    })
}

fn wrap_single_block_stream(encoded: &block::EncodedBlock, block_size_100k: u8) -> Vec<u8> {
    let mut output = Vec::with_capacity(4 + encoded.bytes.len() + 16);
    output.extend_from_slice(b"BZh");
    output.push(b'0' + block_size_100k);

    let mut writer = BitWriter::with_capacity(encoded.bytes.len() + 16);
    writer.write_bit_slice(&encoded.bytes, encoded.bit_len);
    writer.write_bits(block::EOS_MAGIC, 48);
    writer.write_bits(u64::from(encoded.crc), 32);
    output.extend_from_slice(&writer.finish());

    output
}

fn encode_raw_blocks_parallel(
    input: &[u8],
    raw_blocks: &[RawBlock],
    options: &Bzip2Options,
    threads: u32,
) -> Result<Vec<block::EncodedBlock>> {
    let pool = parallel_pool(threads)?;
    pool.install(|| {
        raw_blocks
            .par_iter()
            .map(|range| block::encode(&input[range.start..range.end], options.block_size_100k))
            .collect::<Result<Vec<_>>>()
    })
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn decode_stream(input: &[u8]) -> Result<Vec<u8>> {
    decode_stream_with_threads(input, available_threads())
}

pub fn decode_stream_with_threads(input: &[u8], threads: u32) -> Result<Vec<u8>> {
    if threads <= 1 {
        return decode_stream_serial(input);
    }

    decode_stream_parallel(input, threads).or_else(|_| decode_stream_serial(input))
}

fn decode_stream_parallel(input: &[u8], threads: u32) -> Result<Vec<u8>> {
    let frames = split_stream_frames(input)?;
    let pool = parallel_pool(threads)?;

    if frames.len() <= 1 {
        return decode_one_stream_parallel(input, &pool).map(|decoded| decoded.bytes);
    }

    let decoded_streams = pool.install(|| {
        frames
            .par_iter()
            .map(|frame| decode_one_stream_serial(&input[frame.start..frame.end]))
            .collect::<Result<Vec<_>>>()
    })?;
    let total_size: usize = decoded_streams
        .iter()
        .map(|stream| stream.bytes.len())
        .sum();
    let mut output = Vec::with_capacity(total_size);

    for decoded in decoded_streams {
        output.extend_from_slice(&decoded.bytes);
    }

    Ok(output)
}

fn decode_stream_serial(input: &[u8]) -> Result<Vec<u8>> {
    let mut offset = 0usize;
    let mut output = Vec::new();
    let mut saw_stream = false;

    while offset < input.len() {
        let decoded = decode_one_stream_serial(&input[offset..])?;
        output.extend_from_slice(&decoded.bytes);
        offset += decoded.consumed;
        saw_stream = true;
    }

    if !saw_stream {
        return Err(Error::Format("empty bzip2 input"));
    }

    Ok(output)
}

#[derive(Clone, Copy)]
struct StreamFrame {
    start: usize,
    end: usize,
}

fn split_stream_frames(input: &[u8]) -> Result<Vec<StreamFrame>> {
    let mut frames = Vec::new();
    let mut offset = 0usize;

    while offset < input.len() {
        let consumed = stream_consumed_fast(&input[offset..])?;
        frames.push(StreamFrame {
            start: offset,
            end: offset + consumed,
        });
        offset += consumed;
    }

    if frames.is_empty() {
        return Err(Error::Format("empty bzip2 input"));
    }

    Ok(frames)
}

fn stream_consumed_fast(input: &[u8]) -> Result<usize> {
    if input.len() < 4 {
        return Err(Error::Format("bzip2 stream is too short"));
    }
    if input[0..3] != *b"BZh" {
        return Err(Error::Format("bad bzip2 stream header magic"));
    }

    let block_size_100k = input[3]
        .checked_sub(b'0')
        .ok_or(Error::Format("bad bzip2 block size marker"))?;
    if !(1..=9).contains(&block_size_100k) {
        return Err(Error::Format("bad bzip2 block size marker"));
    }

    let body = &input[4..];
    let eos_marker = find_eos_marker(body)?;
    let mut reader = BitReader::new_at(body, eos_marker.bit_pos + 48);
    let _expected_crc = reader.read_bits(32)?;
    reader.align_to_byte();

    Ok(4 + reader.consumed_bytes())
}

pub fn inspect_stream(input: &[u8]) -> Result<StreamInfo> {
    let mut offset = 0usize;
    let mut streams = 0u64;
    let mut blocks = 0u64;
    let mut uncompressed_size = 0u64;

    while offset < input.len() {
        let decoded = decode_one_stream(&input[offset..])?;
        streams += 1;
        blocks += decoded.blocks;
        uncompressed_size += decoded.uncompressed_size;
        offset += decoded.consumed;
    }

    if streams == 0 {
        return Err(Error::Format("empty bzip2 input"));
    }

    Ok(StreamInfo {
        blocks,
        compressed_size: input.len() as u64,
        streams,
        uncompressed_size,
    })
}

struct DecodedStream {
    bytes: Vec<u8>,
    blocks: u64,
    consumed: usize,
    uncompressed_size: u64,
}

fn decode_one_stream(input: &[u8]) -> Result<DecodedStream> {
    let threads = available_threads();
    if threads <= 1 {
        return decode_one_stream_serial(input);
    }

    let pool = parallel_pool(threads)?;
    decode_one_stream_parallel(input, &pool).or_else(|_| decode_one_stream_serial(input))
}

fn decode_one_stream_parallel(input: &[u8], pool: &rayon::ThreadPool) -> Result<DecodedStream> {
    if input.len() < 4 {
        return Err(Error::Format("bzip2 stream is too short"));
    }
    if input[0..3] != *b"BZh" {
        return Err(Error::Format("bad bzip2 stream header magic"));
    }

    let block_size_100k = input[3]
        .checked_sub(b'0')
        .ok_or(Error::Format("bad bzip2 block size marker"))?;
    if !(1..=9).contains(&block_size_100k) {
        return Err(Error::Format("bad bzip2 block size marker"));
    }

    let body = &input[4..];
    let (block_markers, eos_marker) = find_markers_until_eos(body)?;

    let decoded_blocks = pool.install(|| {
        block_markers
            .par_iter()
            .map(|marker| {
                let mut reader = BitReader::new_at(body, marker.bit_pos + 48);
                block::decode_after_magic(&mut reader, block_size_100k)
            })
            .collect::<Result<Vec<_>>>()
    })?;

    let mut output = Vec::new();
    let mut combined_crc = 0u32;

    for decoded in decoded_blocks {
        combined_crc = crc::update_combined_crc(combined_crc, decoded.crc);
        output.extend_from_slice(&decoded.bytes);
    }

    let mut reader = BitReader::new_at(body, eos_marker.bit_pos + 48);
    let expected_crc = reader.read_bits(32)?;
    if combined_crc != expected_crc {
        return Err(Error::Format("bzip2 stream combined CRC mismatch"));
    }

    reader.align_to_byte();
    Ok(DecodedStream {
        blocks: block_markers.len() as u64,
        consumed: 4 + reader.consumed_bytes(),
        uncompressed_size: output.len() as u64,
        bytes: output,
    })
}

fn decode_one_stream_serial(input: &[u8]) -> Result<DecodedStream> {
    if input.len() < 4 {
        return Err(Error::Format("bzip2 stream is too short"));
    }
    if input[0..3] != *b"BZh" {
        return Err(Error::Format("bad bzip2 stream header magic"));
    }

    let block_size_100k = input[3]
        .checked_sub(b'0')
        .ok_or(Error::Format("bad bzip2 block size marker"))?;
    if !(1..=9).contains(&block_size_100k) {
        return Err(Error::Format("bad bzip2 block size marker"));
    }

    let mut reader = BitReader::new(&input[4..]);
    let mut output = Vec::new();
    let mut blocks = 0u64;
    let mut combined_crc = 0u32;

    loop {
        let magic = reader.read_bits_u64(48)?;
        if magic == block::BLOCK_MAGIC {
            let decoded = block::decode_after_magic(&mut reader, block_size_100k)?;
            combined_crc = crc::update_combined_crc(combined_crc, decoded.crc);
            blocks += 1;
            output.extend_from_slice(&decoded.bytes);
        } else if magic == block::EOS_MAGIC {
            let expected_crc = reader.read_bits(32)?;
            if combined_crc != expected_crc {
                return Err(Error::Format("bzip2 stream combined CRC mismatch"));
            }

            reader.align_to_byte();
            let consumed = 4 + reader.consumed_bytes();
            return Ok(DecodedStream {
                blocks,
                consumed,
                uncompressed_size: output.len() as u64,
                bytes: output,
            });
        } else {
            return Err(Error::Format("bad bzip2 block magic"));
        }
    }
}

#[derive(Clone, Copy)]
struct Marker {
    bit_pos: u64,
}

fn find_markers_until_eos(data: &[u8]) -> Result<(Vec<Marker>, Marker)> {
    let mut markers = Vec::new();
    let total_bits = (data.len() as u64) * 8;
    let mut window = 0u64;
    let mask = (1u64 << 48) - 1;

    for bit_pos in 0..total_bits {
        window = ((window << 1) | u64::from(bit_at(data, bit_pos))) & mask;
        if bit_pos < 47 {
            continue;
        }

        if window == block::BLOCK_MAGIC {
            markers.push(Marker {
                bit_pos: bit_pos + 1 - 48,
            });
        } else if window == block::EOS_MAGIC {
            return Ok((
                markers,
                Marker {
                    bit_pos: bit_pos + 1 - 48,
                },
            ));
        }
    }

    Err(Error::Format("missing bzip2 end-of-stream marker"))
}

fn find_eos_marker(data: &[u8]) -> Result<Marker> {
    let total_bits = (data.len() as u64) * 8;
    let mut window = 0u64;
    let mask = (1u64 << 48) - 1;

    for bit_pos in 0..total_bits {
        window = ((window << 1) | u64::from(bit_at(data, bit_pos))) & mask;
        if bit_pos < 47 {
            continue;
        }

        if window == block::EOS_MAGIC {
            return Ok(Marker {
                bit_pos: bit_pos + 1 - 48,
            });
        }
    }

    Err(Error::Format("missing bzip2 end-of-stream marker"))
}

fn bit_at(data: &[u8], bit_pos: u64) -> bool {
    let byte = data[(bit_pos / 8) as usize];
    let shift = 7 - (bit_pos % 8);
    ((byte >> shift) & 1) != 0
}

fn split_raw_blocks(input: &[u8], block_size_100k: u8) -> Vec<RawBlock> {
    let max_rle_len = block::max_block_len(block_size_100k);
    let mut blocks = Vec::new();
    let mut start = 0usize;

    while start < input.len() {
        let end = next_block_end(input, start, max_rle_len);
        blocks.push(RawBlock { start, end });
        start = end;
    }

    blocks
}

fn next_block_end(input: &[u8], start: usize, max_rle_len: usize) -> usize {
    let mut index = start;
    let mut encoded_len = 0usize;
    let mut end = start;

    while index < input.len() {
        let byte = input[index];
        let mut run = 1usize;
        while index + run < input.len() && input[index + run] == byte {
            run += 1;
        }

        if run <= 3 {
            if encoded_len + run > max_rle_len {
                return end.max(start + 1);
            }
            encoded_len += run;
            end += run;
        } else if !try_add_run(run, &mut encoded_len, &mut end, max_rle_len) {
            return end.max(start + 1);
        }

        index += run;
    }

    end
}

fn try_add_run(
    mut run: usize,
    encoded_len: &mut usize,
    end: &mut usize,
    max_rle_len: usize,
) -> bool {
    while run > 259 {
        if *encoded_len + 5 > max_rle_len {
            return false;
        }

        *encoded_len += 5;
        *end += 259;
        run -= 259;
    }

    let run_len = block::rle_encoded_len_for_run(run);
    if *encoded_len + run_len > max_rle_len {
        return false;
    }

    *encoded_len += run_len;
    *end += run;

    true
}

fn validate_options(options: &Bzip2Options) -> Result<()> {
    if !(1..=9).contains(&options.block_size_100k) {
        return Err(Error::Usage("bzip2 level must be between 1 and 9"));
    }

    Ok(())
}

fn parallel_pool(threads: u32) -> Result<rayon::ThreadPool> {
    rayon::ThreadPoolBuilder::new()
        .num_threads((threads as usize).max(1))
        .build()
        .map_err(|error| Error::Message(error.to_string()))
}

#[cfg_attr(not(test), allow(dead_code))]
fn available_threads() -> u32 {
    std::thread::available_parallelism()
        .map(|count| count.get() as u32)
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::{Bzip2Options, decode_stream, encode_stream};
    use std::process::Command;

    fn options() -> Bzip2Options {
        Bzip2Options {
            block_size_100k: 1,
            threads: 2,
        }
    }

    #[test]
    fn bzip2_round_trips_small_inputs() {
        for input in [
            b"".as_slice(),
            b"hello world",
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            b"The quick brown fox jumps over the lazy dog. The quick brown fox.",
        ] {
            let encoded = encode_stream(input, &options()).unwrap();
            let decoded = decode_stream(&encoded).unwrap();
            assert_eq!(decoded, input);
        }
    }

    #[test]
    fn bzip2_round_trips_multiple_blocks() {
        let mut input = Vec::new();
        for index in 0..350_000 {
            input.push((index * 31 % 251) as u8);
        }

        let encoded = encode_stream(&input, &options()).unwrap();
        let decoded = decode_stream(&encoded).unwrap();
        assert_eq!(decoded, input);
    }

    #[test]
    fn bzip2_splitter_does_not_overfill_on_short_rle_expansion() {
        let mut input = Vec::with_capacity(100_000);
        for index in 0..99_996 {
            input.push(if index % 2 == 0 { b'a' } else { b'b' });
        }
        input.extend_from_slice(b"    ");

        let encoded = encode_stream(&input, &options()).unwrap();
        let decoded = decode_stream(&encoded).unwrap();
        assert_eq!(decoded, input);
    }

    #[test]
    fn stock_bzip2_reads_our_output_when_available() {
        if !command_exists("bzip2") {
            return;
        }

        let input = b"bzip2 compatibility smoke\nbzip2 compatibility smoke\n";
        let encoded = encode_stream(input, &options()).unwrap();
        let path = temp_path("ours", "bz2");
        std::fs::write(&path, encoded).unwrap();

        let output = Command::new("bzip2")
            .args(["-dc", path.to_str().unwrap()])
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&path);

        assert!(output.status.success());
        assert_eq!(output.stdout, input);
    }

    #[test]
    fn our_decoder_reads_stock_tools_when_available() {
        for tool in ["bzip2", "pbzip2"] {
            if !command_exists(tool) {
                continue;
            }

            let input = b"stock compatibility smoke\nstock compatibility smoke\n";
            let input_path = temp_path(tool, "txt");
            std::fs::write(&input_path, input).unwrap();
            let output = Command::new(tool)
                .args(["-9", "-c", input_path.to_str().unwrap()])
                .output()
                .unwrap();
            let _ = std::fs::remove_file(&input_path);

            assert!(output.status.success());
            let decoded = decode_stream(&output.stdout).unwrap();
            assert_eq!(decoded, input);
        }
    }

    fn command_exists(command: &str) -> bool {
        Command::new("sh")
            .args(["-c", &format!("command -v {command} >/dev/null 2>&1")])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn temp_path(label: &str, extension: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "compress-bzip2-test-{}-{label}.{extension}",
            std::process::id()
        ))
    }
}
