use crate::algorithms::checks::{self, CheckType};
use crate::algorithms::lzma::{CompressionMode, LzmaProperties, MatchFinderKind};
use crate::algorithms::lzma2;
use crate::error::{Error, Result};

use rayon::prelude::*;

use std::io::{Read, Write};

const FOOTER_MAGIC: [u8; 2] = [0x59, 0x5A];
const HEADER_MAGIC: [u8; 6] = [0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00];
const LZMA2_FILTER_ID: u64 = 0x21;
const PARALLEL_BLOCK_SIZE_LEVEL_0_TO_3: u64 = 8 * 1024 * 1024;
const PARALLEL_BLOCK_SIZE_LEVEL_4_TO_6: u64 = 14 * 1024 * 1024;
const PARALLEL_BLOCK_SIZE_LEVEL_7_TO_9: u64 = 32 * 1024 * 1024;
const PARALLEL_BATCH_MAX_BLOCKS: usize = 64;

#[derive(Clone, Debug)]
pub struct XzOptions {
    pub block_size: Option<u64>,
    pub check: CheckType,
    pub depth: u32,
    pub dict_size: u32,
    pub lc: u32,
    pub lp: u32,
    pub match_finder: MatchFinderKind,
    pub mode: CompressionMode,
    pub nice: u32,
    pub pb: u32,
    pub threads: u32,
}

#[derive(Debug)]
pub struct StreamInfo {
    pub blocks: u64,
    pub check: CheckType,
    pub compressed_size: u64,
    pub streams: u64,
    pub uncompressed_size: u64,
}

struct Block {
    bytes: Vec<u8>,
    uncompressed_size: u64,
    unpadded_size: u64,
}

#[derive(Clone, Copy)]
struct IndexRecord {
    uncompressed_size: u64,
    unpadded_size: u64,
}

#[cfg(test)]
pub fn encode_stream(input: &[u8], options: &XzOptions) -> Result<Vec<u8>> {
    validate_options(options)?;

    let block_size = effective_block_size(options)?;
    let blocks = encode_blocks(input, options, block_size)?;
    let stream_flags = [0x00, options.check.xz_id()];
    let mut output = Vec::new();

    write_stream_header(&mut output, stream_flags);

    for block in &blocks {
        output.extend_from_slice(&block.bytes);
    }

    let index = encode_index(&blocks)?;
    output.extend_from_slice(&index);
    write_stream_footer(&mut output, stream_flags, index.len() as u64)?;

    Ok(output)
}

pub fn encode_reader_to_writer<R: Read, W: Write>(
    mut reader: R,
    mut writer: W,
    options: &XzOptions,
) -> Result<()> {
    validate_options(options)?;

    let block_size = effective_block_size(options)?;
    let stream_flags = [0x00, options.check.xz_id()];
    let mut header = Vec::new();

    write_stream_header(&mut header, stream_flags);
    writer.write_all(&header)?;

    let records = if options.threads <= 1 {
        encode_reader_blocks_serial(&mut reader, &mut writer, options, block_size)?
    } else {
        encode_reader_blocks_parallel(&mut reader, &mut writer, options, block_size)?
    };
    let index = encode_index_records(&records)?;
    writer.write_all(&index)?;

    let mut footer = Vec::new();
    write_stream_footer(&mut footer, stream_flags, index.len() as u64)?;
    writer.write_all(&footer)?;

    Ok(())
}

