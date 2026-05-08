use crate::error::{Error, Result};

mod crc;
mod sha256;

pub use crc::{crc32, crc64};
pub use sha256::sha256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckType {
    None,
    Crc32,
    Crc64,
    Sha256,
}

impl CheckType {
    pub fn from_name(name: &str) -> Result<CheckType> {
        match name {
            "none" => Ok(CheckType::None),
            "crc32" => Ok(CheckType::Crc32),
            "crc64" => Ok(CheckType::Crc64),
            "sha256" => Ok(CheckType::Sha256),
            _ => Err(Error::Usage("unknown integrity check")),
        }
    }

    pub fn from_xz_id(identifier: u8) -> Result<CheckType> {
        match identifier {
            0x00 => Ok(CheckType::None),
            0x01 => Ok(CheckType::Crc32),
            0x04 => Ok(CheckType::Crc64),
            0x0A => Ok(CheckType::Sha256),
            _ => Err(Error::Unsupported("xz check type")),
        }
    }

    pub fn xz_id(self) -> u8 {
        match self {
            CheckType::None => 0x00,
            CheckType::Crc32 => 0x01,
            CheckType::Crc64 => 0x04,
            CheckType::Sha256 => 0x0A,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            CheckType::None => "None",
            CheckType::Crc32 => "CRC32",
            CheckType::Crc64 => "CRC64",
            CheckType::Sha256 => "SHA-256",
        }
    }

    pub fn size(self) -> u64 {
        match self {
            CheckType::None => 0,
            CheckType::Crc32 => 4,
            CheckType::Crc64 => 8,
            CheckType::Sha256 => 32,
        }
    }
}

pub fn check_bytes(check: CheckType, data: &[u8]) -> Vec<u8> {
    match check {
        CheckType::None => Vec::new(),
        CheckType::Crc32 => crc32(data).to_le_bytes().to_vec(),
        CheckType::Crc64 => crc64(data).to_le_bytes().to_vec(),
        CheckType::Sha256 => sha256(data).to_vec(),
    }
}
