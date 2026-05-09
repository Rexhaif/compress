//! bzip2 container and block-sorting codec implementation.
//!
//! The format layers are intentionally split so the application layer only
//! sees the container API while the transform and entropy coding pieces remain
//! testable in isolation.

mod bitstream;
mod block;
mod bwt;
mod container;
mod crc;
mod huffman;
mod mtf;

pub use container::{
    Bzip2Options, decode_stream_with_threads, encode_reader_to_writer,
    encode_reader_to_writer_with_capacity, inspect_stream,
};
