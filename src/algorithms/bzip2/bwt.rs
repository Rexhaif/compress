use crate::error::{Error, Result};

pub struct BwtBlock {
    pub bytes: Vec<u8>,
    pub primary_index: usize,
}

pub fn transform(input: &[u8]) -> BwtBlock {
    match input.len() {
        0 => BwtBlock {
            bytes: Vec::new(),
            primary_index: 0,
        },
        1 => BwtBlock {
            bytes: vec![input[0]],
            primary_index: 0,
        },
        _ if input.iter().all(|&byte| byte == input[0]) => BwtBlock {
            bytes: input.to_vec(),
            primary_index: 0,
        },
        _ => transform_nontrivial(input),
    }
}

fn transform_nontrivial(input: &[u8]) -> BwtBlock {
    let order = if should_use_msd_sort(input) {
        cyclic_msd_order(input)
    } else {
        cyclic_suffix_order(input)
    };
    let n = input.len();
    let mut output = uninit_u8_vec(n);
    let mut primary_index = 0usize;

    for (rank, &position) in order.iter().enumerate() {
        let position = position as usize;
        if position == 0 {
            primary_index = rank;
        }
        // `order` contains positions in `0..n`, and `rank` is produced by
        // enumerating that same `n`-element order vector.
        unsafe {
            *output.get_unchecked_mut(rank) = if position == 0 {
                *input.get_unchecked(n - 1)
            } else {
                *input.get_unchecked(position - 1)
            };
        }
    }

    BwtBlock {
        bytes: output,
        primary_index,
    }
}

fn should_use_msd_sort(input: &[u8]) -> bool {
    if input.len() < 4096 {
        return false;
    }

    let mut used = [false; 256];
    let mut unique = 0usize;

    for &byte in input {
        let slot = &mut used[usize::from(byte)];
        if !*slot {
            *slot = true;
            unique += 1;
            if unique >= 8 {
                break;
            }
        }
    }

    if unique < 8 {
        return false;
    }

    let sample_len = input.len().min(8_192);
    let stride = (input.len() / sample_len).max(1);
    let mut contexts = FixedSet32::with_capacity(sample_len * 2);
    let can_short_circuit = input.len() >= sample_len + 3;
    let threshold = sample_len * 3;
    let mut sampled = 0usize;
    let mut index = 0usize;

    while sampled < sample_len && index + 3 < input.len() {
        let context = u32::from_be_bytes([
            input[index],
            input[index + 1],
            input[index + 2],
            input[index + 3],
        ]);
        contexts.insert(context);
        sampled += 1;
        if can_short_circuit {
            if contexts.len() * 4 >= threshold {
                return true;
            }
            if (contexts.len() + sample_len - sampled) * 4 < threshold {
                return false;
            }
        }
        index += stride;
    }

    contexts.len() * 4 >= sampled * 3
}

fn should_use_long_prefix_sort(input: &[u8]) -> bool {
    if input.len() < 65_536 {
        return false;
    }

    let sample_len = input.len().min(8_192);
    let stride = (input.len() / sample_len).max(1);
    let mut contexts = FixedSet128::with_capacity(sample_len * 2);
    let can_short_circuit = input.len() >= sample_len + 15;
    let threshold = sample_len * 3;
    let mut sampled = 0usize;
    let mut index = 0usize;

    while sampled < sample_len && index + 15 < input.len() {
        let context = u128::from_be_bytes([
            input[index],
            input[index + 1],
            input[index + 2],
            input[index + 3],
            input[index + 4],
            input[index + 5],
            input[index + 6],
            input[index + 7],
            input[index + 8],
            input[index + 9],
            input[index + 10],
            input[index + 11],
            input[index + 12],
            input[index + 13],
            input[index + 14],
            input[index + 15],
        ]);
        contexts.insert(context);
        sampled += 1;
        if can_short_circuit {
            if contexts.len() * 4 >= threshold {
                return true;
            }
            if (contexts.len() + sample_len - sampled) * 4 < threshold {
                return false;
            }
        }
        index += stride;
    }

    contexts.len() * 4 >= sampled * 3
}