pub fn decode_stream(input: &[u8]) -> Result<Vec<u8>> {
    let stream = parse_single_stream(input)?;
    let mut parsed_blocks = Vec::with_capacity(stream.records.len());
    let mut block_offset = 12usize;

    for record in &stream.records {
        let block = parse_block(input, block_offset, *record, stream.check)?;
        block_offset = block.next_offset;
        parsed_blocks.push(block);
    }

    if block_offset != stream.index_offset {
        return Err(Error::Format("xz index offset mismatch"));
    }

    let decoded_blocks = if parsed_blocks.len() > 1 {
        parsed_blocks
            .par_iter()
            .zip(stream.records.par_iter())
            .map(|(block, record)| decode_parsed_block(block, *record, stream.check))
            .collect::<Result<Vec<_>>>()?
    } else {
        parsed_blocks
            .iter()
            .zip(stream.records.iter())
            .map(|(block, record)| decode_parsed_block(block, *record, stream.check))
            .collect::<Result<Vec<_>>>()?
    };
    let total_size: usize = decoded_blocks.iter().map(Vec::len).sum();
    let mut output = Vec::new();
    output.reserve(total_size);

    for block_output in decoded_blocks {
        output.extend_from_slice(&block_output);
    }

    Ok(output)
}

fn decode_parsed_block(
    block: &ParsedBlock<'_>,
    record: IndexRecord,
    check: CheckType,
) -> Result<Vec<u8>> {
    let block_output = lzma2::decode(block.compressed_data, block.dict_size)?;

    if block_output.len() as u64 != record.uncompressed_size {
        return Err(Error::Format("xz block uncompressed size mismatch"));
    }

    let expected_check = checks::check_bytes(check, &block_output);
    if expected_check != block.check_data {
        return Err(Error::Format("xz block integrity check mismatch"));
    }

    Ok(block_output)
}

pub fn inspect_stream(input: &[u8]) -> Result<StreamInfo> {
    let stream = parse_single_stream(input)?;
    let uncompressed_size = stream
        .records
        .iter()
        .map(|record| record.uncompressed_size)
        .sum();

    Ok(StreamInfo {
        blocks: stream.records.len() as u64,
        check: stream.check,
        compressed_size: input.len() as u64,
        streams: 1,
        uncompressed_size,
    })
}

#[cfg(test)]
fn encode_blocks(input: &[u8], options: &XzOptions, block_size: usize) -> Result<Vec<Block>> {
    let ranges = block_ranges(input.len(), block_size);

    if options.threads <= 1 || ranges.len() <= 1 {
        let mut blocks = Vec::with_capacity(ranges.len());
        for (start, end) in ranges {
            blocks.push(encode_block(&input[start..end], options)?);
        }

        return Ok(blocks);
    }

    encode_blocks_parallel(input, options, &ranges)
}

#[cfg(test)]
fn encode_blocks_parallel(
    input: &[u8],
    options: &XzOptions,
    ranges: &[(usize, usize)],
) -> Result<Vec<Block>> {
    with_parallel_pool(options.threads, || {
        ranges
            .par_iter()
            .map(|&(start, end)| encode_block(&input[start..end], options))
            .collect()
    })
}

fn encode_reader_blocks_serial<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    options: &XzOptions,
    block_size: usize,
) -> Result<Vec<IndexRecord>> {
    let mut records = Vec::new();

    loop {
        let input = read_input_block(reader, block_size)?;
        if input.is_empty() {
            break;
        }

        let block = encode_block(&input, options)?;
        write_encoded_block(writer, block, &mut records)?;
    }

    if records.is_empty() {
        let block = encode_block(&[], options)?;
        write_encoded_block(writer, block, &mut records)?;
    }

    Ok(records)
}

fn encode_reader_blocks_parallel<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    options: &XzOptions,
    block_size: usize,
) -> Result<Vec<IndexRecord>> {
    let batch_size = streaming_batch_size(options.threads);
    let mut records = Vec::new();
    let pool = parallel_pool(options.threads)?;

    loop {
        let inputs = read_input_batch(reader, block_size, batch_size)?;
        if inputs.is_empty() {
            break;
        }

        let blocks = encode_owned_blocks_parallel(&pool, &inputs, options)?;
        for block in blocks {
            write_encoded_block(writer, block, &mut records)?;
        }
    }

    if records.is_empty() {
        let block = encode_block(&[], options)?;
        write_encoded_block(writer, block, &mut records)?;
    }

    Ok(records)
}

