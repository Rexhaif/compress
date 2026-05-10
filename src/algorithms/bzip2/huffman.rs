use crate::algorithms::bzip2::bitstream::{BitReader, BitWriter};
use crate::error::{Error, Result};

const GROUP_SIZE: usize = 50;
pub const DEFAULT_REFINEMENT_PASSES: usize = 0;
const FAST_DECODE_BITS: usize = 10;
const FAST_DECODE_EMPTY: u32 = u32::MAX;
const MAX_GROUPS: usize = 6;
const MAX_CODE_LEN: usize = 20;

#[derive(Clone)]
struct HuffmanCodes {
    lengths: Vec<u8>,
    codes: Vec<u32>,
}

struct DecodeTable {
    fast: Vec<u32>,
    min_len: usize,
    max_len: usize,
    limit: [i32; MAX_CODE_LEN + 2],
    base: [i32; MAX_CODE_LEN + 2],
    perm: Vec<u16>,
}

#[cfg(test)]
pub fn encode_symbols(symbols: &[u16], alpha_size: usize, writer: &mut BitWriter) -> Result<()> {
    encode_symbols_with_passes(symbols, alpha_size, writer, DEFAULT_REFINEMENT_PASSES)
}

pub fn encode_symbols_with_passes(
    symbols: &[u16],
    alpha_size: usize,
    writer: &mut BitWriter,
    refinement_passes: usize,
) -> Result<()> {
    if !(2..=258).contains(&alpha_size) {
        return Err(Error::Format("bzip2 Huffman alphabet size is invalid"));
    }

    let group_count = group_count_for_symbols(symbols.len());
    let mut lengths = initial_code_lengths(symbols, alpha_size, group_count);
    let selector_count = symbols.len().div_ceil(GROUP_SIZE);
    let mut selectors = Vec::with_capacity(selector_count);

    for _ in 0..refinement_passes {
        let mut table_frequencies = [[0u32; 258]; MAX_GROUPS];
        selectors.clear();

        for chunk in symbols.chunks(GROUP_SIZE) {
            let selector = best_table(chunk, &lengths);
            selectors.push(selector as u8);
            let table_frequency = &mut table_frequencies[selector];
            for &symbol in chunk {
                // Symbols are produced below `alpha_size`, and every frequency
                // table has exactly `alpha_size` entries.
                unsafe {
                    *table_frequency.get_unchecked_mut(usize::from(symbol)) += 1;
                }
            }
        }

        for table in 0..group_count {
            lengths[table] = make_code_lengths(&table_frequencies[table][..alpha_size], alpha_size);
        }
    }

    selectors.clear();
    selectors.extend(
        symbols
            .chunks(GROUP_SIZE)
            .map(|chunk| best_table(chunk, &lengths) as u8),
    );

    if selectors.len() > 0x7FFF {
        return Err(Error::Format("bzip2 selector count is too large"));
    }

    let codes: Vec<HuffmanCodes> = lengths
        .iter()
        .map(|lengths| HuffmanCodes {
            lengths: lengths.clone(),
            codes: assign_codes(lengths),
        })
        .collect();

    writer.write_bits(group_count as u64, 3);
    writer.write_bits(selectors.len() as u64, 15);
    write_selectors(&selectors, group_count, writer)?;
    write_code_lengths(&lengths, writer);

    for (chunk_index, chunk) in symbols.chunks(GROUP_SIZE).enumerate() {
        let table = usize::from(selectors[chunk_index]);
        let table_codes = &codes[table].codes;
        let table_lengths = &codes[table].lengths;
        for &symbol in chunk {
            let symbol_index = usize::from(symbol);
            // Symbols are validated by construction to be within the table
            // alphabet, and every generated code table has `alpha_size` slots.
            let (code, length) = unsafe {
                (
                    *table_codes.get_unchecked(symbol_index),
                    *table_lengths.get_unchecked(symbol_index),
                )
            };
            writer.write_bits_unmasked(u64::from(code), length);
        }
    }

    Ok(())
}

fn group_count_for_symbols(symbol_count: usize) -> usize {
    match symbol_count {
        0..=199 => 2,
        200..=599 => 3,
        600..=1199 => 4,
        1200..=2399 => 5,
        _ => 6,
    }
}

