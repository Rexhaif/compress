use crate::error::{Error, Result};

pub struct BitWriter {
    bytes: Vec<u8>,
    buffer: u64,
    filled: u8,
    bit_len: u64,
}

impl BitWriter {
    pub fn with_capacity(capacity: usize) -> BitWriter {
        BitWriter {
            bytes: Vec::with_capacity(capacity),
            buffer: 0,
            filled: 0,
            bit_len: 0,
        }
    }

    #[inline]
    pub fn write_bit(&mut self, bit: bool) {
        self.buffer = (self.buffer << 1) | u64::from(bit);
        self.filled += 1;
        self.bit_len += 1;

        if self.filled == 8 {
            self.bytes.push(self.buffer as u8);
            self.buffer = 0;
            self.filled = 0;
        }
    }

    #[inline]
    pub fn write_bits(&mut self, value: u64, bits: u8) {
        if bits == 0 {
            return;
        }

        let value = if bits == 64 {
            value
        } else {
            value & ((1u64 << bits) - 1)
        };
        self.buffer = (self.buffer << bits) | value;
        self.filled += bits;
        self.bit_len += u64::from(bits);

        while self.filled >= 8 {
            let shift = self.filled - 8;
            self.bytes.push((self.buffer >> shift) as u8);
            self.filled = shift;
            if shift == 0 {
                self.buffer = 0;
            } else {
                self.buffer &= (1u64 << shift) - 1;
            }
        }
    }

    #[inline]
    pub fn write_bits_unmasked(&mut self, value: u64, bits: u8) {
        if bits == 0 {
            return;
        }

        self.buffer = (self.buffer << bits) | value;
        self.filled += bits;
        self.bit_len += u64::from(bits);

        while self.filled >= 8 {
            let shift = self.filled - 8;
            self.bytes.push((self.buffer >> shift) as u8);
            self.filled = shift;
            if shift == 0 {
                self.buffer = 0;
            } else {
                self.buffer &= (1u64 << shift) - 1;
            }
        }
    }

    pub fn write_bit_slice(&mut self, bytes: &[u8], bit_len: u64) {
        let full_bytes = (bit_len / 8) as usize;
        let tail_bits = (bit_len % 8) as u8;

        if self.filled == 0 {
            self.bytes.extend_from_slice(&bytes[..full_bytes]);
            self.bit_len += (full_bytes as u64) * 8;
        } else {
            let filled = self.filled;
            let keep_mask = (1u64 << filled) - 1;
            for &byte in &bytes[..full_bytes] {
                self.bytes
                    .push(((self.buffer << (8 - filled)) | u64::from(byte >> filled)) as u8);
                self.buffer = u64::from(byte) & keep_mask;
            }
            self.bit_len += (full_bytes as u64) * 8;
        }

        if tail_bits != 0 {
            let tail = bytes[full_bytes] >> (8 - tail_bits);
            self.write_bits(u64::from(tail), tail_bits);
        }
    }

    pub fn into_parts(mut self) -> (Vec<u8>, u64) {
        let bit_len = self.bit_len;
        if self.filled > 0 {
            self.bytes.push((self.buffer << (8 - self.filled)) as u8);
            self.buffer = 0;
            self.filled = 0;
        }

        (self.bytes, bit_len)
    }

    pub fn finish(self) -> Vec<u8> {
        self.into_parts().0
    }
}

pub struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: u64,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> BitReader<'a> {
        BitReader { data, bit_pos: 0 }
    }

    pub fn new_at(data: &'a [u8], bit_pos: u64) -> BitReader<'a> {
        BitReader { data, bit_pos }
    }

    pub fn read_bit(&mut self) -> Result<bool> {
        if self.bit_pos >= (self.data.len() as u64) * 8 {
            return Err(Error::Format("truncated bzip2 bitstream"));
        }

        let byte = self.data[(self.bit_pos / 8) as usize];
        let shift = 7 - (self.bit_pos % 8);
        self.bit_pos += 1;

        Ok(((byte >> shift) & 1) != 0)
    }

    pub fn read_bits(&mut self, bits: u8) -> Result<u32> {
        let mut value = 0u32;
        for _ in 0..bits {
            value = (value << 1) | u32::from(self.read_bit()?);
        }

        Ok(value)
    }

    pub fn read_bits_u64(&mut self, bits: u8) -> Result<u64> {
        let mut value = 0u64;
        for _ in 0..bits {
            value = (value << 1) | u64::from(self.read_bit()?);
        }

        Ok(value)
    }

    pub fn align_to_byte(&mut self) {
        let remainder = self.bit_pos % 8;
        if remainder != 0 {
            self.bit_pos += 8 - remainder;
        }
    }

    pub fn consumed_bytes(&self) -> usize {
        self.bit_pos.div_ceil(8) as usize
    }
}