fn read_input_batch<R: Read>(
    reader: &mut R,
    block_size: usize,
    batch_size: usize,
) -> Result<Vec<Vec<u8>>> {
    let mut inputs = Vec::with_capacity(batch_size);

    while inputs.len() < batch_size {
        let input = read_input_block(reader, block_size)?;
        if input.is_empty() {
            break;
        }

        inputs.push(input);
    }

    Ok(inputs)
}

fn read_input_block<R: Read>(reader: &mut R, block_size: usize) -> Result<Vec<u8>> {
    let mut input = Vec::new();
    let mut buffer = [0u8; 64 * 1024];

    while input.len() < block_size {
        let limit = (block_size - input.len()).min(buffer.len());
        let read = reader.read(&mut buffer[..limit])?;
        if read == 0 {
            break;
        }

        input.extend_from_slice(&buffer[..read]);
    }

    Ok(input)
}

fn encode_owned_blocks_parallel(
    pool: &rayon::ThreadPool,
    inputs: &[Vec<u8>],
    options: &XzOptions,
) -> Result<Vec<Block>> {
    pool.install(|| {
        inputs
            .par_iter()
            .map(|input| encode_block(input, options))
            .collect()
    })
}

fn write_encoded_block<W: Write>(
    writer: &mut W,
    block: Block,
    records: &mut Vec<IndexRecord>,
) -> Result<()> {
    let record = IndexRecord {
        uncompressed_size: block.uncompressed_size,
        unpadded_size: block.unpadded_size,
    };

    writer.write_all(&block.bytes)?;
    records.push(record);

    Ok(())
}

fn streaming_batch_size(threads: u32) -> usize {
    (threads as usize).clamp(1, PARALLEL_BATCH_MAX_BLOCKS)
}

fn parallel_pool(threads: u32) -> Result<rayon::ThreadPool> {
    rayon::ThreadPoolBuilder::new()
        .num_threads((threads as usize).max(1))
        .build()
        .map_err(|error| Error::Message(error.to_string()))
}

#[cfg(test)]
fn with_parallel_pool<T, F>(threads: u32, operation: F) -> Result<T>
where
    T: Send,
    F: FnOnce() -> Result<T> + Send,
{
    parallel_pool(threads)?.install(operation)
}

fn encode_block(input: &[u8], options: &XzOptions) -> Result<Block> {
    let compressed = lzma2::encode(input, &lzma2_options(options))?;
    let header = encode_block_header(
        compressed.len() as u64,
        input.len() as u64,
        options.dict_size,
    )?;
    let block_padding = padding_len((header.len() + compressed.len()) as u64);
    let check = checks::check_bytes(options.check, input);
    let unpadded_size = header.len() as u64 + compressed.len() as u64 + check.len() as u64;
    let mut bytes =
        Vec::with_capacity(header.len() + compressed.len() + block_padding + check.len());

    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(&compressed);
    bytes.resize(bytes.len() + block_padding, 0);
    bytes.extend_from_slice(&check);

    Ok(Block {
        bytes,
        uncompressed_size: input.len() as u64,
        unpadded_size,
    })
}

fn lzma2_options(options: &XzOptions) -> lzma2::Lzma2Options {
    lzma2::Lzma2Options {
        depth: options.depth,
        dict_size: options.dict_size,
        match_finder: options.match_finder,
        mode: options.mode,
        nice: options.nice,
        properties: LzmaProperties {
            lc: options.lc,
            lp: options.lp,
            pb: options.pb,
        },
    }
}

fn encode_block_header(
    compressed_size: u64,
    uncompressed_size: u64,
    dict_size: u32,
) -> Result<Vec<u8>> {
    let mut header = Vec::new();
    header.push(0);
    header.push(0x40 | 0x80);
    encode_vli(compressed_size, &mut header)?;
    encode_vli(uncompressed_size, &mut header)?;
    encode_vli(LZMA2_FILTER_ID, &mut header)?;
    encode_vli(1, &mut header)?;
    header.push(encode_lzma2_dict_property(dict_size)?);

    while (header.len() + 4) % 4 != 0 {
        header.push(0);
    }

    let size = header.len() + 4;
    if size > 1024 {
        return Err(Error::Format("xz block header is too large"));
    }

    header[0] = (size / 4 - 1) as u8;
    let crc = checks::crc32(&header);
    header.extend_from_slice(&crc.to_le_bytes());

    Ok(header)
}

