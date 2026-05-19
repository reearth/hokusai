//! 64×64 RGBA fix15 tile — matches libmypaint's tiling so brush math, tile
//! traversal order, and rounding behaviour can be made bit-identical.

pub const TILE_SIZE: usize = 64;

/// `[y][x][rgba]` in fix15 (premultiplied alpha, linear sRGB).
pub type TilePixels = [[[u16; 4]; TILE_SIZE]; TILE_SIZE];

#[inline]
pub fn empty_tile() -> Box<TilePixels> {
    Box::new([[[0u16; 4]; TILE_SIZE]; TILE_SIZE])
}

/// Convert a world pixel coordinate to `(tile_x, tile_y, in_tile_x, in_tile_y)`.
#[inline]
pub fn world_to_tile(x: i32, y: i32) -> (i32, i32, usize, usize) {
    let tx = x.div_euclid(TILE_SIZE as i32);
    let ty = y.div_euclid(TILE_SIZE as i32);
    let ix = x.rem_euclid(TILE_SIZE as i32) as usize;
    let iy = y.rem_euclid(TILE_SIZE as i32) as usize;
    (tx, ty, ix, iy)
}
