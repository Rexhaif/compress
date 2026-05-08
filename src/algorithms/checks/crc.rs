pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;

    for &byte in data {
        let mut value = (crc ^ u32::from(byte)) & 0xFF;

        for _ in 0..8 {
            if value & 1 == 1 {
                value = (value >> 1) ^ 0xEDB8_8320;
            } else {
                value >>= 1;
            }
        }

        crc = (crc >> 8) ^ value;
    }

    !crc
}

pub fn crc64(data: &[u8]) -> u64 {
    let mut crc = u64::MAX;

    for &byte in data {
        let mut value = (crc ^ u64::from(byte)) & 0xFF;

        for _ in 0..8 {
            if value & 1 == 1 {
                value = (value >> 1) ^ 0xC96C_5795_D787_0F42;
            } else {
                value >>= 1;
            }
        }

        crc = (crc >> 8) ^ value;
    }

    !crc
}

#[cfg(test)]
mod tests {
    use super::{crc32, crc64};

    #[test]
    fn crc32_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn crc64_known_vector() {
        assert_eq!(crc64(b"123456789"), 0x995D_C9BB_DF19_39FA);
    }
}