fn write_stream_header(output: &mut Vec<u8>, stream_flags: [u8; 2]) {
    output.extend_from_slice(&HEADER_MAGIC);
    output.extend_from_slice(&stream_flags);
    output.extend_from_slice(&checks::crc32(&stream_flags).to_le_bytes());
}

fn write_stream_footer(output: &mut Vec<u8>, stream_flags: [u8; 2], index_size: u64) -> Result<()> {
    if !index_size.is_multiple_of(4) {
        return Err(Error::Format("xz index is not aligned"));
    }

    let backward_size = index_size / 4 - 1;
    if backward_size > u64::from(u32::MAX) {
        return Err(Error::Format("xz index is too large"));
    }

    let mut fields = Vec::new();
    fields.extend_from_slice(&(backward_size as u32).to_le_bytes());
    fields.extend_from_slice(&stream_flags);

    output.extend_from_slice(&checks::crc32(&fields).to_le_bytes());
    output.extend_from_slice(&fields);
    output.extend_from_slice(&FOOTER_MAGIC);

    Ok(())
}

#[cfg(test)]
fn encode_index(blocks: &[Block]) -> Result<Vec<u8>> {
    let records: Vec<IndexRecord> = blocks
        .iter()
        .map(|block| IndexRecord {
            uncompressed_size: block.uncompressed_size,
            unpadded_size: block.unpadded_size,
        })
        .collect();

    encode_index_records(&records)
}

fn encode_index_records(records: &[IndexRecord]) -> Result<Vec<u8>> {
    let mut index = Vec::new();
    index.push(0x00);
    encode_vli(records.len() as u64, &mut index)?;

    for record in records {
        encode_vli(record.unpadded_size, &mut index)?;
        encode_vli(record.uncompressed_size, &mut index)?;
    }

    while (index.len() + 4) % 4 != 0 {
        index.push(0);
    }

    let crc = checks::crc32(&index);
    index.extend_from_slice(&crc.to_le_bytes());

    Ok(index)
}

fn parse_single_stream(input: &[u8]) -> Result<ParsedStream> {
    if input.len() < 24 {
        return Err(Error::Format("xz stream is too short"));
    }

    if input[..6] != HEADER_MAGIC {
        return Err(Error::Format("bad xz stream header magic"));
    }

    let header_flags = [input[6], input[7]];
    validate_stream_flags(header_flags)?;
    validate_crc32(&input[6..8], &input[8..12], "stream header")?;

    let footer_offset = input.len() - 12;
    if input[footer_offset + 10..footer_offset + 12] != FOOTER_MAGIC {
        return Err(Error::Unsupported("concatenated streams or stream padding"));
    }

    validate_crc32(
        &input[footer_offset + 4..footer_offset + 10],
        &input[footer_offset..footer_offset + 4],
        "stream footer",
    )?;

    let backward_size = read_u32_le(&input[footer_offset + 4..footer_offset + 8]) as u64;
    let index_size = (backward_size + 1)
        .checked_mul(4)
        .ok_or(Error::Format("xz index size overflow"))?;
    let index_offset = footer_offset
        .checked_sub(index_size as usize)
        .ok_or(Error::Format("xz index points before stream"))?;
    let footer_flags = [input[footer_offset + 8], input[footer_offset + 9]];

    if header_flags != footer_flags {
        return Err(Error::Format("xz stream flags mismatch"));
    }

    let records = parse_index(&input[index_offset..footer_offset])?;

    Ok(ParsedStream {
        check: CheckType::from_xz_id(header_flags[1])?,
        index_offset,
        records,
    })
}

