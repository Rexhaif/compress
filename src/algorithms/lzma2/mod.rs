use crate::algorithms::lzma::{
    self, CompressionMode, EncoderOptions, LzmaDecoder, LzmaEncoder, LzmaProperties,
};
use crate::error::{Error, Result};

const LZMA2_COMPRESSED_UNPACK_MAX: usize = 2 * 1024 * 1024;
const LZMA2_DENSE_CHUNK_MAX: usize = 192 * 1024;
const LZMA2_HIGH_SCORE_CHUNK_MAX: usize = 96 * 1024;
const LZMA2_PACKED_CHUNK_MAX: usize = 1 << 16;
const LZMA2_UNCOMPRESSED_CHUNK_MAX: usize = 64 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct Lzma2Options {
    pub depth: u32,
    pub dict_size: u32,
    pub match_finder: lzma::MatchFinderKind,
    pub mode: CompressionMode,
    pub nice: u32,
    pub normal_chunk_max: usize,
    pub properties: LzmaProperties,
}

pub fn encode(data: &[u8], options: &Lzma2Options) -> Result<Vec<u8>> {
    let mut encoded = Vec::with_capacity(data.len() / 2 + 32);
    let mut encoder = LzmaEncoder::new(lzma_options(options), data.len());
    let mut state = Lzma2EncodeState::new();
    let mut offset = 0usize;

    while offset < data.len() {
        let plan = plan_chunk(data, offset, options);
        let end = candidate_end(data, offset, plan.unpack_size);
        let packet = if plan.attempt_compression {
            build_packet(&mut encoder, data, offset, end, options, state)?
        } else {
            build_uncompressed_packet(&mut encoder, data, offset, end, state)
        };

        encoded.extend_from_slice(&packet.bytes);
        state = packet.next_state;
        offset = packet.end;
    }

    encoded.push(0x00);
    verify_lzma2_stream(&encoded, data, options)?;

    Ok(encoded)
}

fn plan_chunk(data: &[u8], start: usize, options: &Lzma2Options) -> ChunkPlan {
    if options.mode == CompressionMode::Fast {
        return ChunkPlan {
            attempt_compression: true,
            unpack_size: LZMA2_UNCOMPRESSED_CHUNK_MAX,
        };
    }

    let score = compressibility_score(data, start);
    let low_variety = has_low_byte_variety(data, start);
    let unpack_size = if score >= 70 && low_variety {
        LZMA2_DENSE_CHUNK_MAX
    } else if options.normal_chunk_max >= LZMA2_UNCOMPRESSED_CHUNK_MAX && score >= 53 {
        LZMA2_HIGH_SCORE_CHUNK_MAX
    } else if score >= 12 {
        options.normal_chunk_max
    } else {
        LZMA2_UNCOMPRESSED_CHUNK_MAX
    };

    ChunkPlan {
        attempt_compression: score >= 3 || low_variety,
        unpack_size,
    }
}

fn has_low_byte_variety(data: &[u8], start: usize) -> bool {
    let end = (start + 4096).min(data.len());
    let mut seen = [false; 256];
    let mut unique = 0usize;
    let mut position = start;

    while position < end {
        let byte = data[position] as usize;
        if !seen[byte] {
            seen[byte] = true;
            unique += 1;
            if unique > 4 {
                return false;
            }
        }

        position += 1;
    }

    true
}

fn compressibility_score(data: &[u8], start: usize) -> usize {
    let end = (start + 16 * 1024).min(data.len());
    if end.saturating_sub(start) < 4 {
        return 0;
    }

    let mut seen = [false; 65_536];
    let mut repeats = 0usize;
    let mut position = start;

    while position + 4 <= end {
        let hash = sample_hash4(data, position);
        if seen[hash] {
            repeats += 1;
        } else {
            seen[hash] = true;
        }

        position += 4;
    }

    repeats * 100 / ((end - start) / 4)
}