fn initial_code_lengths(symbols: &[u16], alpha_size: usize, group_count: usize) -> Vec<Vec<u8>> {
    let mut global = vec![0u32; alpha_size];
    for &symbol in symbols {
        global[usize::from(symbol)] += 1;
    }

    let total: u32 = global.iter().sum();
    if total == 0 {
        return vec![make_code_lengths(&global, alpha_size); group_count];
    }

    let mut lengths = Vec::with_capacity(group_count);
    let mut start = 0usize;
    let mut remaining = total;

    for group in 0..group_count {
        let groups_left = group_count - group;
        let target = remaining.div_ceil(groups_left as u32);
        let mut end = start;
        let mut accumulated = global[end];

        while end + 1 < alpha_size && accumulated + global[end + 1] <= target {
            end += 1;
            accumulated += global[end];
        }
        if end < start {
            end = start;
        }

        let mut local = vec![0u32; alpha_size];
        local[start..=end].copy_from_slice(&global[start..=end]);
        lengths.push(make_code_lengths(&local, alpha_size));

        remaining = remaining.saturating_sub(accumulated);
        start = (end + 1).min(alpha_size - 1);
    }

    lengths
}

fn best_table(symbols: &[u16], lengths: &[Vec<u8>]) -> usize {
    if lengths.len() == 6 {
        return best_table_six(symbols, lengths);
    }

    let mut best_table = 0usize;
    let mut best_cost = u32::MAX;

    for (table, lengths) in lengths.iter().enumerate() {
        let cost = symbols
            .iter()
            .map(|&symbol| u32::from(lengths[usize::from(symbol)]))
            .sum();
        if cost < best_cost {
            best_cost = cost;
            best_table = table;
        }
    }

    best_table
}

fn best_table_six(symbols: &[u16], lengths: &[Vec<u8>]) -> usize {
    let l0 = &lengths[0];
    let l1 = &lengths[1];
    let l2 = &lengths[2];
    let l3 = &lengths[3];
    let l4 = &lengths[4];
    let l5 = &lengths[5];
    let mut c0 = 0u32;
    let mut c1 = 0u32;
    let mut c2 = 0u32;
    let mut c3 = 0u32;
    let mut c4 = 0u32;
    let mut c5 = 0u32;

    let mut index = 0usize;
    while index + 4 <= symbols.len() {
        let s0 = usize::from(symbols[index]);
        let s1 = usize::from(symbols[index + 1]);
        let s2 = usize::from(symbols[index + 2]);
        let s3 = usize::from(symbols[index + 3]);

        // The MTF/Huffman pipeline only emits symbols below `alpha_size`, and
        // every length table is built to that same size.
        unsafe {
            c0 += u32::from(*l0.get_unchecked(s0))
                + u32::from(*l0.get_unchecked(s1))
                + u32::from(*l0.get_unchecked(s2))
                + u32::from(*l0.get_unchecked(s3));
            c1 += u32::from(*l1.get_unchecked(s0))
                + u32::from(*l1.get_unchecked(s1))
                + u32::from(*l1.get_unchecked(s2))
                + u32::from(*l1.get_unchecked(s3));
            c2 += u32::from(*l2.get_unchecked(s0))
                + u32::from(*l2.get_unchecked(s1))
                + u32::from(*l2.get_unchecked(s2))
                + u32::from(*l2.get_unchecked(s3));
            c3 += u32::from(*l3.get_unchecked(s0))
                + u32::from(*l3.get_unchecked(s1))
                + u32::from(*l3.get_unchecked(s2))
                + u32::from(*l3.get_unchecked(s3));
            c4 += u32::from(*l4.get_unchecked(s0))
                + u32::from(*l4.get_unchecked(s1))
                + u32::from(*l4.get_unchecked(s2))
                + u32::from(*l4.get_unchecked(s3));
            c5 += u32::from(*l5.get_unchecked(s0))
                + u32::from(*l5.get_unchecked(s1))
                + u32::from(*l5.get_unchecked(s2))
                + u32::from(*l5.get_unchecked(s3));
        }
        index += 4;
    }

    while index < symbols.len() {
        let symbol = usize::from(symbols[index]);
        unsafe {
            c0 += u32::from(*l0.get_unchecked(symbol));
            c1 += u32::from(*l1.get_unchecked(symbol));
            c2 += u32::from(*l2.get_unchecked(symbol));
            c3 += u32::from(*l3.get_unchecked(symbol));
            c4 += u32::from(*l4.get_unchecked(symbol));
            c5 += u32::from(*l5.get_unchecked(symbol));
        }
        index += 1;
    }

    let mut best_table = 0usize;
    let mut best_cost = c0;
    if c1 < best_cost {
        best_cost = c1;
        best_table = 1;
    }
    if c2 < best_cost {
        best_cost = c2;
        best_table = 2;
    }
    if c3 < best_cost {
        best_cost = c3;
        best_table = 3;
    }
    if c4 < best_cost {
        best_cost = c4;
        best_table = 4;
    }
    if c5 < best_cost {
        best_table = 5;
    }

    best_table
}

