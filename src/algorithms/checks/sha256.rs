pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut state = [
        0x6A09_E667,
        0xBB67_AE85,
        0x3C6E_F372,
        0xA54F_F53A,
        0x510E_527F,
        0x9B05_688C,
        0x1F83_D9AB,
        0x5BE0_CD19,
    ];

    let mut chunks = data.chunks_exact(64);
    for chunk in &mut chunks {
        sha256_compress(&mut state, chunk);
    }

    let remainder = chunks.remainder();
    let bit_length = (data.len() as u64) * 8;
    let mut block = [0u8; 128];
    block[..remainder.len()].copy_from_slice(remainder);
    block[remainder.len()] = 0x80;

    let length_offset = if remainder.len() < 56 { 56 } else { 120 };
    block[length_offset..length_offset + 8].copy_from_slice(&bit_length.to_be_bytes());

    sha256_compress(&mut state, &block[..64]);
    if length_offset == 120 {
        sha256_compress(&mut state, &block[64..128]);
    }

    let mut digest = [0u8; 32];
    for (index, word) in state.iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }

    digest
}

fn sha256_compress(state: &mut [u32; 8], block: &[u8]) {
    debug_assert_eq!(block.len(), 64);

    let words = sha256_schedule(block);
    sha256_rounds(state, &words);
}

fn sha256_schedule(block: &[u8]) -> [u32; 64] {
    let mut words = [0u32; 64];
    for (index, word) in words.iter_mut().enumerate().take(16) {
        let offset = index * 4;
        *word = u32::from_be_bytes([
            block[offset],
            block[offset + 1],
            block[offset + 2],
            block[offset + 3],
        ]);
    }

    for index in 16..64 {
        let s0 = words[index - 15].rotate_right(7)
            ^ words[index - 15].rotate_right(18)
            ^ (words[index - 15] >> 3);
        let s1 = words[index - 2].rotate_right(17)
            ^ words[index - 2].rotate_right(19)
            ^ (words[index - 2] >> 10);
        words[index] = words[index - 16]
            .wrapping_add(s0)
            .wrapping_add(words[index - 7])
            .wrapping_add(s1);
    }

    words
}

fn sha256_rounds(state: &mut [u32; 8], words: &[u32; 64]) {
    let mut working = [
        state[0], state[1], state[2], state[3], state[4], state[5], state[6], state[7],
    ];

    for index in 0..64 {
        sha256_round_step(&mut working, words[index], SHA256_K[index]);
    }

    for index in 0..8 {
        state[index] = state[index].wrapping_add(working[index]);
    }
}

fn sha256_round_step(working: &mut [u32; 8], word: u32, constant: u32) {
    let choice = (working[4] & working[5]) ^ (!working[4] & working[6]);
    let majority =
        (working[0] & working[1]) ^ (working[0] & working[2]) ^ (working[1] & working[2]);
    let big_s0 =
        working[0].rotate_right(2) ^ working[0].rotate_right(13) ^ working[0].rotate_right(22);
    let big_s1 =
        working[4].rotate_right(6) ^ working[4].rotate_right(11) ^ working[4].rotate_right(25);
    let temp1 = working[7]
        .wrapping_add(big_s1)
        .wrapping_add(choice)
        .wrapping_add(constant)
        .wrapping_add(word);
    let temp2 = big_s0.wrapping_add(majority);

    working[7] = working[6];
    working[6] = working[5];
    working[5] = working[4];
    working[4] = working[3].wrapping_add(temp1);
    working[3] = working[2];
    working[2] = working[1];
    working[1] = working[0];
    working[0] = temp1.wrapping_add(temp2);
}

const SHA256_K: [u32; 64] = [
    0x428A_2F98,
    0x7137_4491,
    0xB5C0_FBCF,
    0xE9B5_DBA5,
    0x3956_C25B,
    0x59F1_11F1,
    0x923F_82A4,
    0xAB1C_5ED5,
    0xD807_AA98,
    0x1283_5B01,
    0x2431_85BE,
    0x550C_7DC3,
    0x72BE_5D74,
    0x80DE_B1FE,
    0x9BDC_06A7,
    0xC19B_F174,
    0xE49B_69C1,
    0xEFBE_4786,
    0x0FC1_9DC6,
    0x240C_A1CC,
    0x2DE9_2C6F,
    0x4A74_84AA,
    0x5CB0_A9DC,
    0x76F9_88DA,
    0x983E_5152,
    0xA831_C66D,
    0xB003_27C8,
    0xBF59_7FC7,
    0xC6E0_0BF3,
    0xD5A7_9147,
    0x06CA_6351,
    0x1429_2967,
    0x27B7_0A85,
    0x2E1B_2138,
    0x4D2C_6DFC,
    0x5338_0D13,
    0x650A_7354,
    0x766A_0ABB,
    0x81C2_C92E,
    0x9272_2C85,
    0xA2BF_E8A1,
    0xA81A_664B,
    0xC24B_8B70,
    0xC76C_51A3,
    0xD192_E819,
    0xD699_0624,
    0xF40E_3585,
    0x106A_A070,
    0x19A4_C116,
    0x1E37_6C08,
    0x2748_774C,
    0x34B0_BCB5,
    0x391C_0CB3,
    0x4ED8_AA4A,
    0x5B9C_CA4F,
    0x682E_6FF3,
    0x748F_82EE,
    0x78A5_636F,
    0x84C8_7814,
    0x8CC7_0208,
    0x90BE_FFFA,
    0xA450_6CEB,
    0xBEF9_A3F7,
    0xC671_78F2,
];

#[cfg(test)]
mod tests {
    use super::sha256;

    #[test]
    fn sha256_known_vector() {
        let digest = sha256(b"abc");
        let expected = [
            0xBA, 0x78, 0x16, 0xBF, 0x8F, 0x01, 0xCF, 0xEA, 0x41, 0x41, 0x40, 0xDE, 0x5D, 0xAE,
            0x22, 0x23, 0xB0, 0x03, 0x61, 0xA3, 0x96, 0x17, 0x7A, 0x9C, 0xB4, 0x10, 0xFF, 0x61,
            0xF2, 0x00, 0x15, 0xAD,
        ];

        assert_eq!(digest, expected);
    }
}
