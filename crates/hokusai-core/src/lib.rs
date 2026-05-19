//! hokusai-core — Pure Rust brush engine inspired by libmypaint.
//!
//! This crate provides:
//! - Brush settings / inputs / mapping types ([`setting`], [`input`])
//! - The [`Brush`] value type and per-stroke [`BrushState`]
//! - Fixed-point pixel math compatible with libmypaint ([`fix15`])
//! - The [`TiledSurface`] abstraction and default `draw_dab` / `get_color`
//!
//! Surface backends and `.myb` JSON parsing live in sibling crates.

pub mod brush;
pub mod brushmodes;
pub mod color;
pub mod evaluator;
pub mod fix15;
pub mod input;
pub mod mapping;
pub mod rng;
pub mod setting;
pub mod spectral;
pub mod state;
pub mod stroke;
pub mod surface;
pub mod tile;

pub use brush::Brush;
pub use input::{BrushInput, NUM_INPUTS};
pub use mapping::{InputMapping, SettingValue};
pub use setting::{BrushSetting, NUM_SETTINGS};
pub use state::BrushState;
pub use surface::{Dab, TiledSurface};
pub use tile::{TilePixels, TILE_SIZE};