fn make_code_lengths(frequencies: &[u32], alpha_size: usize) -> Vec<u8> {
    let mut weights: Vec<u32> = frequencies
        .iter()
        .take(alpha_size)
        .map(|&frequency| frequency.max(1))
        .collect();

    loop {
        let lengths = build_lengths_from_weights(&weights);
        if lengths
            .iter()
            .all(|&length| usize::from(length) <= MAX_CODE_LEN)
        {
            return lengths;
        }

        for weight in &mut weights {
            *weight = 1 + (*weight / 2);
        }
    }
}

fn build_lengths_from_weights(weights: &[u32]) -> Vec<u8> {
    let symbol_count = weights.len();
    if symbol_count == 1 {
        return vec![1];
    }

    let mut parents = Vec::with_capacity(symbol_count * 2 - 1);
    parents.resize(symbol_count, usize::MAX);
    let mut node_weights = Vec::with_capacity(symbol_count * 2 - 1);
    node_weights.extend_from_slice(weights);
    let mut leaves: Vec<usize> = (0..symbol_count).collect();
    leaves.sort_unstable_by_key(|&node| (node_weights[node], node));

    let mut leaf_index = 0usize;
    let mut internal_index = symbol_count;

    for _ in 1..symbol_count {
        let left = pop_lowest_node(&leaves, &node_weights, &mut leaf_index, &mut internal_index);
        let right = pop_lowest_node(&leaves, &node_weights, &mut leaf_index, &mut internal_index);
        let parent = node_weights.len();
        node_weights.push(node_weights[left].saturating_add(node_weights[right]));
        parents.push(usize::MAX);
        parents[left] = parent;
        parents[right] = parent;
    }

    let mut lengths = vec![0u8; symbol_count];
    for symbol in 0..symbol_count {
        let mut length = 0u8;
        let mut node = symbol;
        while parents[node] != usize::MAX {
            let parent = parents[node];
            length += 1;
            node = parent;
        }
        lengths[symbol] = length.max(1);
    }

    lengths
}

fn pop_lowest_node(
    leaves: &[usize],
    node_weights: &[u32],
    leaf_index: &mut usize,
    internal_index: &mut usize,
) -> usize {
    if *leaf_index >= leaves.len() {
        let node = *internal_index;
        *internal_index += 1;
        return node;
    }

    if *internal_index >= node_weights.len() {
        let node = leaves[*leaf_index];
        *leaf_index += 1;
        return node;
    }

    let leaf = leaves[*leaf_index];
    let internal = *internal_index;
    if (node_weights[leaf], leaf) <= (node_weights[internal], internal) {
        *leaf_index += 1;
        leaf
    } else {
        *internal_index += 1;
        internal
    }
}

fn assign_codes(lengths: &[u8]) -> Vec<u32> {
    let min_len = usize::from(*lengths.iter().min().unwrap_or(&1));
    let max_len = usize::from(*lengths.iter().max().unwrap_or(&1));
    let mut codes = vec![0u32; lengths.len()];
    let mut code = 0u32;

    for length in min_len..=max_len {
        for (symbol, &symbol_length) in lengths.iter().enumerate() {
            if usize::from(symbol_length) == length {
                codes[symbol] = code;
                code += 1;
            }
        }
        code <<= 1;
    }

    codes
}

