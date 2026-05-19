//! Property tests for stroke continuity.
//!
//! These assert that a steady left-to-right stroke produces a continuous
//! line — no big white gaps along the stroke axis. They simulate the event
//! cadence a browser PointerEvent stream produces (≈60 Hz with small
//! per-event deltas, occasionally larger), which is where the original
//! report came from.

use std::path::PathBuf;

use hokusai_compat::{load_brush, render, Script};

fn brush_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../hokusai/examples/fixtures")
        .join(name)
}

fn horizontal_script(brush: &str, y: f32, dx: f32, dt: f32, n: usize) -> Script {
    let mut events = Vec::with_capacity(n + 1);
    let start_x = 20.0;
    for i in 0..=n {
        events.push([start_x + dx * i as f32, y, 1.0, dt]);
    }
    Script {
        brush: brush_path(brush),
        width: (start_x as u32) + (dx as u32) * (n as u32) + 40,
        height: 80,
        events,
    }
}

struct GapInfo {
    longest: u32,
    longest_start: u32,
}

/// Longest run of all-white columns within `band_y..band_y+band_h`, scanned
/// over `x_range`. Returns length and where the run starts.
fn longest_white_run(
    pixels: &[u8],
    width: u32,
    band_y: u32,
    band_h: u32,
    x_range: std::ops::Range<u32>,
) -> GapInfo {
    let mut longest = 0u32;
    let mut longest_start = 0u32;
    let mut current = 0u32;
    let mut current_start = x_range.start;
    for x in x_range {
        let mut any_painted = false;
        for y in band_y..(band_y + band_h) {
            let idx = ((y * width + x) * 4) as usize;
            let p = &pixels[idx..idx + 3];
            if p[0] < 250 || p[1] < 250 || p[2] < 250 {
                any_painted = true;
                break;
            }
        }
        if any_painted {
            if current > longest {
                longest = current;
                longest_start = current_start;
            }
            current = 0;
            current_start = x + 1;
        } else {
            current += 1;
        }
    }
    if current > longest {
        longest = current;
        longest_start = current_start;
    }
    GapInfo {
        longest,
        longest_start,
    }
}

const MAX_GAP_PX: u32 = 6;

fn check(name: &str, brush: &str) {
    let script = horizontal_script(brush, 40.0, 8.0, 0.016, 80);
    let brush_obj = load_brush(&script.brush).unwrap();
    let pixels = render(&brush_obj, &script);
    let gap = longest_white_run(&pixels, script.width, 30, 20, 40..(script.width - 40));
    assert!(
        gap.longest <= MAX_GAP_PX,
        "{name}: longest white run = {} px at x={} (allowed ≤ {MAX_GAP_PX})",
        gap.longest,
        gap.longest_start,
    );
}

#[test]
fn marker_fat_horizontal_no_big_gaps() {
    // Mimic a browser pointermove burst: 16 ms / event, ~8 px between events.
    check("marker_fat", "marker_fat.myb");
}

#[test]
fn calligraphy_horizontal_no_big_gaps() {
    check("calligraphy", "calligraphy.myb");
}

#[test]
fn charcoal_horizontal_no_big_gaps() {
    check("charcoal", "charcoal.myb");
}