fn uninit_u8_vec(len: usize) -> Vec<u8> {
    let mut values = Vec::with_capacity(len);
    // Every slot is written before the vector is read or returned.
    unsafe {
        values.set_len(len);
    }
    values
}

fn uninit_u32_vec(len: usize) -> Vec<u32> {
    let mut values = Vec::with_capacity(len);
    // These scratch vectors are filled completely before any read.
    unsafe {
        values.set_len(len);
    }
    values
}

struct FixedSet32 {
    keys: Vec<u32>,
    used: Vec<u8>,
    len: usize,
    mask: usize,
}

impl FixedSet32 {
    fn with_capacity(capacity: usize) -> FixedSet32 {
        let table_len = capacity.next_power_of_two().max(2);
        FixedSet32 {
            keys: vec![0; table_len],
            used: vec![0; table_len],
            len: 0,
            mask: table_len - 1,
        }
    }

    #[inline]
    fn insert(&mut self, key: u32) {
        let mut slot = key.wrapping_mul(0x9E37_79B1) as usize & self.mask;
        while self.used[slot] != 0 {
            if self.keys[slot] == key {
                return;
            }
            slot = (slot + 1) & self.mask;
        }

        self.used[slot] = 1;
        self.keys[slot] = key;
        self.len += 1;
    }

    #[inline]
    fn len(&self) -> usize {
        self.len
    }
}

struct FixedSet128 {
    keys: Vec<u128>,
    used: Vec<u8>,
    len: usize,
    mask: usize,
}

impl FixedSet128 {
    fn with_capacity(capacity: usize) -> FixedSet128 {
        let table_len = capacity.next_power_of_two().max(2);
        FixedSet128 {
            keys: vec![0; table_len],
            used: vec![0; table_len],
            len: 0,
            mask: table_len - 1,
        }
    }

    #[inline]
    fn insert(&mut self, key: u128) {
        let folded = (key as u64) ^ (key >> 64) as u64;
        let mut slot = folded.wrapping_mul(0x9E37_79B9_7F4A_7C15) as usize & self.mask;
        while self.used[slot] != 0 {
            if self.keys[slot] == key {
                return;
            }
            slot = (slot + 1) & self.mask;
        }

        self.used[slot] = 1;
        self.keys[slot] = key;
        self.len += 1;
    }

    #[inline]
    fn len(&self) -> usize {
        self.len
    }
}

fn cyclic_msd_order(input: &[u8]) -> Vec<u32> {
    let n = input.len();
    let mut order: Vec<u32> = (0..n as u32).collect();
    let mut scratch = uninit_u32_vec(n);
    let prefix_len = n.min(4);

    for depth in (0..prefix_len).rev() {
        let mut counts = [0usize; 256];
        for &position in &order {
            counts[usize::from(rotation_byte(input, position as usize, depth))] += 1;
        }

        let mut offsets = [0usize; 256];
        let mut cursor = 0usize;
        for byte in 0..256 {
            offsets[byte] = cursor;
            cursor += counts[byte];
        }

        let mut next = offsets;
        for &position in &order {
            let byte = usize::from(rotation_byte(input, position as usize, depth));
            scratch[next[byte]] = position;
            next[byte] += 1;
        }
        std::mem::swap(&mut order, &mut scratch);
    }

    let doubled = doubled_input(input);
    let mut start = 0usize;
    let mut previous_key = cyclic_prefix_u32(input, order[0] as usize);
    for index in 1..=n {
        let at_group_end = if index == n {
            true
        } else {
            let current_key = cyclic_prefix_u32(input, order[index] as usize);
            if current_key == previous_key {
                false
            } else {
                previous_key = current_key;
                true
            }
        };

        if !at_group_end {
            continue;
        }

        let len = index - start;
        if len > 1 {
            if len <= 32 {
                insertion_sort_rotations_linear(&mut order[start..index], &doubled, n, prefix_len);
            } else {
                order[start..index].sort_unstable_by(|&left, &right| {
                    compare_rotation_linear(&doubled, n, left as usize, right as usize, prefix_len)
                });
            }
        }
        start = index;
    }

    order
}

