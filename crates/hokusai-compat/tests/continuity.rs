//! Property tests for stroke continuity.
//!
//! These assert that a steady left-to-right stroke produces a continuous
//! line — no big white gaps along the stroke axis. They simulate the event
//! cadence a browser PointerEvent stream produces (≈60 Hz with small
//! per-event deltas, occasionally larger), which is where the original
//! report came from.

use std::path::PathBuf;

use hokusai_compat::{load_brush, render_with_finish as render, Script};

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

fn check(name: &str, script: Script) {
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

/// Simulate the kind of event stream a browser pointermove produces during
/// an actual drag: bursts of small deltas with occasional larger jumps when
/// the OS coalesces frames under load.
fn variable_speed_script(brush: &str, y: f32) -> Script {
    let deltas: &[f32] = &[
        4.0, 6.0, 8.0, 12.0, 14.0, 16.0, 18.0, 22.0, // accelerate
        24.0, 22.0, 20.0, 16.0, // peak then ease
        12.0, 14.0, 10.0, 8.0, 6.0, // settle
        4.0, 5.0, 6.0, 7.0, 30.0, // small skip-burst (coalesce)
        20.0, 16.0, 12.0, 8.0, 5.0, 4.0, 3.0, 2.0, // wind down
    ];
    let dt: f32 = 0.016;
    let mut events = Vec::with_capacity(deltas.len() + 1);
    let mut x = 20.0;
    events.push([x, y, 1.0, dt]);
    for &d in deltas {
        x += d;
        events.push([x, y, 1.0, dt]);
    }
    Script {
        brush: brush_path(brush),
        width: (x as u32) + 40,
        height: 80,
        events,
    }
}

#[test]
fn marker_fat_horizontal_no_big_gaps() {
    check(
        "marker_fat",
        horizontal_script("marker_fat.myb", 40.0, 8.0, 0.016, 80),
    );
}

#[test]
fn calligraphy_horizontal_no_big_gaps() {
    check(
        "calligraphy",
        horizontal_script("calligraphy.myb", 40.0, 8.0, 0.016, 80),
    );
}

#[test]
fn charcoal_horizontal_no_big_gaps() {
    check(
        "charcoal",
        horizontal_script("charcoal.myb", 40.0, 8.0, 0.016, 80),
    );
}

#[test]
fn marker_fat_variable_speed_no_big_gaps() {
    check(
        "marker_fat (var)",
        variable_speed_script("marker_fat.myb", 40.0),
    );
}

#[test]
fn calligraphy_variable_speed_no_big_gaps() {
    check(
        "calligraphy (var)",
        variable_speed_script("calligraphy.myb", 40.0),
    );
}

#[test]
fn charcoal_variable_speed_no_big_gaps() {
    check(
        "charcoal (var)",
        variable_speed_script("charcoal.myb", 40.0),
    );
}

#[test]
fn brush_variable_speed_no_big_gaps() {
    check("brush (var)", variable_speed_script("brush.myb", 40.0));
}

// Cover a wide grid of constant speeds — the live demo with mouse can fall
// anywhere in this range depending on user motion. Catches gaps the single
// "average" speed test would miss.
fn constant_grid(brush: &str, dx_per_ev: f32, dt: f32, n: usize) -> Script {
    horizontal_script(brush, 40.0, dx_per_ev, dt, n)
}

#[test]
fn marker_fat_constant_speed_grid() {
    // 4 / 8 / 16 / 32 px per 16 ms event — slow drag, normal, fast, flick.
    for (label, dx) in [("4px", 4.0), ("8px", 8.0), ("16px", 16.0), ("32px", 32.0)] {
        check(
            &format!("marker_fat {label}"),
            constant_grid("marker_fat.myb", dx, 0.016, 80),
        );
    }
}

#[test]
fn calligraphy_constant_speed_grid() {
    for (label, dx) in [("4px", 4.0), ("8px", 8.0), ("16px", 16.0), ("32px", 32.0)] {
        check(
            &format!("calligraphy {label}"),
            constant_grid("calligraphy.myb", dx, 0.016, 80),
        );
    }
}

#[test]
fn charcoal_constant_speed_grid() {
    for (label, dx) in [("4px", 4.0), ("8px", 8.0), ("16px", 16.0), ("32px", 32.0)] {
        check(
            &format!("charcoal {label}"),
            constant_grid("charcoal.myb", dx, 0.016, 80),
        );
    }
}

#[test]
fn brush_constant_speed_grid() {
    for (label, dx) in [("4px", 4.0), ("8px", 8.0), ("16px", 16.0), ("32px", 32.0)] {
        check(
            &format!("brush {label}"),
            constant_grid("brush.myb", dx, 0.016, 80),
        );
    }
}

/// Browser `getCoalescedEvents()` replays sub-events with their own (small)
/// timestamps. Simulate it: each "frame" produces 4 sub-events at 4 ms each,
/// 2 px apart — totalling 8 px / 16 ms (a 500 px/s drag) but plumbed
/// through the engine as 4 short events instead of one full one.
fn coalesced_script(brush: &str, y: f32, sub_dt: f32, sub_dx: f32, sub_n: usize) -> Script {
    let mut events = Vec::with_capacity(sub_n + 1);
    let mut x = 20.0;
    // Initial seed.
    events.push([x, y, 1.0, sub_dt]);
    for _ in 0..sub_n {
        x += sub_dx;
        events.push([x, y, 1.0, sub_dt]);
    }
    Script {
        brush: brush_path(brush),
        width: (x as u32) + 40,
        height: 80,
        events,
    }
}

#[test]
fn marker_fat_coalesced_short_dt() {
    // 4 sub-events / frame × 80 frames = 320 events of 4 ms / 2 px.
    check(
        "marker_fat (coalesced)",
        coalesced_script("marker_fat.myb", 40.0, 0.004, 2.0, 320),
    );
}

#[test]
fn charcoal_coalesced_short_dt() {
    check(
        "charcoal (coalesced)",
        coalesced_script("charcoal.myb", 40.0, 0.004, 2.0, 320),
    );
}

#[test]
fn brush_coalesced_short_dt() {
    check(
        "brush (coalesced)",
        coalesced_script("brush.myb", 40.0, 0.004, 2.0, 320),
    );
}

/// Render at a forced larger-than-designed radius, mimicking a user who
/// dragged the demo's size slider up. Elliptical brushes have to stay
/// continuous at any size — the brush's anti_aliasing won't be enough on
/// its own at large radius, so the dab-density elongation correction must
/// kick in.
fn check_oversized(name: &str, brush: &str, radius_log: f32) {
    use hokusai_core::BrushSetting;
    let dx = 12.0_f32;
    let dt: f32 = 0.016;
    let start_x = 30.0_f32;
    let mut events = Vec::with_capacity(81);
    for i in 0..=80 {
        events.push([start_x + dx * i as f32, 60.0, 1.0, dt]);
    }
    let script = Script {
        brush: brush_path(brush),
        width: (start_x as u32) + (dx as u32) * 80 + 40,
        height: 120,
        events,
    };
    let mut brush_obj = load_brush(&script.brush).unwrap();
    brush_obj.get_mut(BrushSetting::Radius).base_value = radius_log;
    let pixels = render(&brush_obj, &script);
    let gap = longest_white_run(&pixels, script.width, 50, 20, 50..(script.width - 50));
    assert!(
        gap.longest <= MAX_GAP_PX,
        "{name}: longest white run = {} px at x={} (allowed ≤ {MAX_GAP_PX})",
        gap.longest,
        gap.longest_start,
    );
}

#[test]
fn calligraphy_large_radius_no_big_gaps() {
    // 2^4 = 16 px radius — well above the brush's designed 4 px.
    check_oversized("calligraphy (large)", "calligraphy.myb", 4.0);
}

#[test]
fn marker_fat_large_radius_no_big_gaps() {
    check_oversized("marker_fat (large)", "marker_fat.myb", 4.5);
}
