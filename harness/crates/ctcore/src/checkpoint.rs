//! Parsing of the Static CT checkpoint (a C2SP signed note) and RFC 6962 STH.
//!
//! Signatures are not verified yet; for the testbed go/no-go both values come
//! from our own log. Signature verification is a later harness milestone.

use base64::Engine;

use crate::merkle::Hash;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeHead {
    pub tree_size: u64,
    pub root_hash: Hash,
}

#[derive(Debug, Clone)]
pub struct Checkpoint {
    pub origin: String,
    pub head: TreeHead,
    /// The full note text, kept verbatim for provenance records.
    pub raw: String,
}

fn decode_root(b64: &str) -> Result<Hash> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| Error::Invalid(format!("root hash base64: {e}")))?;
    bytes
        .try_into()
        .map_err(|_| Error::Invalid("root hash is not 32 bytes".into()))
}

pub fn parse_checkpoint(text: &str) -> Result<Checkpoint> {
    let mut lines = text.lines();
    let origin = lines
        .next()
        .ok_or_else(|| Error::Invalid("empty checkpoint".into()))?
        .to_string();
    let tree_size = lines
        .next()
        .ok_or_else(|| Error::Invalid("checkpoint missing size".into()))?
        .trim()
        .parse::<u64>()
        .map_err(|e| Error::Invalid(format!("checkpoint size: {e}")))?;
    let root_hash = decode_root(
        lines
            .next()
            .ok_or_else(|| Error::Invalid("checkpoint missing root".into()))?,
    )?;
    Ok(Checkpoint {
        origin,
        head: TreeHead {
            tree_size,
            root_hash,
        },
        raw: text.to_string(),
    })
}

/// RFC 6962 `get-sth` response.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SthJson {
    pub tree_size: u64,
    pub timestamp: u64,
    pub sha256_root_hash: String,
    pub tree_head_signature: String,
}

impl SthJson {
    pub fn head(&self) -> Result<TreeHead> {
        Ok(TreeHead {
            tree_size: self.tree_size,
            root_hash: decode_root(&self.sha256_root_hash)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_note() {
        let note = "example.com/log\n42\n47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=\n\n— example.com/log abcd\n";
        let cp = parse_checkpoint(note).unwrap();
        assert_eq!(cp.origin, "example.com/log");
        assert_eq!(cp.head.tree_size, 42);
        assert_eq!(cp.head.root_hash, crate::merkle::empty_root());
    }
}
