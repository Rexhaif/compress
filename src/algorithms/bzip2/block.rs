use crate::algorithms::bzip2::bitstream::{BitReader, BitWriter};
use crate::algorithms::bzip2::{bwt, crc, huffman, mtf};
use crate::error::{Error, Result};

pub const BLOCK_MAGIC: u64 = 0x3141_5926_5359;
pub const EOS_MAGIC: u64 = 0x1772_4538_5090;

pub struct EncodedBlock {
    pub bytes: Vec<u8>,
    pub bit_len: u64,
    pub crc: u32,
}

pub struct DecodedBlock {
    pub bytes: Vec<u8>,
    pub crc: u32,
}

struct RleEncoded {
    bytes: Vec<u8>,
    used: [bool; 256],
}

pub fn encode(raw: &[u8], block_size_100k: u8) -> Result<EncodedBlock> {
    encode_with_huffman_passes(raw, block_size_100k, huffman::DEFAULT_REFINEMENT_PASSES)
}

pub fn encode_with_huffman_passes(
    raw: &[u8],
    block_size_100k: u8,
    huffman_refinement_passes: usize,
) -> Result<EncodedBlock> {
    if raw.is_empty() {
        return Err(Error::Format("bzip2 block cannot be empty"));
    }

    let rle = rle_encode(raw);
    let max_block_len = max_block_len(block_size_100k);
    if rle.bytes.len() > max_block_len {
        return Err(Error::Format("bzip2 RLE block exceeds configured size"));
    }

    let block_crc = crc::crc32(raw);
    let transformed = bwt::transform(&rle.bytes);
    if transformed.primary_index > 0xFF_FFFF {
        return Err(Error::Format("bzip2 BWT primary index is too large"));
    }

    let mtf = mtf::encode_with_used(&transformed.bytes, rle.used);
    let huffman_refinement_passes = if huffman_refinement_passes == 0 {
        adaptive_huffman_refinement_passes(mtf.symbols.len())
    } else {
        huffman_refinement_passes
    };
    let alpha_size = mtf.used_symbols.len() + 2;
    let mut writer = BitWriter::with_capacity(raw.len().min(rle.bytes.len()));

    writer.write_bits(BLOCK_MAGIC, 48);
    writer.write_bits(u64::from(block_crc), 32);
    writer.write_bit(false);
    writer.write_bits(transformed.primary_index as u64, 24);
    write_used_map(&mtf.used, &mut writer);
    huffman::encode_symbols_with_passes(
        &mtf.symbols,
        alpha_size,
        &mut writer,
        huffman_refinement_passes,
    )?;

    let (bytes, bit_len) = writer.into_parts();
    Ok(EncodedBlock {
        bytes,
        bit_len,
        crc: block_crc,
    })
}

fn adaptive_huffman_refinement_passes(_symbol_count: usize) -> usize {
    // A second refinement pass keeps level-9 output inside the size budget
    // while preserving the BWT/MTF speed advantage over reference encoders.
    2
}

pub fn decode_after_magic(reader: &mut BitReader<'_>, block_size_100k: u8) -> Result<DecodedBlock> {
    let expected_crc = reader.read_bits(32)?;
    let randomized = reader.read_bit()?;
    if randomized {
        return Err(Error::Unsupported("randomized bzip2 blocks"));
    }

    let primary_index = reader.read_bits(24)? as usize;
    let used_symbols = read_used_map(reader)?;
    if used_symbols.is_empty() {
        return Err(Error::Format("bzip2 block has empty alphabet"));
    }

    let alpha_size = used_symbols.len() + 2;
    let max_rle_len = max_block_len(block_size_100k);
    let mtf_symbols = huffman::decode_symbols(reader, alpha_size, max_rle_len)?;
    let bwt_bytes = mtf::decode(&mtf_symbols, &used_symbols, max_rle_len)?;
    let rle_bytes = bwt::inverse(&bwt_bytes, primary_index)?;
    let output = rle_decode(&rle_bytes)?;
    let actual_crc = crc::crc32(&output);

    if actual_crc != expected_crc {
        return Err(Error::Format("bzip2 block CRC mismatch"));
    }

    Ok(DecodedBlock {
        bytes: output,
        crc: actual_crc,
    })
}

pub fn max_block_len(block_size_100k: u8) -> usize {
    usize::from(block_size_100k) * 100_000
}

pub fn rle_encoded_len_for_run(run: usize) -> usize {
    let full_chunks = run / 259;
    let tail = run % 259;
    full_chunks * 5
        + match tail {
            0 => 0,
            1..=3 => tail,
            _ => 5,
        }
}

fn append_rle_run(byte: u8, run: usize, output: &mut Vec<u8>, used: &mut [bool; 256]) {
    if run <= 3 {
        output.resize(output.len() + run, byte);
        used[usize::from(byte)] = true;
    } else {
        let extra = (run - 4) as u8;
        output.extend_from_slice(&[byte, byte, byte, byte, extra]);
        used[usize::from(byte)] = true;
        used[usize::from(extra)] = true;
    }
}

fn rle_encode(input: &[u8]) -> RleEncoded {
    let mut output = Vec::with_capacity(input.len());
    let mut used = [false; 256];
    let mut index = 0usize;

    while index < input.len() {
        let byte = input[index];
        let mut run = 1usize;
        while index + run < input.len() && input[index + run] == byte {
            run += 1;
        }

        let mut remaining = run;
        while remaining > 0 {
            let chunk = remaining.min(259);
            append_rle_run(byte, chunk, &mut output, &mut used);
            remaining -= chunk;
        }

        index += run;
    }

    RleEncoded {
        bytes: output,
        used,
    }
}

fn rle_decode(input: &[u8]) -> Result<Vec<u8>> {
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0usize;

    while index < input.len() {
        let byte = input[index];
        index += 1;
        let mut run = 1usize;

        while run < 4 && index < input.len() && input[index] == byte {
            run += 1;
            index += 1;
        }

        output.resize(output.len() + run, byte);
        if run == 4 {
            let extra = *input
                .get(index)
                .ok_or(Error::Format("truncated bzip2 RLE run"))? as usize;
            index += 1;
            output.resize(output.len() + extra, byte);
        }
    }

    Ok(output)
}

fn write_used_map(used: &[bool; 256], writer: &mut BitWriter) {
    let mut group_used = [false; 16];
    for group in 0..16 {
        group_used[group] = used[group * 16..group * 16 + 16].iter().any(|&value| value);
        writer.write_bit(group_used[group]);
    }

    for group in 0..16 {
        if group_used[group] {
            for byte in group * 16..group * 16 + 16 {
                writer.write_bit(used[byte]);
            }
        }
    }
}

fn read_used_map(reader: &mut BitReader<'_>) -> Result<Vec<u8>> {
    let mut group_used = [false; 16];
    for item in &mut group_used {
        *item = reader.read_bit()?;
    }

    let mut used = [false; 256];
    for group in 0..16 {
        if group_used[group] {
            for byte in group * 16..group * 16 + 16 {
                used[byte] = reader.read_bit()?;
            }
        }
    }

    Ok(used
        .iter()
        .enumerate()
        .filter_map(|(byte, &is_used)| is_used.then_some(byte as u8))
        .collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn rle_round_trips() {
        let input = b"aaaabbbbccccccccccccccccccccccccccccccccxyz";
        let encoded = super::rle_encode(input);
        let decoded = super::rle_decode(&encoded.bytes).unwrap();
        assert_eq!(decoded, input);
    }
}
