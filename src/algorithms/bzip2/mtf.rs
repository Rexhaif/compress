use crate::error::{Error, Result};

pub struct EncodedMtf {
    pub symbols: Vec<u16>,
    pub used: [bool; 256],
    pub used_symbols: Vec<u8>,
}

#[cfg(test)]
pub fn encode(input: &[u8]) -> EncodedMtf {
    let mut used = [false; 256];
    for &byte in input {
        used[usize::from(byte)] = true;
    }

    encode_with_used(input, used)
}

pub fn encode_with_used(input: &[u8], used: [bool; 256]) -> EncodedMtf {
    let used_symbols: Vec<u8> = used
        .iter()
        .enumerate()
        .filter_map(|(byte, &is_used)| is_used.then_some(byte as u8))
        .collect();

    if used_symbols.len() > 240 {
        return encode_with_scan(input, used, used_symbols);
    }

    let mut mtf = [0u8; 256];
    mtf[..used_symbols.len()].copy_from_slice(&used_symbols);
    let mut positions = [0u8; 256];
    for (index, &byte) in used_symbols.iter().enumerate() {
        positions[usize::from(byte)] = index as u8;
    }
    let mut symbols = Vec::with_capacity(input.len() + 1);
    let mut zero_run = 0usize;

    for &byte in input {
        if byte == mtf[0] {
            zero_run += 1;
            continue;
        }

        let index = usize::from(positions[usize::from(byte)]);

        flush_zero_run(zero_run, &mut symbols);
        zero_run = 0;
        symbols.push(index as u16 + 1);
        move_to_front(&mut mtf, &mut positions, byte, index);
    }

    flush_zero_run(zero_run, &mut symbols);
    symbols.push(used_symbols.len() as u16 + 1);

    EncodedMtf {
        symbols,
        used,
        used_symbols,
    }
}

#[inline(always)]
fn move_to_front(mtf: &mut [u8; 256], positions: &mut [u8; 256], byte: u8, index: usize) {
    match index {
        1 => {
            let moved = mtf[0];
            mtf[1] = moved;
            positions[usize::from(moved)] = 1;
        }
        2 => {
            let moved = mtf[1];
            mtf[2] = moved;
            positions[usize::from(moved)] = 2;
            let moved = mtf[0];
            mtf[1] = moved;
            positions[usize::from(moved)] = 1;
        }
        3 => {
            let moved = mtf[2];
            mtf[3] = moved;
            positions[usize::from(moved)] = 3;
            let moved = mtf[1];
            mtf[2] = moved;
            positions[usize::from(moved)] = 2;
            let moved = mtf[0];
            mtf[1] = moved;
            positions[usize::from(moved)] = 1;
        }
        4 => {
            let moved = mtf[3];
            mtf[4] = moved;
            positions[usize::from(moved)] = 4;
            let moved = mtf[2];
            mtf[3] = moved;
            positions[usize::from(moved)] = 3;
            let moved = mtf[1];
            mtf[2] = moved;
            positions[usize::from(moved)] = 2;
            let moved = mtf[0];
            mtf[1] = moved;
            positions[usize::from(moved)] = 1;
        }
        _ => {
            for slot in (1..=index).rev() {
                let moved = mtf[slot - 1];
                mtf[slot] = moved;
                positions[usize::from(moved)] = slot as u8;
            }
        }
    }

    mtf[0] = byte;
    positions[usize::from(byte)] = 0;
}

fn encode_with_scan(input: &[u8], used: [bool; 256], used_symbols: Vec<u8>) -> EncodedMtf {
    let mut mtf = [0u8; 256];
    mtf[..used_symbols.len()].copy_from_slice(&used_symbols);
    let mut symbols = Vec::with_capacity(input.len() + 1);
    let mut zero_run = 0usize;

    for &byte in input {
        if byte == mtf[0] {
            zero_run += 1;
            continue;
        }

        let index = find_mtf_index(&mtf, byte);

        flush_zero_run(zero_run, &mut symbols);
        zero_run = 0;
        symbols.push(index as u16 + 1);
        mtf.copy_within(0..index, 1);
        mtf[0] = byte;
    }

    flush_zero_run(zero_run, &mut symbols);
    symbols.push(used_symbols.len() as u16 + 1);

    EncodedMtf {
        symbols,
        used,
        used_symbols,
    }
}