fn build_packet(
    encoder: &mut LzmaEncoder,
    data: &[u8],
    start: usize,
    end: usize,
    options: &Lzma2Options,
    state: Lzma2EncodeState,
) -> Result<PacketCandidate> {
    let resets_lzma_state = lzma_state_resets(&state);
    if !state.dictionary_reset_done {
        encoder.reset_dictionary(lzma_options(options), data.len());
    } else if resets_lzma_state {
        encoder.reset_state();
    }

    let snapshot = encoder.snapshot_state();
    let dictionary_start = if state.dictionary_reset_done {
        0
    } else {
        start
    };
    let unpack_size = end - start;
    let mut bytes = Vec::new();
    let mut next_state = state;
    let compressed =
        encoder.encode_range_limited(data, start, end, dictionary_start, LZMA2_PACKED_CHUNK_MAX)?;
    let compressed_packet = if let Some(compressed) = compressed.as_ref() {
        compressed.len() + compressed_header_size(&state) < uncompressed_fallback_size(unpack_size)
            && verify_lzma_chunk(
                compressed,
                data,
                start,
                end,
                options,
                dictionary_start,
                resets_lzma_state,
            )?
    } else {
        false
    };

    if compressed_packet {
        append_lzma_chunk(
            &mut bytes,
            compressed.as_ref().expect("compressed packet was selected"),
            unpack_size,
            options.properties,
            &mut next_state,
        )?;
    } else {
        encoder.restore_state(snapshot);
        append_uncompressed_packets(&mut bytes, data, start, end, &mut next_state);
    }

    Ok(PacketCandidate {
        bytes,
        end,
        next_state,
    })
}

fn build_uncompressed_packet(
    encoder: &mut LzmaEncoder,
    data: &[u8],
    start: usize,
    end: usize,
    state: Lzma2EncodeState,
) -> PacketCandidate {
    let mut bytes = Vec::new();
    let mut next_state = state;
    let dictionary_start = if state.dictionary_reset_done {
        0
    } else {
        start
    };

    append_uncompressed_packets(&mut bytes, data, start, end, &mut next_state);
    encoder.observe_uncompressed_range(data, start, end, dictionary_start);

    PacketCandidate {
        bytes,
        end,
        next_state,
    }
}

fn sample_hash4(data: &[u8], position: usize) -> usize {
    let mut value = u32::from(data[position]);
    value |= u32::from(data[position + 1]) << 8;
    value |= u32::from(data[position + 2]) << 16;
    value |= u32::from(data[position + 3]) << 24;

    ((value.wrapping_mul(2_654_435_761)) >> 16) as usize & 65_535
}

fn lzma_state_resets(state: &Lzma2EncodeState) -> bool {
    state.state_reset_pending || !state.properties_written || !state.dictionary_reset_done
}

fn candidate_end(data: &[u8], start: usize, unpack_size: usize) -> usize {
    let size = unpack_size.min(LZMA2_COMPRESSED_UNPACK_MAX);
    (start + size).min(data.len())
}

fn verify_lzma_chunk(
    compressed: &[u8],
    data: &[u8],
    start: usize,
    end: usize,
    options: &Lzma2Options,
    dictionary_start: usize,
    resets_lzma_state: bool,
) -> Result<bool> {
    if !cfg!(debug_assertions) {
        return Ok(true);
    }

    if !resets_lzma_state {
        return Ok(true);
    }

    let mut decoder = LzmaDecoder::new(options.properties, options.dict_size);
    let mut output = data[..start].to_vec();

    if decoder
        .decode_chunk(compressed, &mut output, end - start, dictionary_start)
        .is_err()
    {
        return Ok(false);
    }

    Ok(output[start..] == data[start..end])
}

fn verify_lzma2_stream(encoded: &[u8], data: &[u8], options: &Lzma2Options) -> Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }

    let decoded = decode(encoded, options.dict_size)?;
    if decoded != data {
        return Err(Error::Format("LZMA2 encoder verification failed"));
    }

    Ok(())
}