fn parse_index(data: &[u8]) -> Result<Vec<IndexRecord>> {
    if data.len() < 4 {
        return Err(Error::Format("xz index is too short"));
    }

    validate_crc32(&data[..data.len() - 4], &data[data.len() - 4..], "index")?;

    let mut cursor = Cursor::new(&data[..data.len() - 4]);
    if cursor.read_u8()? != 0x00 {
        return Err(Error::Format("xz index missing indicator"));
    }

    let count = cursor.read_vli()?;
    if count > 1_000_000 {
        return Err(Error::Format("xz index record count too large"));
    }

    let mut records = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let unpadded_size = cursor.read_vli()?;
        let uncompressed_size = cursor.read_vli()?;
        records.push(IndexRecord {
            uncompressed_size,
            unpadded_size,
        });
    }

    while cursor.remaining() > 0 {
        if cursor.read_u8()? != 0 {
            return Err(Error::Format("xz index padding is not zero"));
        }
    }

    Ok(records)
}

fn parse_block<'a>(
    input: &'a [u8],
    offset: usize,
    record: IndexRecord,
    check: CheckType,
) -> Result<ParsedBlock<'a>> {
    let header = parse_block_header_slice(input, offset)?;
    let header_size = header.len();
    validate_crc32(
        &header[..header.len() - 4],
        &header[header.len() - 4..],
        "block header",
    )?;
    let fields = parse_block_header_fields(header)?;
    let check_size = check.size();
    let compressed_size = block_compressed_size(&fields, record, header_size, check_size)?;

    if let Some(size) = fields.uncompressed_size
        && size != record.uncompressed_size
    {
        return Err(Error::Format("block and index uncompressed sizes differ"));
    }

    parse_block_data(
        input,
        offset,
        header_size,
        compressed_size,
        check_size,
        fields.dict_size,
    )
}

fn parse_block_header_slice(input: &[u8], offset: usize) -> Result<&[u8]> {
    let header_size_byte = *input
        .get(offset)
        .ok_or(Error::Format("missing block header"))?;
    if header_size_byte == 0 {
        return Err(Error::Format("unexpected index indicator"));
    }

    let header_size = (usize::from(header_size_byte) + 1) * 4;
    let header_end = offset
        .checked_add(header_size)
        .ok_or(Error::Format("block header size overflow"))?;

    if header_end > input.len() {
        return Err(Error::Format("truncated block header"));
    }

    Ok(&input[offset..header_end])
}

fn block_compressed_size(
    fields: &BlockHeaderFields,
    record: IndexRecord,
    header_size: usize,
    check_size: u64,
) -> Result<u64> {
    if let Some(size) = fields.compressed_size {
        return Ok(size);
    }

    record
        .unpadded_size
        .checked_sub(header_size as u64 + check_size)
        .ok_or(Error::Format("bad block size"))
}

fn parse_block_data<'a>(
    input: &'a [u8],
    offset: usize,
    header_size: usize,
    compressed_size: u64,
    check_size: u64,
    dict_size: u32,
) -> Result<ParsedBlock<'a>> {
    let data_offset = offset
        .checked_add(header_size)
        .ok_or(Error::Format("block header size overflow"))?;
    let data_end = data_offset
        .checked_add(compressed_size as usize)
        .ok_or(Error::Format("compressed size overflow"))?;
    let padding = padding_len(header_size as u64 + compressed_size);
    let check_offset = data_end
        .checked_add(padding)
        .ok_or(Error::Format("block padding overflow"))?;
    let check_end = check_offset
        .checked_add(check_size as usize)
        .ok_or(Error::Format("check size overflow"))?;

    if check_end > input.len() {
        return Err(Error::Format("truncated block data"));
    }

    for &byte in &input[data_end..check_offset] {
        if byte != 0 {
            return Err(Error::Format("block padding is not zero"));
        }
    }

    Ok(ParsedBlock {
        check_data: &input[check_offset..check_end],
        compressed_data: &input[data_offset..data_end],
        dict_size,
        next_offset: check_end,
    })
}