#[inline(always)]
fn find_mtf_index(mtf: &[u8; 256], byte: u8) -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        // x86_64 guarantees SSE2. Compare 16 MTF entries at a time and use the
        // equality mask to locate the first match.
        return unsafe { find_mtf_index_sse2(mtf, byte) };
    }

    #[cfg(not(target_arch = "x86_64"))]
    {
        find_mtf_index_word(mtf, byte)
    }
}

#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
fn find_mtf_index_word(mtf: &[u8; 256], byte: u8) -> usize {
    const ONES: u64 = 0x0101_0101_0101_0101;
    const HIGHS: u64 = 0x8080_8080_8080_8080;

    let needle = u64::from(byte) * ONES;
    let mut offset = 0usize;
    while offset < mtf.len() {
        // The MTF table is a fixed 256-byte array, so each 8-byte unaligned read
        // is in bounds and avoids eight scalar compare/branch steps.
        let word = unsafe { std::ptr::read_unaligned(mtf.as_ptr().add(offset).cast::<u64>()) };
        let diff = u64::from_le(word) ^ needle;
        let matches = diff.wrapping_sub(ONES) & !diff & HIGHS;
        if matches != 0 {
            return offset + (matches.trailing_zeros() as usize / 8);
        }
        offset += 8;
    }

    unreachable!("MTF table contains every symbol in the block alphabet")
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn find_mtf_index_sse2(mtf: &[u8; 256], byte: u8) -> usize {
    use core::arch::x86_64::{
        __m128i, _mm_cmpeq_epi8, _mm_loadu_si128, _mm_movemask_epi8, _mm_set1_epi8,
    };

    let needle = _mm_set1_epi8(byte as i8);
    let mut offset = 0usize;
    while offset < mtf.len() {
        let chunk = unsafe { _mm_loadu_si128(mtf.as_ptr().add(offset).cast::<__m128i>()) };
        let matches = _mm_movemask_epi8(_mm_cmpeq_epi8(chunk, needle)) as u32;
        if matches != 0 {
            return offset + matches.trailing_zeros() as usize;
        }
        offset += 16;
    }

    unreachable!("MTF table contains every symbol in the block alphabet")
}

fn flush_zero_run(mut run: usize, symbols: &mut Vec<u16>) {
    if run == 0 {
        return;
    }

    run -= 1;
    loop {
        symbols.push((run & 1) as u16);
        if run < 2 {
            break;
        }
        run = (run - 2) / 2;
    }
}

pub fn decode(symbols: &[u16], used_symbols: &[u8], output_limit: usize) -> Result<Vec<u8>> {
    let mut mtf = [0u8; 256];
    mtf[..used_symbols.len()].copy_from_slice(used_symbols);
    let mtf_len = used_symbols.len();
    let mut output = Vec::new();
    let mut index = 0usize;

    while index < symbols.len() {
        let symbol = symbols[index];
        if symbol <= 1 {
            let mut run = 0usize;
            let mut weight = 1usize;

            while index < symbols.len() {
                match symbols[index] {
                    0 => run += weight,
                    1 => run += weight << 1,
                    _ => break,
                }
                weight <<= 1;
                index += 1;
            }

            if output.len() + run > output_limit {
                return Err(Error::Format("bzip2 MTF output exceeds block size"));
            }

            if mtf_len == 0 {
                return Err(Error::Format("bzip2 MTF run with empty alphabet"));
            }
            let byte = mtf[0];
            output.resize(output.len() + run, byte);
            continue;
        }

        let mtf_index = usize::from(symbol - 1);
        if mtf_index >= mtf_len {
            return Err(Error::Format("bzip2 MTF index is out of range"));
        }

        let byte = mtf[mtf_index];
        mtf.copy_within(0..mtf_index, 1);
        mtf[0] = byte;
        output.push(byte);
        if output.len() > output_limit {
            return Err(Error::Format("bzip2 MTF output exceeds block size"));
        }

        index += 1;
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    #[test]
    fn mtf_round_trips_repetitive_input() {
        let input = b"banana banana banana";
        let encoded = super::encode(input);
        let eob = encoded.used_symbols.len() as u16 + 1;
        let body = &encoded.symbols[..encoded.symbols.len() - 1];
        assert_eq!(*encoded.symbols.last().unwrap(), eob);
        let decoded = super::decode(body, &encoded.used_symbols, input.len()).unwrap();
        assert_eq!(decoded, input);
    }
}
