pub fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

pub fn crc64(data: &[u8]) -> u64 {
    let mut digest = crc64fast::Digest::new();
    digest.write(data);
    digest.sum64()
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