fn parse_block_header_fields(header: &[u8]) -> Result<BlockHeaderFields> {
    let mut cursor = Cursor::new(&header[..header.len() - 4]);
    let _header_size = cursor.read_u8()?;
    let flags = cursor.read_u8()?;

    if flags & 0x3C != 0 {
        return Err(Error::Format("reserved block flag set"));
    }

    if flags & 0x03 != 0 {
        return Err(Error::Unsupported(
            "xz filter chain with more than one filter",
        ));
    }

    let compressed_size = if flags & 0x40 != 0 {
        Some(cursor.read_vli()?)
    } else {
        None
    };

    let uncompressed_size = if flags & 0x80 != 0 {
        Some(cursor.read_vli()?)
    } else {
        None
    };

    let filter_id = cursor.read_vli()?;
    if filter_id != LZMA2_FILTER_ID {
        return Err(Error::Unsupported("non-LZMA2 xz filter"));
    }

    let property_size = cursor.read_vli()?;
    if property_size != 1 {
        return Err(Error::Format("LZMA2 filter property size must be one"));
    }

    let property = cursor.read_u8()?;
    let dict_size = decode_lzma2_dict_property(property)?;

    while cursor.remaining() > 0 {
        if cursor.read_u8()? != 0 {
            return Err(Error::Format("block header padding is not zero"));
        }
    }

    Ok(BlockHeaderFields {
        compressed_size,
        dict_size,
        uncompressed_size,
    })
}

fn validate_options(options: &XzOptions) -> Result<()> {
    if options.dict_size < 4096 {
        return Err(Error::Usage("dictionary must be at least 4 KiB"));
    }

    if options.lc > 4 || options.lp > 4 || options.pb > 4 {
        return Err(Error::Usage("LZMA property is out of range"));
    }

    if options.lc + options.lp > 4 {
        return Err(Error::Usage("lc + lp must be <= 4"));
    }

    Ok(())
}

fn effective_block_size(options: &XzOptions) -> Result<usize> {
    let size = options
        .block_size
        .unwrap_or_else(|| default_block_size(options));

    if size == 0 {
        return Err(Error::Usage("block size must be positive"));
    }

    if size > usize::MAX as u64 {
        return Err(Error::Usage("block size is too large"));
    }

    Ok(size as usize)
}

fn default_block_size(options: &XzOptions) -> u64 {
    let serial_default = (u64::from(options.dict_size) * 3).max(1024 * 1024);
    if options.threads <= 1 {
        return serial_default;
    }

    let dict_size = u64::from(options.dict_size);
    if dict_size < 4 * 1024 * 1024 {
        return (dict_size * 3).max(4 * 1024 * 1024);
    }

    if dict_size <= 4 * 1024 * 1024 {
        PARALLEL_BLOCK_SIZE_LEVEL_0_TO_3
    } else if dict_size <= 8 * 1024 * 1024 {
        PARALLEL_BLOCK_SIZE_LEVEL_4_TO_6
    } else {
        PARALLEL_BLOCK_SIZE_LEVEL_7_TO_9
    }
}

#[cfg(test)]
fn block_ranges(input_len: usize, block_size: usize) -> Vec<(usize, usize)> {
    if input_len == 0 {
        return vec![(0, 0)];
    }

    let mut ranges = Vec::new();
    let mut start = 0usize;

    while start < input_len {
        let end = (start + block_size).min(input_len);
        ranges.push((start, end));
        start = end;
    }

    ranges
}

fn validate_stream_flags(flags: [u8; 2]) -> Result<()> {
    if flags[0] != 0 {
        return Err(Error::Format("reserved stream flag set"));
    }

    let _check = CheckType::from_xz_id(flags[1])?;

    Ok(())
}

fn validate_crc32(data: &[u8], expected: &[u8], label: &'static str) -> Result<()> {
    if expected.len() != 4 {
        return Err(Error::Format("CRC32 field has wrong size"));
    }

    let actual = checks::crc32(data).to_le_bytes();
    if actual != expected {
        return Err(Error::Format(label));
    }

    Ok(())
}

