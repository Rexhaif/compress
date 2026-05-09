use std::sync::OnceLock;

pub fn crc32(data: &[u8]) -> u32 {
    let tables = table();
    let mut crc = u32::MAX;

    let mut index = 0usize;
    while index + 8 <= data.len() {
        // The loop guard guarantees eight contiguous bytes are available.
        let word = unsafe { std::ptr::read_unaligned(data.as_ptr().add(index).cast::<u64>()) };
        let word = u64::from_be(word);
        let high = (word >> 32) as u32;
        let low = word as u32;
        crc ^= high;
        crc = tables[7][((crc >> 24) & 0xFF) as usize]
            ^ tables[6][((crc >> 16) & 0xFF) as usize]
            ^ tables[5][((crc >> 8) & 0xFF) as usize]
            ^ tables[4][(crc & 0xFF) as usize]
            ^ tables[3][((low >> 24) & 0xFF) as usize]
            ^ tables[2][((low >> 16) & 0xFF) as usize]
            ^ tables[1][((low >> 8) & 0xFF) as usize]
            ^ tables[0][(low & 0xFF) as usize];
        index += 8;
    }

    for &byte in &data[index..] {
        let table_index = ((crc >> 24) as u8 ^ byte) as usize;
        crc = (crc << 8) ^ tables[0][table_index];
    }

    !crc
}

pub fn update_combined_crc(combined: u32, block_crc: u32) -> u32 {
    combined.rotate_left(1) ^ block_crc
}

fn table() -> &'static [[u32; 256]; 8] {
    static TABLE: OnceLock<[[u32; 256]; 8]> = OnceLock::new();
    TABLE.get_or_init(build_table)
}

fn build_table() -> [[u32; 256]; 8] {
    let mut table = [[0u32; 256]; 8];

    for (index, slot) in table[0].iter_mut().enumerate() {
        let mut crc = (index as u32) << 24;
        for _ in 0..8 {
            if crc & 0x8000_0000 != 0 {
                crc = (crc << 1) ^ 0x04C1_1DB7;
            } else {
                crc <<= 1;
            }
        }
        *slot = crc;
    }

    for slice in 1..table.len() {
        for index in 0..256 {
            let crc = table[slice - 1][index];
            table[slice][index] = (crc << 8) ^ table[0][((crc >> 24) & 0xFF) as usize];
        }
    }

    table
}

#[cfg(test)]
mod tests {
    #[test]
    fn crc32_bzip2_known_vector() {
        assert_eq!(super::crc32(b"123456789"), 0xFC89_1918);
    }
}
