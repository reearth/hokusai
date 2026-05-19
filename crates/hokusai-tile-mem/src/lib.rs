//! Reference [`TiledSurface`] implementation backed by a `HashMap` of tiles.
//!
//! This is the canonical surface used for compatibility tests against
//! libmypaint — `tiny-skia` and other backends defer to the same default
//! `draw_dab`/`get_color` in `hokusai-core`, so all backends produce
//! identical pixels here.

use std::collections::{HashMap, HashSet};

use hokusai_core::tile::{empty_tile, TilePixels};
use hokusai_core::TiledSurface;

#[derive(Default)]
pub struct MemSurface {
    tiles: HashMap<(i32, i32), Box<TilePixels>>,
    dirty: HashSet<(i32, i32)>,
    in_atomic: bool,
}

impl MemSurface {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tile(&self, tx: i32, ty: i32) -> Option<&TilePixels> {
        self.tiles.get(&(tx, ty)).map(|b| &**b)
    }

    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }
}

impl TiledSurface for MemSurface {
    fn tile_request_start(&mut self, tx: i32, ty: i32) -> &mut TilePixels {
        if self.in_atomic {
            self.dirty.insert((tx, ty));
        }
        self.tiles.entry((tx, ty)).or_insert_with(empty_tile)
    }

    fn tile_request_end(&mut self, _tx: i32, _ty: i32) {}

    fn tile_lookup(&self, tx: i32, ty: i32) -> Option<&TilePixels> {
        self.tiles.get(&(tx, ty)).map(|b| &**b)
    }

    fn begin_atomic(&mut self) {
        self.in_atomic = true;
        self.dirty.clear();
    }

    fn end_atomic(&mut self) -> Vec<(i32, i32)> {
        self.in_atomic = false;
        self.dirty.drain().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hokusai_core::tile::TILE_SIZE;

    #[test]
    fn lends_and_records_dirty() {
        let mut s = MemSurface::new();
        s.begin_atomic();
        {
            let t = s.tile_request_start(2, -1);
            t[0][0] = [1, 2, 3, 4];
            s.tile_request_end(2, -1);
        }
        let dirty = s.end_atomic();
        assert_eq!(dirty, vec![(2, -1)]);
        assert_eq!(s.tile(2, -1).unwrap()[0][0], [1, 2, 3, 4]);
        assert_eq!(s.tile(0, 0), None);
        let _ = TILE_SIZE;
    }
}