fn encode_lzma2_dict_property(dict_size: u32) -> Result<u8> {
    for property in 0..=40 {
        if decode_lzma2_dict_property(property)? >= dict_size {
            return Ok(property);
        }
    }

    Err(Error::Usage("dictionary is too large for LZMA2"))
}

fn decode_lzma2_dict_property(property: u8) -> Result<u32> {
    if property > 40 {
        return Err(Error::Format("invalid LZMA2 dictionary property"));
    }

    if property == 40 {
        return Ok(u32::MAX);
    }

    let base = 2 + u32::from(property & 1);
    let shift = u32::from(property / 2) + 11;

    Ok(base << shift)
}

fn encode_vli(value: u64, output: &mut Vec<u8>) -> Result<()> {
    if value >= (1u64 << 63) {
        return Err(Error::Format("xz VLI is too large"));
    }

    let mut remaining = value;
    loop {
        let mut byte = (remaining & 0x7F) as u8;
        remaining >>= 7;

        if remaining != 0 {
            byte |= 0x80;
        }

        output.push(byte);

        if remaining == 0 {
            return Ok(());
        }
    }
}

fn read_u32_le(data: &[u8]) -> u32 {
    u32::from_le_bytes([data[0], data[1], data[2], data[3]])
}

fn padding_len(size: u64) -> usize {
    ((4 - (size % 4)) % 4) as usize
}

struct Cursor<'a> {
    data: &'a [u8],
    index: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Cursor<'a> {
        Cursor { data, index: 0 }
    }

    fn read_u8(&mut self) -> Result<u8> {
        if self.index < self.data.len() {
            let byte = self.data[self.index];
            self.index += 1;
            Ok(byte)
        } else {
            Err(Error::Format("unexpected end of xz data"))
        }
    }

    fn read_vli(&mut self) -> Result<u64> {
        let start = self.index;
        let mut value = 0u64;

        for shift in (0..=56).step_by(7) {
            let byte = self.read_u8()?;
            value |= u64::from(byte & 0x7F) << shift;

            if byte & 0x80 == 0 {
                let length = self.index - start;
                if length > 1 && value < (1u64 << (7 * (length - 1))) {
                    return Err(Error::Format("non-canonical xz VLI"));
                }

                return Ok(value);
            }
        }

        Err(Error::Format("xz VLI is too long"))
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.index
    }
}

struct BlockHeaderFields {
    compressed_size: Option<u64>,
    dict_size: u32,
    uncompressed_size: Option<u64>,
}

struct ParsedBlock<'a> {
    check_data: &'a [u8],
    compressed_data: &'a [u8],
    dict_size: u32,
    next_offset: usize,
}

struct ParsedStream {
    check: CheckType,
    index_offset: usize,
    records: Vec<IndexRecord>,
}

#[cfg(test)]
mod tests {
    use super::{
        XzOptions, decode_stream, default_block_size, encode_reader_to_writer, encode_stream,
        inspect_stream,
    };
    use crate::algorithms::checks::CheckType;
    use crate::algorithms::lzma::{CompressionMode, MatchFinderKind};

    fn test_options(block_size: u64) -> XzOptions {
        XzOptions {
            block_size: Some(block_size),
            check: CheckType::Crc64,
            depth: 192,
            dict_size: 1 << 20,
            lc: 3,
            lp: 0,
            match_finder: MatchFinderKind::Bt4,
            mode: CompressionMode::Normal,
            nice: 64,
            pb: 2,
            threads: 2,
        }
    }

    #[test]
    fn parallel_default_block_size_exposes_more_work() {
        let mut options = test_options(1);
        options.block_size = None;
        options.dict_size = 8 * 1024 * 1024;
        options.threads = 1;
        assert_eq!(default_block_size(&options), 24 * 1024 * 1024);

        options.threads = 4;
        assert_eq!(default_block_size(&options), 14 * 1024 * 1024);
    }

