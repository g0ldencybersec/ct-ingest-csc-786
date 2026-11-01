//! Shared core for the CT ingestion harness.
//!
//! Everything in this crate is used identically by both read paths
//! (RFC 6962 `get-entries` and Static CT tiles). Each fetch backend only
//! turns bytes from the wire into [`entry::Entry`] values; from there on the
//! decode, deduplicate, and verify steps are the same code. That is what lets
//! measured differences be attributed to the access model.

pub mod checkpoint;
pub mod entry;
pub mod merkle;
pub mod sample;
pub mod tile;
pub mod tls;

pub use entry::{Entry, EntryKind};
pub use merkle::{leaf_hash, Frontier, Hash};

/// Errors produced while decoding log data.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("truncated input: needed {needed} more bytes at offset {offset}")]
    Truncated { needed: usize, offset: usize },
    #[error("invalid data: {0}")]
    Invalid(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Decompress `bytes` if they are gzip (magic 1f 8b), otherwise return as-is.
///
/// Static CT data tiles are usually gzip-compressed, but whether the server
/// marks that with `Content-Encoding` varies, so callers keep the raw wire
/// bytes (for byte accounting) and decode here.
pub fn maybe_gunzip(bytes: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        let mut out = Vec::with_capacity(bytes.len() * 4);
        flate2::read::MultiGzDecoder::new(bytes).read_to_end(&mut out)?;
        Ok(out)
    } else {
        Ok(bytes.to_vec())
    }
}

/// SHA-256 of `data`.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).into()
}
