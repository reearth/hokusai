//! Stroke engine — port of libmypaint's `mypaint_brush_stroke_to`.
//!
//! Pipeline per event:
//! 1. If the stroke is fresh (or `dtime ≥ 5 s`), seed state and return.
//! 2. Compute raw event delta, speed, direction.
//! 3. Build [`InputValues`] and evaluate every setting via [`evaluate`].
//! 4. Apply `slow_tracking` to advance `state.actual_x/y` toward the event.
//! 5. Accumulate fractional dabs (`dabs_per_*` + time term) along the
//!    advanced segment; emit each dab, drifting HSV per-dab via
//!    `change_color_*`, and updating the smudge bucket from the canvas.
//! 6. Commit final event state.
//!
//! Still deferred (`TODO(M2-followup-2)`): `tracking_noise`, `attack`,
//! `stroke_holdtime`, `speed1_slowness`/`speed2_slowness` low-pass,
//! `offset_by_random`/`offset_by_speed`, `stroke_threshold`,
//! tilt-derived inputs, recursive custom input.

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

        // --- Fresh stroke: seed state, no dabs ------------------------------
        if !state.started || dtime >= 5.0 {
            state.last_event_x = x;
            state.last_event_y = y;
            state.last_event_time += dtime;
            state.actual_x = x;
            state.actual_y = y;
            state.last_dab_x = x;
            state.last_dab_y = y;
            state.dist_past_dab = 0.0;
            // Seed dynamic color from the brush's base color so per-dab drift
            // has somewhere to start.
            state.actual_h = self.get(BrushSetting::ColorH).base_value;
            state.actual_s = self.get(BrushSetting::ColorS).base_value;
            state.actual_v = self.get(BrushSetting::ColorV).base_value;
            state.smudge_ra = 0.0;
            state.smudge_ga = 0.0;
            state.smudge_ba = 0.0;
            state.smudge_a = 0.0;
            state.norm_speed1_slow = 0.0;
            state.norm_speed2_slow = 0.0;
            state.stroke_total_painting_time = 0.0;
            state.stroke_current_idling_time = 0.0;
            state.started = true;
            return false;
        }

        // --- Event delta (raw, for speed / direction inputs) ----------------
        let dx_raw = x - state.last_event_x;
        let dy_raw = y - state.last_event_y;
        let dt = dtime.max(0.0001) as f32;
        let dist_raw = (dx_raw * dx_raw + dy_raw * dy_raw).sqrt();
        let raw_speed = dist_raw / dt;

        // --- Speed slowness: low-pass filter the raw speed for both bands ---
        // libmypaint reads slowness from the brush base values (these settings
        // rarely receive input mappings, so the simplification is exact in
        // practice and avoids the chicken-and-egg of speed-feeding-itself).
        let slow1 = self
            .get(BrushSetting::Speed1Slowness)
            .base_value
            .clamp(0.0, 1.0);
        let slow2 = self
            .get(BrushSetting::Speed2Slowness)
            .base_value
            .clamp(0.0, 1.0);
        let alpha1 = 1.0 - (1.0 - slow1).powf((dt * 60.0).max(1e-3));
        let alpha2 = 1.0 - (1.0 - slow2).powf((dt * 60.0).max(1e-3));
        state.norm_speed1_slow += (raw_speed - state.norm_speed1_slow) * (1.0 - alpha1);
        state.norm_speed2_slow += (raw_speed - state.norm_speed2_slow) * (1.0 - alpha2);

        // --- Stroke progress: 0..1 across `stroke_duration_logarithmic` ----
        // dur_log is log2(seconds), so `2^dur_log` is the configured length.
        let dur_log = self.get(BrushSetting::StrokeDurationLogarithmic).base_value;
        let dur = dur_log.exp2().max(0.01);
        let stroke_progress = (state.stroke_total_painting_time as f32 / dur).clamp(0.0, 1.0);

        // --- Tilt-derived inputs --------------------------------------------
        // xtilt, ytilt come in normalized to [-1, 1] from Wacom-style devices.
        // declination = asin(|tilt|) → 0° straight up, 90° lying flat.
        // ascension   = atan2(ytilt, xtilt) → rotation around the up axis.
        let tilt_mag = (xtilt * xtilt + ytilt * ytilt).sqrt().min(1.0);
        let tilt_declination = tilt_mag.asin().to_degrees();
        let tilt_ascension = if xtilt == 0.0 && ytilt == 0.0 {
            0.0
        } else {
            ytilt.atan2(xtilt).to_degrees()
        };

        // --- Build input vector ---------------------------------------------
        let mut inputs = InputValues::new();
        inputs.set(BrushInput::Pressure, pressure);
        inputs.set(BrushInput::Speed1, log_speed(state.norm_speed1_slow));
        inputs.set(BrushInput::Speed2, log_speed(state.norm_speed2_slow));
        inputs.set(BrushInput::Random, state.rng.next_unit());
        inputs.set(BrushInput::Stroke, stroke_progress);
        inputs.set(BrushInput::Attack, stroke_progress);
        inputs.set(BrushInput::Direction, direction_input(dx_raw, dy_raw));
        inputs.set(BrushInput::DirectionAngle, direction_angle(dx_raw, dy_raw));
        inputs.set(BrushInput::Tilt, tilt_mag);
        inputs.set(BrushInput::TiltDeclination, tilt_declination);
        inputs.set(BrushInput::TiltAscension, tilt_ascension);
        let sv = evaluate(self, &inputs);

        // --- Resolve actual radius ------------------------------------------
        let radius = sv.get(BrushSetting::Radius).exp2().max(0.1);
        state.actual_radius = radius;

        // --- Slow tracking: advance smoothed position toward the event ------
        // libmypaint multiplies the lag factor by dtime so it stabilises at
        // 60 Hz regardless of event rate.
        let slow = sv.get(BrushSetting::SlowTracking).clamp(0.0, 1.0);
        let approach = (1.0 - slow).powf((dt * 60.0).max(1e-3));
        let prev_actual_x = state.actual_x;
        let prev_actual_y = state.actual_y;
        let mut new_actual_x = prev_actual_x + (x - prev_actual_x) * approach;
        let mut new_actual_y = prev_actual_y + (y - prev_actual_y) * approach;

        // --- Tracking noise: gaussian jitter on the smoothed position -------
        let noise = sv.get(BrushSetting::TrackingNoise).max(0.0);
        if noise > 0.0 {
            new_actual_x += state.rng.next_gauss() * noise * radius;
            new_actual_y += state.rng.next_gauss() * noise * radius;
        }
        let dx = new_actual_x - prev_actual_x;
        let dy = new_actual_y - prev_actual_y;
        let dist = (dx * dx + dy * dy).sqrt();

        // --- Stroke threshold: suppress dabs below a pressure floor --------
        let threshold = sv.get(BrushSetting::StrokeThreshold).max(0.0);
        let below_threshold = pressure < threshold;

        // --- Dab count along the smoothed segment ---------------------------
        let dpar = sv.get(BrushSetting::DabsPerActualRadius).max(0.0);
        let dpbr = sv.get(BrushSetting::DabsPerBasicRadius).max(0.0);
        let dps = sv.get(BrushSetting::DabsPerSecond).max(0.0);
        let dist_dabs = if radius > 0.0 {
            dist * (dpar + dpbr) / (radius * 2.0)
        } else {
            0.0
        };
        let total_dabs = dist_dabs + dps * dt;
        let mut accumulated = state.dist_past_dab + total_dabs;
        let mut painted = false;

        if accumulated >= 1.0 && !below_threshold {
            let n = accumulated.floor().min(10_000.0) as u32;
            let dt_per_dab = dt / n.max(1) as f32;
            let smudge_amt = sv.get(BrushSetting::Smudge).clamp(0.0, 1.0);
            let smudge_length = sv.get(BrushSetting::SmudgeLength).clamp(0.0, 1.0);
            let smudge_radius = sv.get(BrushSetting::SmudgeRadiusLog).exp2().max(1.0);
            let off_random = sv.get(BrushSetting::OffsetByRandom).max(0.0);
            let off_speed = sv.get(BrushSetting::OffsetBySpeed).max(0.0);
            // Unit vector along motion, used by offset_by_speed.
            let (ux, uy) = if dist > 1e-6 {
                (dx / dist, dy / dist)
            } else {
                (0.0, 0.0)
            };

            for i in 1..=n {
                let frac = (i as f32 - (accumulated - n as f32)) / total_dabs.max(1e-6);
                let mut px = prev_actual_x + dx * frac;
                let mut py = prev_actual_y + dy * frac;

                if off_random > 0.0 {
                    px += state.rng.next_gauss() * off_random * radius;
                    py += state.rng.next_gauss() * off_random * radius;
                }
                if off_speed > 0.0 {
                    // libmypaint pushes the dab along the motion direction
                    // proportional to the slowed speed, normalised so that
                    // unit setting × unit-radius brush gives a one-radius
                    // displacement at moderate speeds.
                    let mag = (state.norm_speed1_slow * 0.04).clamp(-4.0, 4.0);
                    px += ux * off_speed * radius * mag;
                    py += uy * off_speed * radius * mag;
                }

                // Smudge: refresh bucket from canvas, weighted by smudge_length.
                if smudge_amt > 0.0 {
                    let sample = surface.get_color(px, py, smudge_radius);
                    let fac = smudge_length; // 1.0 = keep bucket, 0.0 = always refresh
                    state.smudge_ra = state.smudge_ra * fac + sample.r * sample.a * (1.0 - fac);
                    state.smudge_ga = state.smudge_ga * fac + sample.g * sample.a * (1.0 - fac);
                    state.smudge_ba = state.smudge_ba * fac + sample.b * sample.a * (1.0 - fac);
                    state.smudge_a = state.smudge_a * fac + sample.a * (1.0 - fac);
                }

                // Color drift between dabs (change_color_*).
                drift_color(state, &sv, dt_per_dab);

                let dab = build_dab(&sv, state, px, py, smudge_amt);
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
        state.actual_x = new_actual_x;
        state.actual_y = new_actual_y;
        if painted {
            state.stroke_total_painting_time += dtime;
        } else {
            state.stroke_current_idling_time += dtime;
        }

        painted
    }
}