#[inline(always)]
fn rotation_byte(input: &[u8], position: usize, depth: usize) -> u8 {
    let index = position + depth;
    if index >= input.len() {
        input[index - input.len()]
    } else {
        input[index]
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn first_mismatch_sse2(left: *const u8, right: *const u8) -> u32 {
    use core::arch::x86_64::{__m128i, _mm_cmpeq_epi8, _mm_loadu_si128, _mm_movemask_epi8};

    let left = unsafe { _mm_loadu_si128(left.cast::<__m128i>()) };
    let right = unsafe { _mm_loadu_si128(right.cast::<__m128i>()) };
    let equal = _mm_movemask_epi8(_mm_cmpeq_epi8(left, right)) as u32;
    let different = !equal & 0xFFFF;
    if different == 0 {
        16
    } else {
        different.trailing_zeros()
    }
}

fn cyclic_suffix_order(input: &[u8]) -> Vec<u32> {
    let n = input.len();
    let mut counts = vec![0u32; 65_536];
    let mut order: Vec<u32> = (0..n as u32).collect();
    let mut next_order = uninit_u32_vec(n);
    let use_exact_finish = should_use_long_prefix_sort(input);
    let prefix_words = 4usize;
    let mut length = prefix_words * 2;

    if n >= 8 {
        let mut prefix_counts = vec![0u32; 65_536 * prefix_words];
        for position in 0..n {
            for word in 0..prefix_words {
                let word_value = cyclic_prefix_word(input, position, word);
                // `word` is in `0..prefix_words` and `word_value` is a 16-bit
                // prefix, so the histogram slot is in bounds.
                unsafe {
                    *prefix_counts.get_unchecked_mut(word * 65_536 + word_value) += 1;
                }
            }
        }

        for word in (0..prefix_words).rev() {
            let counts = &mut prefix_counts[word * 65_536..(word + 1) * 65_536];
            for index in 1..65_536 {
                counts[index] += counts[index - 1];
            }
            for &position in order.iter().rev() {
                let word_value = cyclic_prefix_word(input, position as usize, word);
                // The prefix word is 16-bit, and the decremented cumulative
                // count is a destination inside the `next_order` scratch block.
                unsafe {
                    let slot = counts.get_unchecked_mut(word_value);
                    *slot -= 1;
                    *next_order.get_unchecked_mut(*slot as usize) = position;
                }
            }
            std::mem::swap(&mut order, &mut next_order);
        }

        if use_exact_finish {
            exact_sort_prefix_groups(input, &mut order, length);
            return order;
        }

        let mut classes_by_pos = vec![0u32; n];
        let mut classes = 1usize;
        classes_by_pos[order[0] as usize] = 0;
        let mut previous_key = cyclic_prefix_u64(input, order[0] as usize);
        for index in 1..n {
            let current_key = cyclic_prefix_u64(input, order[index] as usize);
            if current_key != previous_key {
                classes += 1;
                previous_key = current_key;
            }
            classes_by_pos[order[index] as usize] = (classes - 1) as u32;
        }

        if counts.len() < n {
            counts.resize(n, 0);
        }

        return refine_cyclic_order(
            input,
            order,
            next_order,
            classes_by_pos,
            counts,
            classes,
            length,
        );
    }

    for position in 0..n {
        counts[cyclic_pair(input, position)] += 1;
    }
    for index in 1..65_536 {
        counts[index] += counts[index - 1];
    }
    order.fill(0);
    for position in (0..n).rev() {
        let pair = cyclic_pair(input, position);
        counts[pair] -= 1;
        order[counts[pair] as usize] = position as u32;
    }

    let mut classes = 1usize;
    let mut classes_by_pos = vec![0u32; n];
    classes_by_pos[order[0] as usize] = 0;
    let mut previous_pair = cyclic_pair(input, order[0] as usize);
    for index in 1..n {
        let current_pair = cyclic_pair(input, order[index] as usize);
        if current_pair != previous_pair {
            classes += 1;
            previous_pair = current_pair;
        }
        classes_by_pos[order[index] as usize] = (classes - 1) as u32;
    }

    length = 2;

    refine_cyclic_order(
        input,
        order,
        next_order,
        classes_by_pos,
        counts,
        classes,
        length,
    )
}

fn refine_cyclic_order(
    input: &[u8],
    mut order: Vec<u32>,
    mut next_order: Vec<u32>,
    mut classes_by_pos: Vec<u32>,
    mut counts: Vec<u32>,
    mut classes: usize,
    mut length: usize,
) -> Vec<u32> {
    let n = input.len();
    let mut next_classes = uninit_u32_vec(n);

    while length < n && classes < n {
        counts[..classes].fill(0);
        for &position in &order {
            let position = position as usize;
            let shifted_position = if position >= length {
                (position - length) as u32
            } else {
                (position + n - length) as u32
            };
            unsafe {
                let class = *classes_by_pos.get_unchecked(shifted_position as usize) as usize;
                *counts.get_unchecked_mut(class) += 1;
            }
        }
        for index in 1..classes {
            counts[index] += counts[index - 1];
        }
        for &position in order.iter().rev() {
            let position = position as usize;
            let shifted_position = if position >= length {
                (position - length) as u32
            } else {
                (position + n - length) as u32
            };
            unsafe {
                let class = *classes_by_pos.get_unchecked(shifted_position as usize) as usize;
                let count = counts.get_unchecked_mut(class);
                *count -= 1;
                *next_order.get_unchecked_mut(*count as usize) = shifted_position;
            }
        }
        std::mem::swap(&mut order, &mut next_order);

        unsafe {
            *next_classes.get_unchecked_mut(order[0] as usize) = 0;
        }
        let mut next_class_count = 1usize;
        for index in 1..n {
            let current_position = unsafe { *order.get_unchecked(index) as usize };
            let previous_position = unsafe { *order.get_unchecked(index - 1) as usize };
            let current = unsafe {
                (
                    *classes_by_pos.get_unchecked(current_position),
                    *classes_by_pos.get_unchecked(wrapping_add(current_position, length, n)),
                )
            };
            let previous = unsafe {
                (
                    *classes_by_pos.get_unchecked(previous_position),
                    *classes_by_pos.get_unchecked(wrapping_add(previous_position, length, n)),
                )
            };

            if current != previous {
                next_class_count += 1;
            }
            unsafe {
                *next_classes.get_unchecked_mut(current_position) = (next_class_count - 1) as u32;
            }
        }

        if next_class_count == classes {
            break;
        }

        std::mem::swap(&mut classes_by_pos, &mut next_classes);
        classes = next_class_count;
        length <<= 1;
    }

    order
}

fn doubled_input(input: &[u8]) -> Vec<u8> {
    let mut doubled = Vec::with_capacity(input.len() * 2);
    doubled.extend_from_slice(input);
    doubled.extend_from_slice(input);
    doubled
}

fn exact_sort_prefix_groups(input: &[u8], order: &mut [u32], depth: usize) {
    let doubled = doubled_input(input);
    exact_sort_prefix_groups_linear(&doubled, input.len(), order, depth);
}

fn exact_sort_prefix_groups_linear(doubled: &[u8], n: usize, order: &mut [u32], depth: usize) {
    let mut start = 0usize;
    let mut previous_key = cyclic_prefix_u64_linear(doubled, order[0] as usize);

    for index in 1..=order.len() {
        let at_group_end = if index == order.len() {
            true
        } else {
            let current_key = cyclic_prefix_u64_linear(doubled, order[index] as usize);
            if current_key == previous_key {
                false
            } else {
                previous_key = current_key;
                true
            }
        };

        if !at_group_end {
            continue;
        }

        let len = index - start;
        if len == 2 {
            if compare_rotation_linear(
                doubled,
                n,
                order[start] as usize,
                order[start + 1] as usize,
                depth,
            )
            .is_gt()
            {
                order.swap(start, start + 1);
            }
        } else if len <= 16 {
            insertion_sort_rotations_linear(&mut order[start..index], doubled, n, depth);
        } else if len > 2 {
            order[start..index].sort_unstable_by(|&left, &right| {
                compare_rotation_linear(doubled, n, left as usize, right as usize, depth)
            });
        }
        start = index;
    }
}

fn insertion_sort_rotations_linear(order: &mut [u32], doubled: &[u8], n: usize, depth: usize) {
    for index in 1..order.len() {
        let value = order[index];
        let mut slot = index;
        while slot > 0
            && compare_rotation_linear(doubled, n, value as usize, order[slot - 1] as usize, depth)
                .is_lt()
        {
            order[slot] = order[slot - 1];
            slot -= 1;
        }
        order[slot] = value;
    }
}

fn compare_rotation_linear(
    doubled: &[u8],
    n: usize,
    left: usize,
    right: usize,
    depth: usize,
) -> std::cmp::Ordering {
    let mut offset = depth;

    while offset < n {
        #[cfg(target_arch = "x86_64")]
        if offset + 16 <= n {
            let mismatch = unsafe {
                first_mismatch_sse2(
                    doubled.as_ptr().add(left + offset),
                    doubled.as_ptr().add(right + offset),
                )
            };
            if mismatch == 16 {
                offset += 16;
                continue;
            }

            let mismatch = mismatch as usize;
            return doubled[left + offset + mismatch].cmp(&doubled[right + offset + mismatch]);
        }

        if offset + 8 <= n {
            // Big-endian words preserve bytewise lexicographic order.
            let left_word = unsafe {
                std::ptr::read_unaligned(doubled.as_ptr().add(left + offset).cast::<u64>())
            };
            let right_word = unsafe {
                std::ptr::read_unaligned(doubled.as_ptr().add(right + offset).cast::<u64>())
            };
            let left_word = u64::from_be(left_word);
            let right_word = u64::from_be(right_word);
            match left_word.cmp(&right_word) {
                std::cmp::Ordering::Equal => {
                    offset += 8;
                    continue;
                }
                ordering => return ordering,
            }
        }

        let remaining = n - offset;
        let left_slice =
            unsafe { std::slice::from_raw_parts(doubled.as_ptr().add(left + offset), remaining) };
        let right_slice =
            unsafe { std::slice::from_raw_parts(doubled.as_ptr().add(right + offset), remaining) };

        match left_slice.cmp(right_slice) {
            std::cmp::Ordering::Equal => offset = n,
            ordering => return ordering,
        }
    }

    left.cmp(&right)
}

#[inline(always)]
fn cyclic_prefix_word(input: &[u8], position: usize, word: usize) -> usize {
    let offset = word * 2;
    let index = position + offset;
    if index + 1 < input.len() {
        // The direct path has two contiguous bytes available; an unaligned
        // load avoids two indexed byte loads in the radix-pass hot path.
        let word = unsafe { std::ptr::read_unaligned(input.as_ptr().add(index).cast::<u16>()) };
        usize::from(u16::from_be(word))
    } else {
        (usize::from(rotation_byte(input, position, offset)) << 8)
            | usize::from(rotation_byte(input, position, offset + 1))
    }
}

#[inline(always)]
fn cyclic_prefix_u32(input: &[u8], position: usize) -> u32 {
    if position + 4 <= input.len() {
        let word = unsafe { std::ptr::read_unaligned(input.as_ptr().add(position).cast::<u32>()) };
        return u32::from_be(word);
    }

    let mut value = 0u32;
    for offset in 0..4 {
        value = (value << 8) | u32::from(rotation_byte(input, position, offset));
    }
    value
}

#[inline(always)]
fn cyclic_prefix_u64(input: &[u8], position: usize) -> u64 {
    if position + 8 <= input.len() {
        // The slice has at least eight contiguous bytes in this branch; unaligned
        // reads avoid eight per-byte loads in the hot prefix-classification path.
        let word = unsafe { std::ptr::read_unaligned(input.as_ptr().add(position).cast::<u64>()) };
        return u64::from_be(word);
    }

    let mut value = 0u64;
    for offset in 0..8 {
        value = (value << 8) | u64::from(rotation_byte(input, position, offset));
    }
    value
}

#[inline(always)]
fn cyclic_prefix_u64_linear(doubled: &[u8], position: usize) -> u64 {
    let word = unsafe { std::ptr::read_unaligned(doubled.as_ptr().add(position).cast::<u64>()) };
    u64::from_be(word)
}

#[inline(always)]
fn cyclic_pair(input: &[u8], position: usize) -> usize {
    (usize::from(input[position]) << 8) | usize::from(input[wrapping_add(position, 1, input.len())])
}

#[inline(always)]
fn wrapping_add(position: usize, offset: usize, len: usize) -> usize {
    let next = position + offset;
    if next >= len { next - len } else { next }
}

pub fn inverse(last_column: &[u8], primary_index: usize) -> Result<Vec<u8>> {
    let n = last_column.len();
    if n == 0 {
        return Ok(Vec::new());
    }
    if primary_index >= n {
        return Err(Error::Format("bzip2 BWT primary index is out of range"));
    }
    if n > 0x00FF_FFFF {
        return Err(Error::Format("bzip2 BWT block is too large"));
    }

    let mut counts = [0usize; 256];
    for &byte in last_column {
        counts[usize::from(byte)] += 1;
    }

    let mut starts = [0usize; 256];
    let mut total = 0usize;
    for (byte, count) in counts.iter().enumerate() {
        starts[byte] = total;
        total += count;
    }

    let mut next = vec![0u32; n];
    for (index, &byte) in last_column.iter().enumerate() {
        let byte_index = usize::from(byte);
        let slot = starts[byte_index];
        starts[byte_index] += 1;
        // `slot` is formed from the cumulative byte histogram and occurrence
        // count, so every slot in `0..n` is written exactly once. Pack the
        // next LF-mapping index and byte together to avoid a second random
        // access to `last_column` in the inverse walk.
        unsafe {
            *next.get_unchecked_mut(slot) = ((index as u32) << 8) | u32::from(byte);
        }
    }

    let mut output = uninit_u8_vec(n);
    let mut position = primary_index;
    for slot in 0..n {
        // `next` contains only indices produced from `last_column`, and the
        // primary index was validated above.
        unsafe {
            let entry = *next.get_unchecked(position);
            position = (entry >> 8) as usize;
            *output.get_unchecked_mut(slot) = entry as u8;
        }
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    fn round_trip(input: &[u8]) {
        let transformed = super::transform(input);
        let restored = super::inverse(&transformed.bytes, transformed.primary_index).unwrap();
        assert_eq!(restored, input);
    }

    #[test]
    fn bwt_round_trips_small_inputs() {
        round_trip(b"");
        round_trip(b"a");
        round_trip(b"banana");
        round_trip(b"abracadabra");
        round_trip(b"aaaaaaaaaaaa");
        round_trip(b"abcabcabcabc");
    }
}
