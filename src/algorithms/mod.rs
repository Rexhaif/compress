//! Compression and integrity algorithm implementations.
//!
//! Keep algorithm families behind this namespace so application code does not
//! accumulate codec-specific modules at the crate root.

pub mod bzip2;
pub mod checks;
pub mod lzma;
pub mod lzma2;
pub mod xz;
