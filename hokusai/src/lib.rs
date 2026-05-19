//! Hokusai — pure Rust brush engine inspired by libmypaint.
//!
//! Umbrella crate that re-exports the active backends based on cargo features.

pub use hokusai_core::*;

#[cfg(feature = "myb-json")]
pub mod myb {
    pub use hokusai_brush::*;
}

#[cfg(feature = "tile-mem")]
pub mod tile_mem {
    pub use hokusai_tile_mem::*;
}

#[cfg(feature = "tiny-skia")]
pub mod tiny_skia {
    pub use hokusai_tiny_skia::*;
}