fn append_lzma_chunk(
    encoded: &mut Vec<u8>,
    compressed: &[u8],
    unpack_size: usize,
    properties: LzmaProperties,
    state: &mut Lzma2EncodeState,
) -> Result<()> {
    if unpack_size == 0 {
        return Ok(());
    }

    if unpack_size > LZMA2_COMPRESSED_UNPACK_MAX {
        return Err(Error::Format("LZMA2 chunk unpack size is too large"));
    }

    if compressed.is_empty() {
        return Err(Error::Format("empty LZMA range chunk"));
    }

    let unpack_field = (unpack_size as u32) - 1;
    let pack_field = (compressed.len() as u32) - 1;
    let has_properties = !state.properties_written || !state.dictionary_reset_done;
    let control_base = if !state.dictionary_reset_done {
        0xE0
    } else if has_properties {
        0xC0
    } else if state.state_reset_pending {
        0xA0
    } else {
        0x80
    };
    let control = control_base | ((unpack_field >> 16) as u8);

    encoded.push(control);
    encoded.extend_from_slice(&(unpack_field as u16).to_be_bytes());
    encoded.extend_from_slice(&(pack_field as u16).to_be_bytes());
    if has_properties {
        encoded.push(properties.encode()?);
        state.properties_written = true;
    }
    encoded.extend_from_slice(compressed);
    state.dictionary_reset_done = true;
    state.state_reset_pending = false;

    Ok(())
}

fn compressed_header_size(state: &Lzma2EncodeState) -> usize {
    if state.properties_written { 5 } else { 6 }
}

fn uncompressed_fallback_size(unpack_size: usize) -> usize {
    let chunks = unpack_size.div_ceil(LZMA2_UNCOMPRESSED_CHUNK_MAX);

    unpack_size + chunks * 3
}

fn append_uncompressed_packets(
    encoded: &mut Vec<u8>,
    data: &[u8],
    start: usize,
    end: usize,
    state: &mut Lzma2EncodeState,
) {
    let mut offset = start;

    while offset < end {
        let chunk_end = (offset + LZMA2_UNCOMPRESSED_CHUNK_MAX).min(end);
        let packet = uncompressed_packet(&data[offset..chunk_end], !state.dictionary_reset_done);

        encoded.extend_from_slice(&packet);
        state.dictionary_reset_done = true;
        state.state_reset_pending = true;
        offset = chunk_end;
    }
}

fn uncompressed_packet(data: &[u8], reset_dictionary: bool) -> Vec<u8> {
    let mut packet = Vec::with_capacity(data.len() + 3);
    let size_field = (data.len() as u16).wrapping_sub(1);

    packet.push(if reset_dictionary { 0x01 } else { 0x02 });
    packet.extend_from_slice(&size_field.to_be_bytes());
    packet.extend_from_slice(data);

    packet
}

fn lzma_options(options: &Lzma2Options) -> EncoderOptions {
    EncoderOptions {
        depth: options.depth,
        dict_size: options.dict_size,
        match_finder: options.match_finder,
        mode: options.mode,
        nice: options.nice,
        properties: options.properties,
    }
}

#[derive(Clone, Copy)]
struct Lzma2EncodeState {
    dictionary_reset_done: bool,
    properties_written: bool,
    state_reset_pending: bool,
}

impl Lzma2EncodeState {
    fn new() -> Lzma2EncodeState {
        Lzma2EncodeState {
            dictionary_reset_done: false,
            properties_written: false,
            state_reset_pending: false,
        }
    }
}

struct PacketCandidate {
    bytes: Vec<u8>,
    end: usize,
    next_state: Lzma2EncodeState,
}

struct ChunkPlan {
    attempt_compression: bool,
    unpack_size: usize,
}

pub fn decode(data: &[u8], dict_size: u32) -> Result<Vec<u8>> {
    decode_with_capacity(data, dict_size, 0)
}

pub fn decode_with_capacity(data: &[u8], dict_size: u32, capacity: usize) -> Result<Vec<u8>> {
    let mut decoder = Lzma2Decoder {
        data,
        dict_size,
        dictionary_start: 0,
        index: 0,
        lzma: None,
        need_dictionary_reset: true,
        need_properties: true,
        output: Vec::with_capacity(capacity),
    };

    decoder.decode()
}

struct Lzma2Decoder<'a> {
    data: &'a [u8],
    dict_size: u32,
    dictionary_start: usize,
    index: usize,
    lzma: Option<LzmaDecoder>,
    need_dictionary_reset: bool,
    need_properties: bool,
    output: Vec<u8>,
}