fn write_selectors(selectors: &[u8], group_count: usize, writer: &mut BitWriter) -> Result<()> {
    let mut mtf: Vec<u8> = (0..group_count as u8).collect();

    for &selector in selectors {
        let index = mtf
            .iter()
            .position(|&value| value == selector)
            .ok_or(Error::Format("bzip2 selector is out of range"))?;
        for _ in 0..index {
            writer.write_bit(true);
        }
        writer.write_bit(false);

        let value = mtf.remove(index);
        mtf.insert(0, value);
    }

    Ok(())
}

fn write_code_lengths(lengths: &[Vec<u8>], writer: &mut BitWriter) {
    for table_lengths in lengths {
        let mut current = table_lengths[0];
        writer.write_bits(u64::from(current), 5);

        for &target in table_lengths {
            while current < target {
                writer.write_bits(0b10, 2);
                current += 1;
            }
            while current > target {
                writer.write_bits(0b11, 2);
                current -= 1;
            }
            writer.write_bit(false);
        }
    }
}

pub fn decode_symbols(
    reader: &mut BitReader<'_>,
    alpha_size: usize,
    output_limit: usize,
) -> Result<Vec<u16>> {
    let group_count = reader.read_bits(3)? as usize;
    if !(2..=MAX_GROUPS).contains(&group_count) {
        return Err(Error::Format("bzip2 Huffman group count is invalid"));
    }

    let selector_count = reader.read_bits(15)? as usize;
    if selector_count == 0 {
        return Err(Error::Format("bzip2 selector count is zero"));
    }

    let selectors = read_selectors(reader, group_count, selector_count)?;
    let lengths = read_code_lengths(reader, group_count, alpha_size)?;
    let tables: Vec<DecodeTable> = lengths
        .iter()
        .map(|lengths| build_decode_table(lengths))
        .collect::<Result<_>>()?;

    let eob = (alpha_size - 1) as u16;
    let symbol_capacity = selector_count
        .saturating_mul(GROUP_SIZE)
        .min(output_limit.saturating_mul(2).saturating_add(1024));
    let mut symbols = Vec::with_capacity(symbol_capacity);
    let mut group_pos = 0usize;
    let mut selector_index = 0usize;
    let mut table = &tables[usize::from(selectors[0])];

    loop {
        if group_pos == GROUP_SIZE {
            group_pos = 0;
            selector_index += 1;
            if selector_index >= selectors.len() {
                return Err(Error::Format("bzip2 selector stream ended before EOB"));
            }
            table = &tables[usize::from(selectors[selector_index])];
        }

        let symbol = decode_one(reader, table)?;
        group_pos += 1;

        if symbol == eob {
            break;
        }

        symbols.push(symbol);
        if symbols.len() > output_limit * 2 + 1024 {
            return Err(Error::Format("bzip2 Huffman output is too large"));
        }
    }

    Ok(symbols)
}

fn read_selectors(
    reader: &mut BitReader<'_>,
    group_count: usize,
    selector_count: usize,
) -> Result<Vec<u8>> {
    let mut mtf = [0u8; MAX_GROUPS];
    for (index, value) in mtf.iter_mut().take(group_count).enumerate() {
        *value = index as u8;
    }
    let mut selectors = Vec::with_capacity(selector_count);

    for _ in 0..selector_count {
        let mut index = 0usize;
        while reader.read_bit()? {
            index += 1;
            if index >= group_count {
                return Err(Error::Format("bzip2 selector MTF index is out of range"));
            }
        }

        let value = mtf[index];
        for slot in (1..=index).rev() {
            mtf[slot] = mtf[slot - 1];
        }
        mtf[0] = value;
        selectors.push(value);
    }

    Ok(selectors)
}

fn read_code_lengths(
    reader: &mut BitReader<'_>,
    group_count: usize,
    alpha_size: usize,
) -> Result<Vec<Vec<u8>>> {
    let mut tables = Vec::with_capacity(group_count);

    for _ in 0..group_count {
        let mut current = reader.read_bits(5)? as i32;
        let mut lengths = Vec::with_capacity(alpha_size);

        for _ in 0..alpha_size {
            while reader.read_bit()? {
                if reader.read_bit()? {
                    current -= 1;
                } else {
                    current += 1;
                }

                if !(1..=MAX_CODE_LEN as i32).contains(&current) {
                    return Err(Error::Format("bzip2 Huffman code length is invalid"));
                }
            }
            lengths.push(current as u8);
        }

        tables.push(lengths);
    }

    Ok(tables)
}

