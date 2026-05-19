//! Stroke engine — port of libmypaint's `mypaint_brush_stroke_to`.
//!
//! This is a minimal but structurally faithful version:
//! - Event delta → raw speed and direction
//! - Build [`InputValues`], evaluate every setting
//! - Accumulate fractional dab count from `dabs_per_actual_radius`,
//!   `dabs_per_basic_radius`, and `dabs_per_second`
//! - Emit dabs along the segment with linear interpolation
//! - Update [`BrushState`] for the next event
//!
//! Intentionally **not yet** implemented (`TODO(M2-followup)`):
//! - `slow_tracking` / `slow_tracking_per_dab` smoothing
//! - `tracking_noise`, `attack`, `stroke_holdtime`
//! - Speed low-pass filter (`speed1_slowness`, `speed2_slowness`)
//! - `offset_by_random`, `offset_by_speed`
//! - `change_color_*` HSV drift between dabs
//! - Smudge bucket update (needs `get_color` from M3)
//! - `stroke_threshold` skip
//! - `direction_filter`, tilt-derived inputs
//! - Custom input (recursive setting evaluation)

use crate::brush::Brush;
use crate::color::{hsv_to_rgb, Hsv};
use crate::evaluator::{evaluate, InputValues, SettingValues};
use crate::setting::BrushSetting;
use crate::state::BrushState;
use crate::surface::{Dab, TiledSurface};
use crate::BrushInput;

impl Brush {
    /// Feed one pointer event. `dtime` is seconds since the previous call.
    ///
    /// Returns `true` if at least one dab was painted.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_to<S: TiledSurface>(
        &self,
        state: &mut BrushState,
        surface: &mut S,
        x: f32,
        y: f32,
        pressure: f32,
        xtilt: f32,
        ytilt: f32,
        dtime: f64,
    ) -> bool {
        let pressure = pressure.clamp(0.0, 1.0);

        // --- Fresh stroke: seed position, no dabs ---------------------------
        // Matches libmypaint's behaviour after `mypaint_brush_reset` — the
        // first event of a new stroke only sets state. Also kick in when the
        // caller signals "long pause" via dtime ≥ 5 s.
        if !state.started || dtime >= 5.0 {
            state.last_event_x = x;
            state.last_event_y = y;
            state.last_event_time += dtime;
            state.actual_x = x;
            state.actual_y = y;
            state.last_dab_x = x;
            state.last_dab_y = y;
            state.dist_past_dab = 0.0;
            state.started = true;
            return false;
        }

        // --- Event delta -----------------------------------------------------
        let dx = x - state.last_event_x;
        let dy = y - state.last_event_y;
        let dt = dtime.max(0.0001) as f32;
        let dist = (dx * dx + dy * dy).sqrt();
        let raw_speed = dist / dt;

        // --- Build input vector ----------------------------------------------
        // Raw speeds for now; the slowness low-pass goes in M2-followup.
        let mut inputs = InputValues::new();
        inputs.set(BrushInput::Pressure, pressure);
        inputs.set(BrushInput::Speed1, log_speed(raw_speed));
        inputs.set(BrushInput::Speed2, log_speed(raw_speed));
        inputs.set(BrushInput::Random, state.rng.next_unit());
        inputs.set(BrushInput::Stroke, 0.0); // TODO: stroke_duration_logarithmic
        inputs.set(BrushInput::Direction, direction_input(dx, dy));
        inputs.set(BrushInput::DirectionAngle, direction_angle(dx, dy));
        inputs.set(BrushInput::Tilt, (xtilt * xtilt + ytilt * ytilt).sqrt());
        inputs.set(BrushInput::TiltDeclination, 0.0); // TODO
        inputs.set(BrushInput::TiltAscension, 0.0); // TODO
        inputs.set(BrushInput::Attack, 0.0); // TODO
        inputs.set(BrushInput::Custom, 0.0); // TODO (recursive)
        inputs.set(BrushInput::GridmapX, 0.0); // TODO
        inputs.set(BrushInput::GridmapY, 0.0); // TODO

        let sv = evaluate(self, &inputs);

        // --- Resolve actual radius (libmypaint stores it as log2(px)) --------
        let radius_log = sv.get(BrushSetting::Radius);
        let radius = radius_log.exp2().max(0.1);
        state.actual_radius = radius;

        // --- How many dabs along this segment? -------------------------------
        // libmypaint: distance_to_step = radius * 2 / dabs_per_actual_radius
        //             + radius_base * 2 / dabs_per_basic_radius
        //             time term:  dabs_per_second * dt
        let dpar = sv.get(BrushSetting::DabsPerActualRadius).max(0.0);
        let dpbr = sv.get(BrushSetting::DabsPerBasicRadius).max(0.0);
        let dps = sv.get(BrushSetting::DabsPerSecond).max(0.0);

        let dist_dabs = if radius > 0.0 {
            (dist * dpar) / (radius * 2.0) + (dist * dpbr) / (radius * 2.0)
        } else {
            0.0
        };
        let time_dabs = dps * dt;
        let total_dabs = dist_dabs + time_dabs;

        let mut painted = false;
        let mut accumulated = state.dist_past_dab + total_dabs;

        if accumulated >= 1.0 {
            let mut n = accumulated.floor() as u32;
            // Defensive cap — runaway settings shouldn't OOM the surface.
            if n > 10_000 {
                n = 10_000;
            }
            for i in 1..=n {
                // Fractional position along the segment where this dab lands.
                let frac = (i as f32 - (accumulated - n as f32)) / total_dabs.max(1e-6);
                let dab = build_dab(&sv, state, x, y, dx, dy, frac);
                if surface.draw_dab(&dab) {
                    painted = true;
                }
                state.last_dab_x = dab.x;
                state.last_dab_y = dab.y;
            }
            accumulated -= n as f32;
        }
        state.dist_past_dab = accumulated;

        // --- Commit event state ---------------------------------------------
        state.last_event_x = x;
        state.last_event_y = y;
        state.last_event_time += dtime;
        state.actual_x = x;
        state.actual_y = y;
        if painted {
            state.stroke_total_painting_time += dtime;
        } else {
            state.stroke_current_idling_time += dtime;
        }

        painted
    }
}