impl<'a> Lzma2Decoder<'a> {
    fn decode(&mut self) -> Result<Vec<u8>> {
        loop {
            let control = self.read_u8()?;

            if control == 0x00 {
                if self.index == self.data.len() {
                    return Ok(std::mem::take(&mut self.output));
                }

                return Err(Error::Format("trailing data after LZMA2 end marker"));
            }

            if control == 0x01 {
                self.decode_uncompressed(true)?;
            } else if control == 0x02 {
                self.decode_uncompressed(false)?;
            } else if control >= 0x80 {
                self.decode_lzma(control)?;
            } else {
                return Err(Error::Format("invalid LZMA2 control byte"));
            }
        }
    }

    fn decode_uncompressed(&mut self, reset_dictionary: bool) -> Result<()> {
        if self.need_dictionary_reset && !reset_dictionary {
            return Err(Error::Format("LZMA2 stream did not reset dictionary"));
        }

        if reset_dictionary {
            self.dictionary_start = self.output.len();
            self.need_dictionary_reset = false;
        }

        let size = usize::from(self.read_u16()?) + 1;
        let chunk = self.read_slice(size)?;
        self.output.extend_from_slice(chunk);

        Ok(())
    }

    fn decode_lzma(&mut self, control: u8) -> Result<()> {
        if self.need_dictionary_reset && control < 0xE0 {
            return Err(Error::Format("first LZMA2 chunk did not reset dictionary"));
        }

        let has_properties = control >= 0xC0;
        if control >= 0xE0 {
            self.dictionary_start = self.output.len();
            self.need_dictionary_reset = false;
            self.reset_lzma_state()?;
        } else if control >= 0xA0 {
            self.reset_lzma_state()?;
        } else if self.need_properties {
            return Err(Error::Format("LZMA2 chunk used missing properties"));
        }

        let unpack_size =
            (((usize::from(control) & 0x1F) << 16) | usize::from(self.read_u16()?)) + 1;
        let pack_size = usize::from(self.read_u16()?) + 1;

        if has_properties {
            self.decode_lzma_properties()?;
        }

        let compressed = self.read_slice(pack_size)?;

        let decoder = self
            .lzma
            .as_mut()
            .ok_or(Error::Format("LZMA2 properties missing"))?;
        decoder.decode_chunk(
            compressed,
            &mut self.output,
            unpack_size,
            self.dictionary_start,
        )?;

        if self.output.len() < self.dictionary_start {
            return Err(Error::Format("LZMA2 dictionary moved backwards"));
        }

        Ok(())
    }

    fn reset_lzma_state(&mut self) -> Result<()> {
        if let Some(decoder) = self.lzma.as_mut() {
            decoder.reset_state();
            self.need_properties = false;
            Ok(())
        } else {
            self.need_properties = true;
            Ok(())
        }
    }

    fn decode_lzma_properties(&mut self) -> Result<()> {
        let byte = self.read_u8()?;
        let properties = LzmaProperties::decode(byte)?;

        if properties.lc + properties.lp > 4 {
            return Err(Error::Format("LZMA2 lc + lp is too large"));
        }

        self.lzma = Some(LzmaDecoder::new(properties, self.dict_size));
        self.need_properties = false;

        Ok(())
    }

    fn read_u8(&mut self) -> Result<u8> {
        if self.index < self.data.len() {
            let byte = self.data[self.index];
            self.index += 1;
            Ok(byte)
        } else {
            Err(Error::Format("unexpected end of LZMA2 data"))
        }
    }

    fn read_u16(&mut self) -> Result<u16> {
        let high = self.read_u8()?;
        let low = self.read_u8()?;

        Ok(u16::from_be_bytes([high, low]))
    }

    fn read_slice(&mut self, size: usize) -> Result<&'a [u8]> {
        let end = self
            .index
            .checked_add(size)
            .ok_or(Error::Format("LZMA2 size overflow"))?;

        if end <= self.data.len() {
            let slice = &self.data[self.index..end];
            self.index = end;
            Ok(slice)
        } else {
            Err(Error::Format("truncated LZMA2 chunk"))
        }
    }
}
