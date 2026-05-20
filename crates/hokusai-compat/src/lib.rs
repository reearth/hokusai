//! Snapshot regression harness for the hokusai brush engine.
//!
//! # Why this exists
//!
//! The "Hokusai" name does not imply behavioural divergence from libmypaint;
//! we target pixel-level parity. This crate is the place where that target
//! gets verified.
//!
//! # What it does today
//!
//! - Each fixture is a directory with `brush.myb` (or a path-relative ref),
//!   `stroke.json`, and an expected `out.png` snapshot.
//! - The runner replays the stroke through a [`MemSurface`], flattens the
//!   painted tiles to RGBA8 over white, and compares to the snapshot using
//!   a mean-absolute-difference tolerance.
//! - With `HOKUSAI_UPDATE_GOLDENS=1`, mismatching snapshots are overwritten
//!   so they can be reviewed in the diff.
//!
//! # Path to true libmypaint parity
//!
//! The committed `out.png` golden images are currently produced by hokusai
//! itself — so the harness only detects **regressions**, not true parity.
//! To upgrade to parity testing, regenerate every `out.png` by feeding the
//! same `stroke.json` to libmypaint's reference C code (e.g. a small
//! command-line wrapper around `mypaint_brush_stroke_to`) and commit those
//! images instead. The runner here doesn't need to change.

#![allow(clippy::needless_range_loop)]

use hokusai_brush as myb;
use hokusai_core::color::linear_to_srgb;
use hokusai_core::tile::TILE_SIZE;
use hokusai_core::{fix15, Brush, BrushState};
use hokusai_tile_mem::MemSurface;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Script {
    /// Path to the `.myb` brush, resolved relative to the script file.
    pub brush: std::path::PathBuf,
    pub width: u32,
    pub height: u32,
    /// Each entry: `[x, y, pressure, dtime_seconds]`.
    pub events: Vec<[f32; 4]>,
}

#[derive(Debug)]
pub struct RenderError(pub String);

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for RenderError {}

/// Replay `script` against `brush` and return an RGBA8 byte buffer composited
/// over a white background, with sRGB encoding applied to match what
/// `image::RgbaImage::save_png` would emit. Tile traversal and flattening
/// are deterministic.
pub fn render(brush: &Brush, script: &Script) -> Vec<u8> {
    let mut state = BrushState::default();
    let mut surface = MemSurface::new();
    // Symmetric warm-up: the libmypaint C wrapper precedes the script with
    // a dt > 5s call at the first event's position so STATE.X / .PRESSURE
    // get seeded before any drawing. Mirror that here so the first script
    // event renders dabs (with pressure interpolating from 0 → its value)
    // instead of being consumed as hokusai's own seed.
    if let Some(first) = script.events.first() {
        brush.stroke_to(
            &mut state,
            &mut surface,
            first[0],
            first[1],
            0.0,
            0.0,
            0.0,
            10.0,
        );
    }
    for ev in &script.events {
        brush.stroke_to(
            &mut state,
            &mut surface,
            ev[0],
            ev[1],
            ev[2],
            0.0,
            0.0,
            ev[3] as f64,
        );
    }
    // No finish_stroke here: libmypaint's reference path does not flush
    // slow_tracking on its own, and the parity goldens are produced without
    // such a flush. Applications that want a trailing-pixel drain should
    // call `Brush::finish_stroke` themselves — see [`render_with_finish`].
    flatten(&surface, script.width, script.height)
}

/// Same as [`render`] but drains `slow_tracking` lag at the end with
/// [`Brush::finish_stroke`]. This is what an interactive app does on
/// pointer-up, so continuity property tests use this variant — otherwise
/// every stroke ends with a `slow_tracking * speed`-sized unpainted tail
/// (which is shared with libmypaint, just normally hidden by app code).
pub fn render_with_finish(brush: &Brush, script: &Script) -> Vec<u8> {
    let mut state = BrushState::default();
    let mut surface = MemSurface::new();
    if let Some(first) = script.events.first() {
        brush.stroke_to(
            &mut state,
            &mut surface,
            first[0],
            first[1],
            0.0,
            0.0,
            0.0,
            10.0,
        );
    }
    for ev in &script.events {
        brush.stroke_to(
            &mut state,
            &mut surface,
            ev[0],
            ev[1],
            ev[2],
            0.0,
            0.0,
            ev[3] as f64,
        );
    }
    brush.finish_stroke(&mut state, &mut surface);
    flatten(&surface, script.width, script.height)
}

fn flatten(surface: &MemSurface, w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![255u8; (w * h * 4) as usize];
    let ts = TILE_SIZE as i32;
    let tiles_x = (w as i32 + ts - 1) / ts;
    let tiles_y = (h as i32 + ts - 1) / ts;

    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let Some(tile) = surface.tile(tx, ty) else {
                continue;
            };
            for ly in 0..TILE_SIZE {
                for lx in 0..TILE_SIZE {
                    let wx = tx * ts + lx as i32;
                    let wy = ty * ts + ly as i32;
                    if wx < 0 || wy < 0 || wx >= w as i32 || wy >= h as i32 {
                        continue;
                    }
                    let p = tile[ly][lx];
                    let a = fix15::to_f32(p[3]);
                    if a <= 0.0 {
                        continue;
                    }
                    let r = fix15::to_f32(p[0]) / a;
                    let g = fix15::to_f32(p[1]) / a;
                    let b = fix15::to_f32(p[2]) / a;
                    // White background, sRGB output.
                    let or = r * a + 1.0 * (1.0 - a);
                    let og = g * a + 1.0 * (1.0 - a);
                    let ob = b * a + 1.0 * (1.0 - a);
                    let idx = ((wy as u32 * w + wx as u32) * 4) as usize;
                    out[idx] = (linear_to_srgb(or).clamp(0.0, 1.0) * 255.0).round() as u8;
                    out[idx + 1] = (linear_to_srgb(og).clamp(0.0, 1.0) * 255.0).round() as u8;
                    out[idx + 2] = (linear_to_srgb(ob).clamp(0.0, 1.0) * 255.0).round() as u8;
                    out[idx + 3] = 255;
                }
            }
        }
    }
    out
}

/// Mean absolute difference per channel, in [0, 255].
pub fn diff_mad(a: &[u8], b: &[u8]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let mut sum = 0u64;
    for (x, y) in a.iter().zip(b.iter()) {
        sum += x.abs_diff(*y) as u64;
    }
    sum as f32 / a.len() as f32
}

pub fn load_brush(path: &std::path::Path) -> Result<Brush, RenderError> {
    let json = std::fs::read_to_string(path)
        .map_err(|e| RenderError(format!("read {}: {e}", path.display())))?;
    myb::from_str(&json).map_err(|e| RenderError(format!("parse {}: {e}", path.display())))
}

pub fn load_script(path: &std::path::Path) -> Result<Script, RenderError> {
    let json = std::fs::read_to_string(path)
        .map_err(|e| RenderError(format!("read {}: {e}", path.display())))?;
    serde_json::from_str(&json).map_err(|e| RenderError(format!("parse {}: {e}", path.display())))
}