fn build_dab(
    sv: &SettingValues,
    state: &BrushState,
    x: f32,
    y: f32,
    dx: f32,
    dy: f32,
    frac: f32,
) -> Dab {
    let px = state.last_event_x + dx * frac;
    let py = state.last_event_y + dy * frac;

    let color = hsv_to_rgb(Hsv {
        h: sv.get(BrushSetting::ColorH),
        s: sv.get(BrushSetting::ColorS),
        v: sv.get(BrushSetting::ColorV),
    });
    let opaque = sv.get(BrushSetting::Opaque).clamp(0.0, 2.0);
    let hardness = sv.get(BrushSetting::Hardness).clamp(0.0, 1.0);
    let eraser = sv.get(BrushSetting::Eraser).clamp(0.0, 1.0);
    let alpha_eraser = 1.0 - eraser;

    let _ = (x, y); // x/y are the event endpoint; we use interpolated px,py.

    Dab {
        x: px,
        y: py,
        radius: state.actual_radius,
        color,
        opaque,
        hardness,
        alpha_eraser,
        aspect_ratio: sv.get(BrushSetting::EllipticalDabRatio).max(1.0),
        angle: sv.get(BrushSetting::EllipticalDabAngle),
        lock_alpha: sv.get(BrushSetting::LockAlpha).clamp(0.0, 1.0),
        colorize: sv.get(BrushSetting::Colorize).clamp(0.0, 1.0),
        posterize: sv.get(BrushSetting::Posterize).clamp(0.0, 1.0),
        posterize_num: sv.get(BrushSetting::PosterizeNum).max(1.0),
        paint: sv.get(BrushSetting::Paint).clamp(0.0, 1.0),
        anti_aliasing: sv.get(BrushSetting::AntiAliasing).clamp(0.0, 1.0),
    }
}

/// libmypaint maps speed via log10-ish scaling for `speed1`/`speed2`.
fn log_speed(raw: f32) -> f32 {
    // `0.5 * log10(0.01 + speed)` keeps everyday speeds near 0..4.
    // The exact constants come from `helpers.c` and will be tightened in M3.
    0.5 * (0.01 + raw).log10()
}

/// Direction in [0, 1) — angle of segment normalised by 360°.
fn direction_input(dx: f32, dy: f32) -> f32 {
    if dx == 0.0 && dy == 0.0 {
        0.0
    } else {
        (dy.atan2(dx) / (2.0 * std::f32::consts::PI)).rem_euclid(1.0)
    }
}

fn direction_angle(dx: f32, dy: f32) -> f32 {
    if dx == 0.0 && dy == 0.0 {
        0.0
    } else {
        dy.atan2(dx).to_degrees()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::SettingValue;

    struct CountingSurface {
        count: u32,
    }
    impl TiledSurface for CountingSurface {
        fn tile_request_start(&mut self, _tx: i32, _ty: i32) -> &mut crate::tile::TilePixels {
            unreachable!("not used: draw_dab is stubbed in M1")
        }
        fn tile_request_end(&mut self, _tx: i32, _ty: i32) {}
        fn draw_dab(&mut self, _dab: &Dab) -> bool {
            self.count += 1;
            true
        }
    }

    fn make_brush(radius_log: f32, dabs_per_actual_radius: f32) -> Brush {
        let mut b = Brush::new();
        b.set(BrushSetting::Radius, SettingValue::constant(radius_log));
        b.set(
            BrushSetting::DabsPerActualRadius,
            SettingValue::constant(dabs_per_actual_radius),
        );
        b.set(BrushSetting::Opaque, SettingValue::constant(1.0));
        b.set(BrushSetting::Hardness, SettingValue::constant(0.5));
        b
    }

    #[test]
    fn no_movement_no_dabs() {
        let brush = make_brush(1.0, 2.0);
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        let painted = brush.stroke_to(&mut state, &mut surf, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        assert!(!painted);
        assert_eq!(surf.count, 0);
    }

    #[test]
    fn moves_emit_proportional_dabs() {
        // radius = 2^1 = 2 px. dpar = 2 → dab every 2 px.
        // Move 20 px → expect ~10 dabs.
        let brush = make_brush(1.0, 2.0);
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        // First call seeds last_event_*; no distance yet.
        brush.stroke_to(&mut state, &mut surf, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        brush.stroke_to(&mut state, &mut surf, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        assert!(
            (8..=11).contains(&surf.count),
            "expected ~10 dabs, got {}",
            surf.count
        );
    }
}
