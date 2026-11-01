//! Static CT tile addressing (C2SP tlog-tiles / static-ct-api).

/// Entries per full tile.
pub const TILE_WIDTH: u64 = 256;

/// Encode a tile index as a path, e.g. `1234067` -> `x001/x234/067`.
pub fn index_path(mut n: u64) -> String {
    let mut s = format!("{:03}", n % 1000);
    n /= 1000;
    while n > 0 {
        s = format!("x{:03}/{s}", n % 1000);
        n /= 1000;
    }
    s
}

/// Path of a data tile relative to the monitoring prefix.
///
/// `width` is `Some(w)` for a partial tile of `w` entries (`.p/<w>`).
pub fn data_tile_path(tile_index: u64, width: Option<u64>) -> String {
    match width {
        Some(w) if w < TILE_WIDTH => format!("tile/data/{}.p/{w}", index_path(tile_index)),
        _ => format!("tile/data/{}", index_path(tile_index)),
    }
}

/// The tiles needed to cover entries `[0, tree_size)`: (index, partial width).
pub fn tiles_for(tree_size: u64) -> Vec<(u64, Option<u64>)> {
    let full = tree_size / TILE_WIDTH;
    let rem = tree_size % TILE_WIDTH;
    let mut v: Vec<_> = (0..full).map(|i| (i, None)).collect();
    if rem > 0 {
        v.push((full, Some(rem)));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(index_path(0), "000");
        assert_eq!(index_path(5), "005");
        assert_eq!(index_path(1000), "x001/000");
        assert_eq!(index_path(1234067), "x001/x234/067");
        assert_eq!(data_tile_path(3, Some(10)), "tile/data/003.p/10");
        assert_eq!(data_tile_path(3, None), "tile/data/003");
    }

    #[test]
    fn tiles_cover_tree() {
        assert_eq!(tiles_for(0), vec![]);
        assert_eq!(tiles_for(256), vec![(0, None)]);
        assert_eq!(tiles_for(300), vec![(0, None), (1, Some(44))]);
    }
}