fn build_decode_table(lengths: &[u8]) -> Result<DecodeTable> {
    let min_len = usize::from(*lengths.iter().min().unwrap_or(&1));
    let max_len = usize::from(*lengths.iter().max().unwrap_or(&1));
    if max_len > MAX_CODE_LEN || min_len == 0 {
        return Err(Error::Format("bzip2 Huffman table has invalid lengths"));
    }

    let mut base = [0i32; MAX_CODE_LEN + 2];
    let mut limit = [-1i32; MAX_CODE_LEN + 2];
    let mut perm = Vec::with_capacity(lengths.len());

    for &length in lengths {
        base[usize::from(length) + 1] += 1;
    }
    for index in 1..base.len() {
        base[index] += base[index - 1];
    }

    for length in min_len..=max_len {
        for (symbol, &symbol_length) in lengths.iter().enumerate() {
            if usize::from(symbol_length) == length {
                perm.push(symbol as u16);
            }
        }
    }

    let mut value = 0i32;
    for length in min_len..=max_len {
        value += base[length + 1] - base[length];
        limit[length] = value - 1;
        value <<= 1;
    }
    for length in (min_len + 1)..=max_len {
        base[length] = ((limit[length - 1] + 1) << 1) - base[length];
    }

    let fast = build_fast_decode_table(lengths);

    Ok(DecodeTable {
        fast,
        min_len,
        max_len,
        limit,
        base,
        perm,
    })
}

fn build_fast_decode_table(lengths: &[u8]) -> Vec<u32> {
    let codes = assign_codes(lengths);
    let mut fast = vec![FAST_DECODE_EMPTY; 1 << FAST_DECODE_BITS];

    for (symbol, &length) in lengths.iter().enumerate() {
        let length = usize::from(length);
        if length == 0 || length > FAST_DECODE_BITS {
            continue;
        }

        let code = codes[symbol] as usize;
        let shift = FAST_DECODE_BITS - length;
        let start = code << shift;
        let end = start + (1usize << shift);
        let entry = ((length as u32) << 16) | symbol as u32;
        for slot in start..end {
            fast[slot] = entry;
        }
    }

    fast
}

fn decode_one(reader: &mut BitReader<'_>, table: &DecodeTable) -> Result<u16> {
    if reader.remaining_bits() >= FAST_DECODE_BITS {
        let bits = reader.peek_bits(FAST_DECODE_BITS as u8)? as usize;
        let entry = table.fast[bits];
        if entry != FAST_DECODE_EMPTY {
            reader.skip_bits((entry >> 16) as u8)?;
            return Ok(entry as u16);
        }
    }

    let mut length = table.min_len;
    let mut code = reader.read_bits(length as u8)? as i32;

    while length <= table.max_len && code > table.limit[length] {
        length += 1;
        code = (code << 1) | i32::from(reader.read_bit()?);
    }

    if length > table.max_len {
        return Err(Error::Format("bzip2 Huffman code is invalid"));
    }

    let index = code - table.base[length];
    if index < 0 || index as usize >= table.perm.len() {
        return Err(Error::Format("bzip2 Huffman symbol is out of range"));
    }

    Ok(table.perm[index as usize])
}

#[cfg(test)]
mod tests {
    use crate::algorithms::bzip2::bitstream::{BitReader, BitWriter};

    #[test]
    fn huffman_round_trips_symbol_stream() {
        let symbols = [0, 0, 1, 2, 3, 3, 3, 4, 5, 1, 0, 6];
        let mut with_eob = symbols.to_vec();
        with_eob.push(7);

        let mut writer = BitWriter::with_capacity(0);
        super::encode_symbols(&with_eob, 8, &mut writer).unwrap();
        let bytes = writer.finish();
        let mut reader = BitReader::new(&bytes);
        let decoded = super::decode_symbols(&mut reader, 8, 100).unwrap();

        assert_eq!(decoded, symbols);
    }
}
