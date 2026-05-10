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

    pub fn with_output_prefix(bytes: Vec<u8>) -> BitWriter {
        BitWriter {
            bytes,
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
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> BitReader<'a> {
        BitReader { data, bit_pos: 0 }
    }

    pub fn new_at(data: &'a [u8], bit_pos: u64) -> BitReader<'a> {
        BitReader {
            data,
            bit_pos: bit_pos as usize,
        }
    }

    pub fn read_bit(&mut self) -> Result<bool> {
        if self.bit_pos >= self.data.len() * 8 {
            return Err(Error::Format("truncated bzip2 bitstream"));
        }

        let byte = self.data[self.bit_pos / 8];
        let shift = 7 - (self.bit_pos % 8);
        self.bit_pos += 1;

        Ok(((byte >> shift) & 1) != 0)
    }

    pub fn read_bits(&mut self, bits: u8) -> Result<u32> {
        if bits == 0 {
            return Ok(0);
        }
        if self.bit_pos + usize::from(bits) > self.data.len() * 8 {
            return Err(Error::Format("truncated bzip2 bitstream"));
        }
        if bits <= 16 {
            let value = self.peek_bits(bits)?;
            self.bit_pos += usize::from(bits);
            return Ok(value);
        }

        let mut value = 0u32;
        let mut remaining = bits;

        while remaining > 0 {
            let byte = self.data[self.bit_pos / 8];
            let bit_offset = (self.bit_pos % 8) as u8;
            let available = 8 - bit_offset;
            let take = remaining.min(available);
            let shift = available - take;
            let mask = (1u16 << take) - 1;
            value = (value << take) | u32::from((byte >> shift) & mask as u8);
            self.bit_pos += usize::from(take);
            remaining -= take;
        }

        Ok(value)
    }

    pub fn peek_bits(&self, bits: u8) -> Result<u32> {
        if bits == 0 {
            return Ok(0);
        }
        if self.bit_pos + usize::from(bits) > self.data.len() * 8 {
            return Err(Error::Format("truncated bzip2 bitstream"));
        }

        if bits <= 16 {
            let byte_pos = self.bit_pos / 8;
            if byte_pos + 2 < self.data.len() {
                let bit_offset = (self.bit_pos % 8) as u8;
                let word = (u32::from(self.data[byte_pos]) << 16)
                    | (u32::from(self.data[byte_pos + 1]) << 8)
                    | u32::from(self.data[byte_pos + 2]);
                let shift = 24 - u32::from(bit_offset) - u32::from(bits);
                return Ok((word >> shift) & ((1u32 << bits) - 1));
            }
        }

        let mut value = 0u32;
        let mut remaining = bits;
        let mut bit_pos = self.bit_pos;

        while remaining > 0 {
            let byte = self.data[bit_pos / 8];
            let bit_offset = (bit_pos % 8) as u8;
            let available = 8 - bit_offset;
            let take = remaining.min(available);
            let shift = available - take;
            let mask = (1u16 << take) - 1;
            value = (value << take) | u32::from((byte >> shift) & mask as u8);
            bit_pos += usize::from(take);
            remaining -= take;
        }

        Ok(value)
    }

    pub fn skip_bits(&mut self, bits: u8) -> Result<()> {
        if self.bit_pos + usize::from(bits) > self.data.len() * 8 {
            return Err(Error::Format("truncated bzip2 bitstream"));
        }
        self.bit_pos += usize::from(bits);
        Ok(())
    }

    pub fn remaining_bits(&self) -> usize {
        self.data.len() * 8 - self.bit_pos
    }

    pub fn read_bits_u64(&mut self, bits: u8) -> Result<u64> {
        if bits == 0 {
            return Ok(0);
        }
        if self.bit_pos + usize::from(bits) > self.data.len() * 8 {
            return Err(Error::Format("truncated bzip2 bitstream"));
        }

        let mut value = 0u64;
        let mut remaining = bits;

        while remaining > 0 {
            let byte = self.data[self.bit_pos / 8];
            let bit_offset = (self.bit_pos % 8) as u8;
            let available = 8 - bit_offset;
            let take = remaining.min(available);
            let shift = available - take;
            let mask = (1u16 << take) - 1;
            value = (value << take) | u64::from((byte >> shift) & mask as u8);
            self.bit_pos += usize::from(take);
            remaining -= take;
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
        self.bit_pos.div_ceil(8)
    }
}

#[cfg(test)]
mod tests {
    use super::BitReader;

    #[test]
    fn peek_bits_does_not_advance_reader() {
        let mut reader = BitReader::new(&[0b1010_1100, 0b0111_0001, 0b1100_0000]);

        assert_eq!(reader.peek_bits(10).unwrap(), 0b1010_1100_01);
        assert_eq!(reader.remaining_bits(), 24);
        assert_eq!(reader.read_bits(4).unwrap(), 0b1010);
        assert_eq!(reader.peek_bits(9).unwrap(), 0b1100_0111_0);
        assert_eq!(reader.read_bits(9).unwrap(), 0b1100_0111_0);
    }

    #[test]
    fn skip_bits_advances_reader() {
        let mut reader = BitReader::new(&[0b1111_0000, 0b1010_0101]);

        reader.skip_bits(5).unwrap();
        assert_eq!(reader.read_bits(6).unwrap(), 0b000_101);
        assert_eq!(reader.remaining_bits(), 5);
    }
}