    #[test]
    fn round_trip_lzma2_xz() {
        let options = test_options(32);
        let input = b"hello hello hello hello hello hello";
        let encoded = encode_stream(input, &options).unwrap();
        let decoded = decode_stream(&encoded).unwrap();

        assert_eq!(decoded, input);
    }

    #[test]
    fn compressed_lzma2_xz_is_smaller_for_repeated_input() {
        let options = test_options(64 * 1024);
        let mut input = Vec::new();

        for index in 0..2048 {
            input.extend_from_slice(b"alpha beta gamma alpha beta gamma ");
            input.extend_from_slice(index.to_string().as_bytes());
            input.push(b'\n');
        }

        let encoded = encode_stream(&input, &options).unwrap();
        let decoded = decode_stream(&encoded).unwrap();

        assert_eq!(decoded, input);
        assert!(encoded.len() < input.len());
    }

    #[test]
    fn round_trip_random_like_lzma2_xz() {
        let options = test_options(32 * 1024);
        let mut input = Vec::new();
        let mut state = 0x1234_5678u32;

        for _ in 0..96 * 1024 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            input.push((state >> 24) as u8);
        }

        let encoded = encode_stream(&input, &options).unwrap();
        let decoded = decode_stream(&encoded).unwrap();

        assert_eq!(decoded, input);
    }

    #[test]
    fn uncompressed_then_compressed_lzma2_round_trip() {
        let options = test_options(512 * 1024);
        let mut input = Vec::new();
        let mut state = 0xCAFE_BABEu32;

        for _ in 0..96 * 1024 {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            input.push((state >> 24) as u8);
        }

        for index in 0..8192 {
            input.extend_from_slice(b"compressible tail alpha beta gamma ");
            input.extend_from_slice(index.to_string().as_bytes());
            input.push(b'\n');
        }

        let encoded = encode_stream(&input, &options).unwrap();
        let decoded = decode_stream(&encoded).unwrap();

        assert_eq!(decoded, input);
    }

    #[test]
    fn streaming_round_trip_lzma2_xz() {
        let mut options = test_options(32 * 1024);
        options.threads = 1;
        let mut input = Vec::new();

        for index in 0..4096 {
            input.extend_from_slice(b"streaming block input alpha beta gamma ");
            input.extend_from_slice(index.to_string().as_bytes());
            input.push(b'\n');
        }

        let mut encoded = Vec::new();
        encode_reader_to_writer(input.as_slice(), &mut encoded, &options).unwrap();
        let decoded = decode_stream(&encoded).unwrap();
        let info = inspect_stream(&encoded).unwrap();

        assert_eq!(decoded, input);
        assert!(info.blocks > 1);
    }

    #[test]
    fn streaming_parallel_round_trip_lzma2_xz() {
        let mut options = test_options(32 * 1024);
        options.threads = 4;
        let mut input = Vec::new();

        for index in 0..8192 {
            input.extend_from_slice(b"parallel streaming block input alpha beta gamma ");
            input.extend_from_slice(index.to_string().as_bytes());
            input.push(b'\n');
        }

        let mut encoded = Vec::new();
        encode_reader_to_writer(input.as_slice(), &mut encoded, &options).unwrap();
        let decoded = decode_stream(&encoded).unwrap();
        let info = inspect_stream(&encoded).unwrap();

        assert_eq!(decoded, input);
        assert!(info.blocks > 1);
    }

    #[test]
    fn large_block_contains_multiple_lzma2_chunks() {
        let options = test_options(1024 * 1024);
        let mut input = Vec::new();

        for index in 0..8192 {
            input.extend_from_slice(b"line ");
            input.extend_from_slice(index.to_string().as_bytes());
            input.extend_from_slice(b" alpha beta gamma alpha beta gamma\n");
        }

        let encoded = encode_stream(&input, &options).unwrap();
        let info = inspect_stream(&encoded).unwrap();
        let decoded = decode_stream(&encoded).unwrap();

        assert_eq!(info.blocks, 1);
        assert_eq!(decoded, input);
        assert!(encoded.len() < input.len());
    }
}
