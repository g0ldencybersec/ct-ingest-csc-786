//! RFC 6962 Merkle tree hashing with a streaming frontier.
//!
//! [`Frontier`] consumes leaf hashes in index order and keeps only the
//! perfect subtrees along the right edge, so memory is O(log N). Its
//! [`Frontier::root`] equals RFC 6962 `MTH(D[0:n])` for any `n`.

use crate::sha256;

pub type Hash = [u8; 32];

/// `SHA-256(0x00 || leaf)`
pub fn leaf_hash(leaf: &[u8]) -> Hash {
    let mut buf = Vec::with_capacity(leaf.len() + 1);
    buf.push(0u8);
    buf.extend_from_slice(leaf);
    sha256(&buf)
}

/// `SHA-256(0x01 || left || right)`
pub fn node_hash(left: &Hash, right: &Hash) -> Hash {
    let mut buf = [0u8; 65];
    buf[0] = 1;
    buf[1..33].copy_from_slice(left);
    buf[33..].copy_from_slice(right);
    sha256(&buf)
}

/// Hash of the empty tree: `SHA-256("")`.
pub fn empty_root() -> Hash {
    sha256(&[])
}

#[derive(Debug, Default, Clone)]
pub struct Frontier {
    /// (subtree root, subtree size), sizes strictly decreasing powers of two.
    stack: Vec<(Hash, u64)>,
    size: u64,
}

impl Frontier {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn push(&mut self, leaf: Hash) {
        self.stack.push((leaf, 1));
        self.size += 1;
        while self.stack.len() >= 2 {
            let n = self.stack.len();
            if self.stack[n - 1].1 != self.stack[n - 2].1 {
                break;
            }
            let (right, size) = self.stack.pop().expect("len >= 2");
            let (left, _) = self.stack.pop().expect("len >= 1");
            self.stack.push((node_hash(&left, &right), size * 2));
        }
    }

    /// Root of the tree over every leaf pushed so far.
    pub fn root(&self) -> Hash {
        let mut iter = self.stack.iter().rev();
        let Some((last, _)) = iter.next() else {
            return empty_root();
        };
        let mut acc = *last;
        for (h, _) in iter {
            acc = node_hash(h, &acc);
        }
        acc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Direct recursive RFC 6962 MTH, used as a reference.
    fn mth(leaves: &[Hash]) -> Hash {
        match leaves.len() {
            0 => empty_root(),
            1 => leaves[0],
            n => {
                let mut k = 1;
                while k * 2 < n {
                    k *= 2;
                }
                node_hash(&mth(&leaves[..k]), &mth(&leaves[k..]))
            }
        }
    }

    #[test]
    fn frontier_matches_reference_for_all_small_sizes() {
        let leaves: Vec<Hash> = (0u32..300).map(|i| leaf_hash(&i.to_be_bytes())).collect();
        let mut f = Frontier::new();
        assert_eq!(f.root(), empty_root());
        for (i, l) in leaves.iter().enumerate() {
            f.push(*l);
            assert_eq!(f.root(), mth(&leaves[..=i]), "size {}", i + 1);
        }
    }

    #[test]
    fn empty_tree_root_is_known_value() {
        // RFC 6962 / RFC 9162 test vector for the empty tree.
        assert_eq!(
            hex::encode(empty_root()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
