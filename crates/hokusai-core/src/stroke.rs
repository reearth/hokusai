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
        // Capture the pressure value from the previous event *before* we
        // overwrite it: dabs emitted inside this stroke segment interpolate
        // pressure linearly along the segment, the way libmypaint advances
        // STATE.PRESSURE inside its `while (dabs_moved + dabs_todo >= 1.0)`
        // loop. Without this carry, every dab uses the event's final pressure
        // and pressure-driven dynamics (radius, opacity, …) jump in steps.
        let entry_pressure = if state.started {
            state.last_pressure
        } else {
            pressure
        };
        state.last_pressure = pressure;

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
        // libmypaint treats `speedN_slowness` as a time constant in seconds
        // and applies `fac = 1 - exp(-dt / slow)`. We read the base value
        // (these settings rarely receive input mappings, so the
        // simplification is exact in practice and avoids the chicken-and-egg
        // of speed-feeding-itself).
        let slow1 = self.get(BrushSetting::Speed1Slowness).base_value.max(0.0);
        let slow2 = self.get(BrushSetting::Speed2Slowness).base_value.max(0.0);
        let fac1 = if slow1 > 1e-3 {
            1.0 - (-dt / slow1).exp()
        } else {
            1.0
        };
        let fac2 = if slow2 > 1e-3 {
            1.0 - (-dt / slow2).exp()
        } else {
            1.0
        };
        state.norm_speed1_slow += (raw_speed - state.norm_speed1_slow) * fac1;
        state.norm_speed2_slow += (raw_speed - state.norm_speed2_slow) * fac2;

        // --- Stroke progress: 0..1 across `stroke_duration_logarithmic` ----
        // libmypaint stores duration as `ln(seconds)`, so `exp(dur_log)` is
        // the configured length in seconds.
        let dur_log = self.get(BrushSetting::StrokeDurationLogarithmic).base_value;
        let dur = dur_log.exp().max(0.01);
        let stroke_progress = (state.stroke_total_painting_time as f32 / dur).clamp(0.0, 1.0);

        // --- Tilt-derived inputs --------------------------------------------
        // libmypaint convention (mypaint-brush.c):
        //   declination = 90° when the pen is straight up, decreasing as the
        //     pen tilts toward the tablet. Formula: `90 - hypot(xtilt,ytilt) * 60`.
        //   ascension   = `atan2(-xtilt, ytilt)` in degrees.
        // When no tilt is reported, libmypaint leaves declination at 90 and
        // ascension at 0 — anything else makes pressure-only strokes evaluate
        // brushes (e.g. marker_fat) as if the pen were lying flat against the
        // tablet, which feeds wildly wrong radius/aspect values into the
        // curves.
        let (tilt_mag, tilt_declination, tilt_ascension) = if xtilt == 0.0 && ytilt == 0.0 {
            (0.0, 90.0, 0.0)
        } else {
            let m = (xtilt * xtilt + ytilt * ytilt).sqrt().min(1.0);
            (m, 90.0 - m * 60.0, (-xtilt).atan2(ytilt).to_degrees())
        };

        // --- Speed input mapping --------------------------------------------
        // libmypaint maps the smoothed normalised speed through a logarithmic
        // curve with two fix-points anchored at `(speed=45, value=0.5)` and
        // slope `0.015` there:
        //     gamma = exp(speedN_gamma)
        //     m     = 0.015 * (45 + gamma)
        //     q     = 0.5 - m * log(45 + gamma)
        //     value = log(gamma + speed) * m + q
        // The previous `0.5 * log10(0.01 + speed)` shortcut ignored the brush's
        // `speedN_gamma` entirely and used a different curve shape, so brushes
        // whose dynamics ride on `speed1` (calligraphy hardness/radius,
        // marker pressure-vs-speed) ended up with wildly wrong inputs.
        let speed1_input = speed_input(
            state.norm_speed1_slow,
            self.get(BrushSetting::Speed1Gamma).base_value,
        );
        let speed2_input = speed_input(
            state.norm_speed2_slow,
            self.get(BrushSetting::Speed2Gamma).base_value,
        );

        // --- Build input vector ---------------------------------------------
        let mut inputs = InputValues::new();
        inputs.set(BrushInput::Pressure, pressure);
        inputs.set(BrushInput::Speed1, speed1_input);
        inputs.set(BrushInput::Speed2, speed2_input);
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
        // libmypaint's `radius_logarithmic` is stored as `ln(radius)`, so the
        // brush's effective radius in pixels is `exp(value)`. Using `exp2`
        // here previously made every dab ~2.6× smaller than libmypaint's.
        let radius = sv.get(BrushSetting::Radius).exp().max(0.1);
        state.actual_radius = radius;

        // --- Slow tracking: advance smoothed position toward the event ------
        // libmypaint applies an exponential moving average with time
        // constant `0.01 * slow_tracking` seconds (the `0.01` makes the
        // setting's "displayed range" of 0–10 cover ~0–100 ms of lag).
        // Formula: approach = 1 - exp(-dt / (0.01 * slow)).
        let slow = sv.get(BrushSetting::SlowTracking).max(0.0);
        let approach = if slow > 1e-3 {
            1.0 - (-dt / (0.01 * slow)).exp()
        } else {
            1.0
        };
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

        // Elliptical brushes whose major axis is perpendicular to motion
        // expose only their thin minor-axis cross-section along the stroke,
        // so the dab-per-radius rate has to scale up to keep coverage
        // continuous. Compute the dab's projection factor in motion-space:
        //   sqrt(cos²θ_rel + aspect² · sin²θ_rel)
        // (1.0 when aligned with motion, → aspect when perpendicular).
        // Without this, calligraphy at "size +N" leaves the visible
        // slash gaps the user reported.
        let aspect = sv.get(BrushSetting::EllipticalDabRatio).max(1.0);
        let elongation = if aspect > 1.0 && dist > 1e-6 {
            let motion_angle = dy.atan2(dx);
            let dab_angle = sv.get(BrushSetting::EllipticalDabAngle).to_radians();
            let rel = dab_angle - motion_angle;
            (rel.cos().powi(2) + aspect.powi(2) * rel.sin().powi(2)).sqrt()
        } else {
            1.0
        };
        // libmypaint's `count_dabs_to` is `dist_ellipse / radius * DPAR` plus
        // the basic-radius and per-second terms — i.e. `dabs_per_actual_radius`
        // means dabs per *radius* of travel, not per diameter. Dividing by
        // `radius * 2.0` here (as a previous revision did) halved the dab
        // count, producing visible gaps in libmypaint-thin brushes once the
        // tilt_declination fix made radii small enough to expose them.
        let dist_dabs = if radius > 0.0 {
            dist * (dpar + dpbr) * elongation / radius
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
            // libmypaint: `smudge_radius = radius * expf(SMUDGE_RADIUS_LOG)`.
            // The setting is a multiplier in `ln` space, not an absolute size.
            let smudge_radius = (radius * sv.get(BrushSetting::SmudgeRadiusLog).exp()).max(1.0);
            let off_random = sv.get(BrushSetting::OffsetByRandom).max(0.0);
            let off_speed = sv.get(BrushSetting::OffsetBySpeed).max(0.0);
            // Unit vector along motion, used by offset_by_speed.
            let (ux, uy) = if dist > 1e-6 {
                (dx / dist, dy / dist)
            } else {
                (0.0, 0.0)
            };

            // The K-th dab (1-indexed across the stroke's running accumulator)
            // lands at `(K - dist_past_dab_entry) / total_dabs` along the
            // segment, where `dist_past_dab_entry` is the carryover *before*
            // we added total_dabs. Using `accumulated - n` (the post-emit
            // remainder) instead — as a previous revision did — placed dabs
            // outside [0, 1] of the segment and produced visible gaps at
            // event boundaries on real brushes (calligraphy, marker, …).
            let entry_carry = state.dist_past_dab;
            // Per-dab inputs: clone the event-level vector so we only touch
            // the values that interpolate (pressure for now; speed/tilt are
            // approximated as constant across the segment).
            let mut dab_inputs = inputs.clone();
            for i in 1..=n {
                let frac = (i as f32 - entry_carry) / total_dabs.max(1e-6);
                let mut px = prev_actual_x + dx * frac;
                let mut py = prev_actual_y + dy * frac;

                // libmypaint advances STATE.PRESSURE inside the dab loop by
                // `step_dpressure = frac * (pressure - STATE.PRESSURE)`. We
                // achieve the same by interpolating from `entry_pressure` to
                // the event's pressure along the segment fraction.
                let dab_pressure =
                    entry_pressure + (pressure - entry_pressure) * frac.clamp(0.0, 1.0);
                dab_inputs.set(BrushInput::Pressure, dab_pressure);
                let dab_sv = evaluate(self, &dab_inputs);
                let dab_radius = dab_sv.get(BrushSetting::Radius).exp().max(0.1);
                state.actual_radius = dab_radius;

                if off_random > 0.0 {
                    px += state.rng.next_gauss() * off_random * dab_radius;
                    py += state.rng.next_gauss() * off_random * dab_radius;
                }
                if off_speed > 0.0 {
                    // libmypaint pushes the dab along the motion direction
                    // proportional to the slowed speed, normalised so that
                    // unit setting × unit-radius brush gives a one-radius
                    // displacement at moderate speeds.
                    let mag = (state.norm_speed1_slow * 0.04).clamp(-4.0, 4.0);
                    px += ux * off_speed * dab_radius * mag;
                    py += uy * off_speed * dab_radius * mag;
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
                drift_color(state, &dab_sv, dt_per_dab);

                let dab = build_dab(self, &dab_sv, state, px, py, smudge_amt);
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

    /// Flush `slow_tracking` lag and paint the trailing pixels.
    ///
    /// Call this on pointer-up. The smoothed position lags behind the live
    /// cursor by up to `velocity * τ` pixels (where `τ ≈ 0.01 * slow_tracking`
    /// seconds). Without flushing, that trail is left unpainted — so a
    /// stroke ending at x=660 would only have paint up to x≈645 for a brush
    /// with `slow_tracking=3` at 500 px/s.
    ///
    /// Pumps a handful of small idle events at the last cursor position so
    /// the smoothed position catches up. Returns `true` if any pixel was
    /// painted.
    pub fn finish_stroke<S: TiledSurface>(&self, state: &mut BrushState, surface: &mut S) -> bool {
        if !state.started {
            return false;
        }
        let mut painted = false;
        // Up to 8 × 16 ms ≈ 130 ms of catch-up. With τ ≤ 100 ms (slow ≤ 10)
        // that's ≥ 1 time constant, leaving < 37 % residual lag; for typical
        // brushes (slow ≤ 5) it's ≥ 2.5 τ and < 8 %. Pressure is held at the
        // last received value so brushes whose `opaque` is pressure-driven
        // keep painting along the trailing segment.
        let p = state.last_pressure;
        for _ in 0..8 {
            painted |= self.stroke_to(
                state,
                surface,
                state.last_event_x,
                state.last_event_y,
                p,
                0.0,
                0.0,
                0.016,
            );
            let lag = ((state.last_event_x - state.actual_x).powi(2)
                + (state.last_event_y - state.actual_y).powi(2))
            .sqrt();
            if lag < 0.5 {
                break;
            }
        }
        painted
    }
}

fn drift_color(state: &mut BrushState, sv: &SettingValues, dt_per_dab: f32) {
    let k = 0.05 * dt_per_dab * 60.0;
    let dh = sv.get(BrushSetting::ChangeColorH) * k;
    let dv = sv.get(BrushSetting::ChangeColorV) * k;
    let dhsv_s = sv.get(BrushSetting::ChangeColorHsvS) * k;
    let dl = sv.get(BrushSetting::ChangeColorL) * k;
    let dhsl_s = sv.get(BrushSetting::ChangeColorHslS) * k;

    // HSV-side drift first (cheap).
    state.actual_h = (state.actual_h + dh).rem_euclid(1.0);
    state.actual_v = (state.actual_v + dv).clamp(0.0, 1.0);
    state.actual_s = (state.actual_s + dhsv_s).clamp(0.0, 1.0);

    // HSL-side drift: roundtrip via RGB only when the brush actually uses it
    // (almost no brushes do — keep the common path free of the conversion).
    if dl != 0.0 || dhsl_s != 0.0 {
        let rgb = crate::color::hsv_to_rgb(crate::color::Hsv {
            h: state.actual_h,
            s: state.actual_s,
            v: state.actual_v,
        });
        let mut hsl = crate::color::rgb_to_hsl(rgb.r, rgb.g, rgb.b);
        hsl.l = (hsl.l + dl).clamp(0.0, 1.0);
        hsl.s = (hsl.s + dhsl_s).clamp(0.0, 1.0);
        let rgb2 = crate::color::hsl_to_rgb(hsl);
        let hsv2 = crate::color::rgb_to_hsv(rgb2.r, rgb2.g, rgb2.b);
        state.actual_h = hsv2.h;
        state.actual_s = hsv2.s;
        state.actual_v = hsv2.v;
    }
}

/// Returns the effective `opaque_multiply` factor. When the brush leaves
/// the setting wholly at defaults (base 0, no inputs) we use 1.0 so the
/// final opacity matches libmypaint's default behaviour rather than
/// zeroing out every dab.
fn opaque_multiplier(brush: &Brush, sv: &SettingValues) -> f32 {
    let setting = brush.get(BrushSetting::OpaqueMultiply);
    if setting.base_value == 0.0 && setting.inputs.is_empty() {
        return 1.0;
    }
    sv.get(BrushSetting::OpaqueMultiply).clamp(0.0, 1.0)
}

fn build_dab(
    brush: &Brush,
    sv: &SettingValues,
    state: &BrushState,
    px: f32,
    py: f32,
    smudge_amt: f32,
) -> Dab {
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

    // libmypaint composes the final opacity as opaque * opaque_multiply.
    // Many stock brushes (charcoal, pencil, …) drive opaque_multiply from
    // pressure, so skipping it makes them look wrong at non-full pressure.
    // libmypaint defaults opaque_multiply to 1.0; treat a wholly-default
    // setting (no base value and no input curves) as that identity.
    let opaque_raw = sv.get(BrushSetting::Opaque).clamp(0.0, 2.0);
    let opaque_mult = opaque_multiplier(brush, sv);
    let opaque = (opaque_raw * opaque_mult).clamp(0.0, 1.0);
    // TODO: opaque_linearize as a gamma-style adjustment on the result.

    let mut hardness = sv.get(BrushSetting::Hardness).clamp(0.0, 1.0);
    let mut radius = state.actual_radius;

    // libmypaint's anti_aliasing: if the current edge fadeout (in pixels)
    // is narrower than the requested minimum, soften the brush by lowering
    // hardness and growing the geometric radius so the *optical* radius —
    // the perceptual center of the falloff — stays the same. Encoding AA
    // this way (rather than as a separate dab field) means the renderer
    // sees a regular hard/soft dab with no special path. See
    // libmypaint/mypaint-brush.c `prepare_and_draw_dab`.
    let aa_min = sv.get(BrushSetting::AntiAliasing).max(0.0);
    let current_fadeout = radius * (1.0 - hardness);
    if aa_min > current_fadeout && hardness < 1.0 || (aa_min > 0.0 && hardness >= 1.0) {
        let optical = radius - (1.0 - hardness) * radius * 0.5;
        let hardness_new = (optical - aa_min * 0.5) / (optical + aa_min * 0.5);
        if hardness_new > 0.0 && hardness_new < 1.0 {
            radius = aa_min / (1.0 - hardness_new);
            hardness = hardness_new;
        }
    }

    let eraser = sv.get(BrushSetting::Eraser).clamp(0.0, 1.0);
    let alpha_eraser = 1.0 - eraser;

    Dab {
        x: px,
        y: py,
        radius,
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
        // AA has been baked into `radius` and `hardness` above.
        anti_aliasing: 0.0,
    }
}

/// libmypaint's per-speed input mapping. The brush's `speedN_gamma` setting
/// is `ln(gamma)`; with `gamma`, `m`, and `q` derived to anchor the curve at
/// `(speed=45, value=0.5)` with slope `0.015`, the resulting input is
/// `log(gamma + speed) * m + q`.
fn speed_input(speed_norm: f32, gamma_log: f32) -> f32 {
    let gamma = gamma_log.exp();
    let fix1 = 45.0_f32;
    let m = 0.015 * (fix1 + gamma);
    let q = 0.5 - m * (fix1 + gamma).ln();
    (gamma + speed_norm.max(0.0)).ln() * m + q
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