fn drift_color(state: &mut BrushState, sv: &SettingValues, dt_per_dab: f32) {
    // libmypaint scales the drift by 0.05 per dab — keeps "tiny constant"
    // values usable.
    let k = 0.05 * dt_per_dab * 60.0;
    state.actual_h = (state.actual_h + sv.get(BrushSetting::ChangeColorH) * k).rem_euclid(1.0);
    state.actual_v = (state.actual_v + sv.get(BrushSetting::ChangeColorV) * k).clamp(0.0, 1.0);
    state.actual_s = (state.actual_s + sv.get(BrushSetting::ChangeColorHsvS) * k).clamp(0.0, 1.0);
    // change_color_hsl_s and change_color_l would need HSL math; deferred.
}

fn build_dab(sv: &SettingValues, state: &BrushState, px: f32, py: f32, smudge_amt: f32) -> Dab {
    // Base color from the (drifted) HSV state.
    let base = hsv_to_rgb(Hsv {
        h: state.actual_h,
        s: state.actual_s,
        v: state.actual_v,
    });
    // Mix in smudge bucket (already premultiplied by sampled alpha).
    let bucket_a = state.smudge_a.max(0.0001);
    let mix_r = (state.smudge_ra / bucket_a).clamp(0.0, 1.0);
    let mix_g = (state.smudge_ga / bucket_a).clamp(0.0, 1.0);
    let mix_b = (state.smudge_ba / bucket_a).clamp(0.0, 1.0);
    let color = crate::color::RgbaF32 {
        r: base.r * (1.0 - smudge_amt) + mix_r * smudge_amt,
        g: base.g * (1.0 - smudge_amt) + mix_g * smudge_amt,
        b: base.b * (1.0 - smudge_amt) + mix_b * smudge_amt,
        a: 1.0,
    };

    let opaque = sv.get(BrushSetting::Opaque).clamp(0.0, 2.0);
    let hardness = sv.get(BrushSetting::Hardness).clamp(0.0, 1.0);
    let eraser = sv.get(BrushSetting::Eraser).clamp(0.0, 1.0);
    let alpha_eraser = 1.0 - eraser;

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

fn log_speed(raw: f32) -> f32 {
    0.5 * (0.01 + raw).log10()
}

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
            unreachable!()
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
        let brush = make_brush(1.0, 2.0);
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        brush.stroke_to(&mut state, &mut surf, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        brush.stroke_to(&mut state, &mut surf, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        assert!(
            (8..=11).contains(&surf.count),
            "expected ~10 dabs, got {}",
            surf.count
        );
    }

    #[test]
    fn slow_tracking_smooths_position() {
        // High slow_tracking → fewer pixels covered → fewer dabs.
        let a = make_brush(1.0, 2.0);
        let mut b = make_brush(1.0, 2.0);
        b.set(BrushSetting::SlowTracking, SettingValue::constant(0.9));

        let mut sa = BrushState::default();
        let mut sb = BrushState::default();
        let mut surf_a = CountingSurface { count: 0 };
        let mut surf_b = CountingSurface { count: 0 };

        a.stroke_to(&mut sa, &mut surf_a, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        a.stroke_to(&mut sa, &mut surf_a, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        b.stroke_to(&mut sb, &mut surf_b, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        b.stroke_to(&mut sb, &mut surf_b, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01);

        assert!(
            surf_b.count < surf_a.count,
            "slow_tracking should suppress dab count: {} >= {}",
            surf_b.count,
            surf_a.count
        );
    }

    #[test]
    fn stroke_threshold_suppresses_low_pressure_dabs() {
        let mut brush = make_brush(1.0, 2.0);
        brush.set(BrushSetting::StrokeThreshold, SettingValue::constant(0.5));
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        brush.stroke_to(&mut state, &mut surf, 0.0, 0.0, 0.3, 0.0, 0.0, 0.01);
        brush.stroke_to(&mut state, &mut surf, 20.0, 0.0, 0.3, 0.0, 0.0, 0.01);
        assert_eq!(surf.count, 0, "pressure 0.3 should be below threshold 0.5");

        // Same motion, pressure above threshold: dabs land.
        let mut state2 = BrushState::default();
        let mut surf2 = CountingSurface { count: 0 };
        brush.stroke_to(&mut state2, &mut surf2, 0.0, 0.0, 0.8, 0.0, 0.0, 0.01);
        brush.stroke_to(&mut state2, &mut surf2, 20.0, 0.0, 0.8, 0.0, 0.0, 0.01);
        assert!(surf2.count > 0, "pressure 0.8 should produce dabs");
    }

    #[test]
    fn tracking_noise_shifts_dab_positions() {
        // Same input + seed should be deterministic. Two states with the same
        // seed differ in the dab positions iff tracking_noise injects gauss.
        let noise_brush = {
            let mut b = make_brush(1.0, 2.0);
            b.set(BrushSetting::TrackingNoise, SettingValue::constant(0.5));
            b
        };
        let plain = make_brush(1.0, 2.0);

        struct CaptureSurface {
            xs: Vec<f32>,
        }
        impl TiledSurface for CaptureSurface {
            fn tile_request_start(&mut self, _: i32, _: i32) -> &mut crate::tile::TilePixels {
                unreachable!()
            }
            fn tile_request_end(&mut self, _: i32, _: i32) {}
            fn draw_dab(&mut self, d: &Dab) -> bool {
                self.xs.push(d.x);
                true
            }
        }

        let mut sa = BrushState::default();
        let mut sb = BrushState::default();
        let mut ca = CaptureSurface { xs: vec![] };
        let mut cb = CaptureSurface { xs: vec![] };

        plain.stroke_to(&mut sa, &mut ca, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        plain.stroke_to(&mut sa, &mut ca, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        noise_brush.stroke_to(&mut sb, &mut cb, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        noise_brush.stroke_to(&mut sb, &mut cb, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01);

        // Noise perturbs the segment length so dab counts can differ by one.
        // Compare overlapping prefixes — any difference proves noise applied.
        let any_differ = ca
            .xs
            .iter()
            .zip(cb.xs.iter())
            .any(|(a, b)| (a - b).abs() > 1e-3);
        assert!(
            any_differ || ca.xs.len() != cb.xs.len(),
            "tracking_noise should perturb the dab stream"
        );
    }

    #[test]
    fn speed_slowness_smooths_speed_input() {
        // High slowness → speed1_slow stays near 0 even after rapid event.
        let mut b = make_brush(1.0, 2.0);
        b.set(BrushSetting::Speed1Slowness, SettingValue::constant(0.99));
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        b.stroke_to(&mut state, &mut surf, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        b.stroke_to(&mut state, &mut surf, 200.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        let smoothed = state.norm_speed1_slow;

        let b2 = make_brush(1.0, 2.0); // slowness = 0 (default)
        let mut state2 = BrushState::default();
        let mut surf2 = CountingSurface { count: 0 };
        b2.stroke_to(&mut state2, &mut surf2, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        b2.stroke_to(&mut state2, &mut surf2, 200.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        let raw = state2.norm_speed1_slow;

        assert!(
            smoothed < raw,
            "slowness should suppress speed1_slow ({smoothed} >= {raw})"
        );
    }

    #[test]
    fn tilt_declination_is_zero_when_pen_upright() {
        let brush = make_brush(1.0, 2.0);
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        brush.stroke_to(&mut state, &mut surf, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        brush.stroke_to(&mut state, &mut surf, 10.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        // We don't store tilt directly; check via a brush that maps it.
        let mut tilt_brush = make_brush(1.0, 2.0);
        tilt_brush.set(
            BrushSetting::Radius,
            SettingValue {
                base_value: 1.0,
                inputs: vec![crate::mapping::InputMapping {
                    input: BrushInput::TiltDeclination,
                    points: vec![(0.0, 0.0), (90.0, 1.0)],
                }],
            },
        );
        let mut s1 = BrushState::default();
        let mut surf1 = CountingSurface { count: 0 };
        tilt_brush.stroke_to(&mut s1, &mut surf1, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        tilt_brush.stroke_to(&mut s1, &mut surf1, 10.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        let r_upright = s1.actual_radius;

        let mut s2 = BrushState::default();
        let mut surf2 = CountingSurface { count: 0 };
        tilt_brush.stroke_to(&mut s2, &mut surf2, 0.0, 0.0, 1.0, 1.0, 0.0, 0.01);
        tilt_brush.stroke_to(&mut s2, &mut surf2, 10.0, 0.0, 1.0, 1.0, 0.0, 0.01);
        let r_tilted = s2.actual_radius;

        assert!(
            r_tilted > r_upright,
            "tilted pen should map to bigger radius via declination: {r_tilted} <= {r_upright}"
        );
    }

    #[test]
    fn change_color_h_drifts_hue() {
        let mut brush = make_brush(1.0, 2.0);
        brush.set(BrushSetting::ChangeColorH, SettingValue::constant(0.5));
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        brush.stroke_to(&mut state, &mut surf, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        let h0 = state.actual_h;
        brush.stroke_to(&mut state, &mut surf, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        let h1 = state.actual_h;
        assert!((h1 - h0).abs() > 0.01, "hue should drift, h0={h0}, h1={h1}");
    }
}
