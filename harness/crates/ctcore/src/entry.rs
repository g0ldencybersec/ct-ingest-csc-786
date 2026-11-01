//! A single log entry, decoded identically from either read API.
//!
//! RFC 6962 `get-entries` returns, per entry:
//!   - `leaf_input`: a `MerkleTreeLeaf` (version, leaf_type, TimestampedEntry)
//!   - `extra_data`: an `ASN.1CertChain` (x509 entries) or a
//!     `PrecertChainEntry` (precert entries), with full DER certificates.
//!
//! Static CT data tiles contain a sequence of `TileLeaf`:
//!   TimestampedEntry, then `pre_certificate` (precert entries only), then
//!   `certificate_chain` as SHA-256 fingerprints of the issuer certificates.
//!
//! Both are normalized into [`Entry`]. The Merkle leaf bytes are identical by
//! construction, and the chain is compared by fingerprint.

use crate::merkle::{leaf_hash, Hash};
use crate::tls::Reader;
use crate::{sha256, Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum EntryKind {
    X509,
    Precert,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// `MerkleTreeLeaf` bytes; `leaf_hash(merkle_leaf)` is the tree leaf.
    pub merkle_leaf: Vec<u8>,
    pub kind: EntryKind,
    pub timestamp_ms: u64,
    /// For x509 entries: the end-entity certificate (DER).
    /// For precert entries: the full pre-certificate (DER).
    pub submitted_cert: Vec<u8>,
    /// SHA-256 fingerprints of the chain certificates, in order.
    pub chain_fingerprints: Vec<[u8; 32]>,
}

impl Entry {
    pub fn leaf_hash(&self) -> Hash {
        leaf_hash(&self.merkle_leaf)
    }
}

/// Parsed `TimestampedEntry`, with the raw bytes it occupied.
struct Timestamped<'a> {
    raw: &'a [u8],
    timestamp_ms: u64,
    kind: EntryKind,
    /// x509 entries: the certificate. Precert entries: the TBSCertificate.
    signed: &'a [u8],
}

fn read_timestamped<'a>(r: &mut Reader<'a>) -> Result<Timestamped<'a>> {
    let start = r.pos();
    let timestamp_ms = r.u64()?;
    let kind = match r.u16()? {
        0 => EntryKind::X509,
        1 => EntryKind::Precert,
        t => return Err(Error::Invalid(format!("unknown LogEntryType {t}"))),
    };
    let signed = match kind {
        EntryKind::X509 => r.vec24()?,
        EntryKind::Precert => {
            let _issuer_key_hash = r.take(32)?;
            r.vec24()?
        }
    };
    let _extensions = r.vec16()?;
    Ok(Timestamped {
        raw: r.slice_from(start),
        timestamp_ms,
        kind,
        signed,
    })
}

fn merkle_leaf_from_timestamped(raw_te: &[u8]) -> Vec<u8> {
    // MerkleTreeLeaf: Version v1 (0), MerkleLeafType timestamped_entry (0).
    let mut out = Vec::with_capacity(raw_te.len() + 2);
    out.push(0);
    out.push(0);
    out.extend_from_slice(raw_te);
    out
}

/// Parse every `TileLeaf` in a decompressed Static CT data tile.
pub fn parse_data_tile(tile: &[u8]) -> Result<Vec<Entry>> {
    let mut r = Reader::new(tile);
    let mut out = Vec::with_capacity(256);
    while !r.is_empty() {
        let te = read_timestamped(&mut r)?;
        let submitted_cert = match te.kind {
            EntryKind::X509 => te.signed.to_vec(),
            EntryKind::Precert => r.vec24()?.to_vec(),
        };
        let chain_bytes = r.vec16()?;
        if chain_bytes.len() % 32 != 0 {
            return Err(Error::Invalid(format!(
                "certificate_chain length {} is not a multiple of 32",
                chain_bytes.len()
            )));
        }
        let chain_fingerprints = chain_bytes
            .chunks_exact(32)
            .map(|c| c.try_into().expect("32 bytes"))
            .collect();
        out.push(Entry {
            merkle_leaf: merkle_leaf_from_timestamped(te.raw),
            kind: te.kind,
            timestamp_ms: te.timestamp_ms,
            submitted_cert,
            chain_fingerprints,
        });
    }
    Ok(out)
}

/// Parse one RFC 6962 `get-entries` element (already base64-decoded).
///
/// Returns the entry plus the full DER chain certificates, which callers can
/// use for replay; the fingerprints are computed here.
pub fn parse_rfc6962_entry(leaf_input: &[u8], extra_data: &[u8]) -> Result<(Entry, Vec<Vec<u8>>)> {
    let mut r = Reader::new(leaf_input);
    let version = r.u8()?;
    let leaf_type = r.u8()?;
    if version != 0 || leaf_type != 0 {
        return Err(Error::Invalid(format!(
            "unexpected MerkleTreeLeaf version {version} / type {leaf_type}"
        )));
    }
    let te = read_timestamped(&mut r)?;
    if !r.is_empty() {
        return Err(Error::Invalid("trailing bytes after MerkleTreeLeaf".into()));
    }

    let mut x = Reader::new(extra_data);
    let submitted_cert = match te.kind {
        EntryKind::X509 => te.signed.to_vec(),
        EntryKind::Precert => x.vec24()?.to_vec(),
    };
    let chain_blob = x.vec24()?;
    if !x.is_empty() {
        return Err(Error::Invalid("trailing bytes after extra_data".into()));
    }
    let mut c = Reader::new(chain_blob);
    let mut chain = Vec::new();
    while !c.is_empty() {
        chain.push(c.vec24()?.to_vec());
    }
    let chain_fingerprints = chain.iter().map(|der| sha256(der)).collect();

    Ok((
        Entry {
            merkle_leaf: leaf_input.to_vec(),
            kind: te.kind,
            timestamp_ms: te.timestamp_ms,
            submitted_cert,
            chain_fingerprints,
        },
        chain,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn te_x509(ts: u64, cert: &[u8]) -> Vec<u8> {
        let mut v = ts.to_be_bytes().to_vec();
        v.extend_from_slice(&0u16.to_be_bytes());
        v.extend_from_slice(&(cert.len() as u32).to_be_bytes()[1..]);
        v.extend_from_slice(cert);
        v.extend_from_slice(&0u16.to_be_bytes()); // extensions
        v
    }

    #[test]
    fn tile_and_rfc6962_agree() {
        let cert = b"leaf-cert-der";
        let issuer = b"issuer-cert-der";
        let te = te_x509(1234, cert);

        // Tile form.
        let mut tile = te.clone();
        tile.extend_from_slice(&32u16.to_be_bytes());
        tile.extend_from_slice(&sha256(issuer));
        let from_tile = parse_data_tile(&tile).unwrap();

        // RFC 6962 form.
        let leaf_input = merkle_leaf_from_timestamped(&te);
        let mut chain_blob = (issuer.len() as u32).to_be_bytes()[1..].to_vec();
        chain_blob.extend_from_slice(issuer);
        let mut extra = (chain_blob.len() as u32).to_be_bytes()[1..].to_vec();
        extra.extend_from_slice(&chain_blob);
        let (from_rfc, chain) = parse_rfc6962_entry(&leaf_input, &extra).unwrap();

        assert_eq!(from_tile.len(), 1);
        assert_eq!(from_tile[0], from_rfc);
        assert_eq!(chain, vec![issuer.to_vec()]);
    }
}
