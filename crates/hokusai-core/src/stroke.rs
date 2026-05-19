//! Stroke engine — port of libmypaint's `mypaint_brush_stroke_to`.
//!
//! Pipeline per event:
//! 1. If the stroke is fresh (or `dtime ≥ 5 s`), seed state and return.
//! 2. Compute raw event delta and tilt-derived inputs.
//! 3. Apply `slow_tracking` to advance `state.actual_x/y` toward the event.
//! 4. Run a libmypaint-style `while (dabs_moved + dabs_todo >= 1)` loop.
//!    Each iteration advances `cur_pressure`, `state.norm_speedN_slow`,
//!    `cur_ax/cur_ay` (lagged dab centre via `slow_tracking_per_dab`) and
//!    `state.stroke_state`, re-evaluates every setting, then draws the
//!    dab. `random_input` is refreshed from the PRNG after each draw.
//! 5. A final no-draw step absorbs the remaining `dtime_left` into the
//!    speed slowness state so the next event starts cleanly.
//!
//! Still deferred: the spectral `paint` mode is ported but the
//! libmypaint reference comparison uses the legacy stroke_to which
//! hard-codes `paint = 0`, so pigment-mixing brushes (blenders,
//! watercolours) won't show parity until the C wrapper switches to
//! `mypaint_brush_stroke_to_2` with a Surface2 wrapper.

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
            // libmypaint sets `self->random_input = rng_double_next()` inside
            // its `dtime > max_dtime || reset_requested` branch, before
            // returning without drawing. Mirror that so the first real event
            // after the warm-up sees the same `INPUT(RANDOM)` value as
            // libmypaint does.
            state.random_input = state.rng.next_unit();
            state.last_event_x = x;
            state.last_event_y = y;
            state.last_event_time += dtime;
            state.actual_x = x;
            state.actual_y = y;
            state.actual_dab_x = x;
            state.actual_dab_y = y;
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
            state.norm_dx_slow = 0.0;
            state.norm_dy_slow = 0.0;
            state.direction_dx = 0.0;
            state.direction_dy = 0.0;
            state.direction_angle_dx = 0.0;
            state.direction_angle_dy = 0.0;
            state.stroke_total_painting_time = 0.0;
            state.stroke_current_idling_time = 0.0;
            state.stroke_state = 0.0;
            state.stroke_started = false;
            state.custom_input = 0.0;
            state.flip = -1.0;
            // Seed the smoothed tilt state at the event's input so the
            // first dab doesn't lerp away from a stale value.
            let m = (xtilt * xtilt + ytilt * ytilt).sqrt().min(1.0);
            state.ascension = if xtilt == 0.0 && ytilt == 0.0 {
                0.0
            } else {
                (-xtilt).atan2(ytilt).to_degrees()
            };
            state.declination = if xtilt == 0.0 && ytilt == 0.0 {
                90.0
            } else {
                90.0 - m * 60.0
            };
            state.declination_x = xtilt * 60.0;
            state.declination_y = ytilt * 60.0;
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
        // and applies `fac = 1 - exp(-step_dtime / slow)`. The smoothing is
        // run *inside* the dab loop using each step's slice of `dtime`, so
        // the first dab of a fresh segment only inherits a tiny fraction of
        // the new raw speed. Hokusai used to apply the full-event smoothing
        // up front, which pushed `norm_speed1_slow` straight to its final
        // value on dab #1 and tanked the radius for any brush whose
        // `radius_logarithmic` curve includes `speed1` (calligraphy, …).
        // Just cache the inputs here; advance the state per dab below.
        let slow1 = self.get(BrushSetting::Speed1Slowness).base_value.max(0.0);
        let slow2 = self.get(BrushSetting::Speed2Slowness).base_value.max(0.0);

        // --- Stroke input: start / end gating ------------------------------
        // libmypaint flips `STATE.STROKE_STARTED` based on pressure crossing
        // `stroke_threshold` (and `stroke_threshold * 0.9 + ε` on the way
        // down). On the rising edge we reset `stroke_state` so `INPUT(STROKE)`
        // restarts at 0; otherwise we'll advance it per dab below by
        // `norm_dist * exp(-stroke_duration_logarithmic)` and wrap on
        // `1 + stroke_holdtime`.
        let stroke_threshold = self
            .get(BrushSetting::StrokeThreshold)
            .base_value
            .max(0.0);
        const STROKE_EPS: f32 = 0.0001;
        if !state.stroke_started && pressure > stroke_threshold + STROKE_EPS {
            state.stroke_started = true;
            state.stroke_state = 0.0;
        } else if state.stroke_started && pressure <= stroke_threshold * 0.9 + STROKE_EPS {
            state.stroke_started = false;
        }
        let stroke_freq = (-self
            .get(BrushSetting::StrokeDurationLogarithmic)
            .base_value)
            .exp();
        let stroke_wrap = 1.0 + self.get(BrushSetting::StrokeHoldtime).base_value.max(0.0);

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
        // libmypaint's `INPUT(RANDOM)` comes from `self->random_input`, which
        // is consumed per-dab (refreshed after each draw) rather than per
        // event. Use the cached value at the event level too — the loop
        // below overrides it per dab to match.
        inputs.set(BrushInput::Random, state.random_input);
        // libmypaint clamps INPUT(STROKE) at evaluation time.
        inputs.set(BrushInput::Stroke, state.stroke_state.min(1.0));
        // libmypaint's INPUT(ATTACK_ANGLE): the smallest angular difference
        // between the pen ascension and (direction_angle + 90°). With no
        // tilt reported we use the default ascension = 0 (matching the tilt
        // block above).
        inputs.set(
            BrushInput::AttackAngle,
            attack_angle(tilt_ascension, dx_raw, dy_raw),
        );
        inputs.set(BrushInput::Direction, direction_input(dx_raw, dy_raw));
        inputs.set(BrushInput::DirectionAngle, direction_angle(dx_raw, dy_raw));
        inputs.set(BrushInput::Tilt, tilt_mag);
        inputs.set(BrushInput::TiltDeclination, tilt_declination);
        inputs.set(BrushInput::TiltAscension, tilt_ascension);
        // libmypaint maps the signed tilt components directly to
        // `*60` degrees so curves can use the per-axis lean separately.
        inputs.set(BrushInput::TiltDeclinationX, xtilt * 60.0);
        inputs.set(BrushInput::TiltDeclinationY, ytilt * 60.0);
        // `viewzoom = log(scale)` in libmypaint; with the app feeding no
        // zoom information we sit at the neutral value (1.0× → 0).
        inputs.set(BrushInput::Viewzoom, 0.0);
        // No barrel/twist on a plain stroke_to API, so always 0°.
        inputs.set(BrushInput::BarrelRotation, 0.0);
        // libmypaint feeds `BASEVAL(RADIUS_LOGARITHMIC)` directly (`ln(r)`).
        inputs.set(
            BrushInput::BrushRadius,
            self.get(BrushSetting::Radius).base_value,
        );
        let sv = evaluate(self, &inputs);

        // libmypaint's *base_radius* is `expf(BASEVAL(RADIUS_LOGARITHMIC))` —
        // a brush-level constant unaffected by per-event input curves.
        // Several downstream calculations (offset_by_random jitter, the
        // dabs_per_basic_radius term, tracking_noise) scale by it rather
        // than the current dab radius.
        let base_radius = self.get(BrushSetting::Radius).base_value.exp().max(0.1);

        // --- Resolve actual radius ------------------------------------------
        // libmypaint's `radius_logarithmic` is stored as `ln(radius)`, so the
        // brush's effective radius in pixels is `exp(value)`. Using `exp2`
        // here previously made every dab ~2.6× smaller than libmypaint's.
        let radius = sv.get(BrushSetting::Radius).exp().max(0.1);

        // For the dab-count step we need the radius at the *start* of this
        // segment. libmypaint uses `STATE.ACTUAL_RADIUS` here — for a fresh
        // stroke that's 0 (cleared by `brush_reset`), which `count_dabs_to`
        // then defaults to `base_radius`. For subsequent events it's the
        // radius the last dab drew at (≈ end-of-segment pressure's radius).
        // Mirror that: prefer the carried-over `state.actual_radius`, fall
        // back to `base_radius` for the first event so we don't pile dabs
        // up at the very start of the stroke.
        let entry_radius = if state.actual_radius > 0.0 {
            state.actual_radius
        } else {
            base_radius
        };
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
        // --- Tracking noise: gaussian jitter on the raw input position ------
        // libmypaint adds the noise *before* slow_tracking smoothing, scaled
        // by `base_radius * BASEVAL(TRACKING_NOISE)` so the jitter doesn't
        // shrink at low pressure. (libmypaint also has a `skip` mechanism
        // that makes the noise frequency-independent — we drop that and
        // jitter on every event for now.)
        let (mut noisy_x, mut noisy_y) = (x, y);
        let noise_mag =
            base_radius * self.get(BrushSetting::TrackingNoise).base_value.max(0.0);
        if noise_mag > 0.001 {
            noisy_x += state.rng.next_gauss() * noise_mag;
            noisy_y += state.rng.next_gauss() * noise_mag;
        }

        let prev_actual_x = state.actual_x;
        let prev_actual_y = state.actual_y;
        let new_actual_x = prev_actual_x + (noisy_x - prev_actual_x) * approach;
        let new_actual_y = prev_actual_y + (noisy_y - prev_actual_y) * approach;

        // The segment delta after smoothing — used by the dab loop and
        // count_dabs_to. We don't need the magnitude here directly.
        let _ = (new_actual_x - prev_actual_x, new_actual_y - prev_actual_y);

        // (Historically hokusai gated dab emission on `stroke_threshold`,
        // but libmypaint does not — that setting only drives the
        // `stroke_started` reset around `INPUT(STROKE)`, handled above.)

        // --- Dab count along the smoothed segment ---------------------------
        // libmypaint counts dabs with BASE values for DPAR/DPBR/DPS (it
        // ignores any input curves on these settings via `BASEVAL(...)`),
        // re-evaluating the count after each dab against the freshly
        // advanced state. Mirror that with a per-iteration loop.
        let dpar = self.get(BrushSetting::DabsPerActualRadius).base_value.max(0.0);
        let dpbr = self.get(BrushSetting::DabsPerBasicRadius).base_value.max(0.0);
        let dps = self.get(BrushSetting::DabsPerSecond).base_value.max(0.0);

        // Elliptical correction: libmypaint computes `count_dabs_to`'s
        // distance via `sqrt(((dy*cs - dx*sn) * aspect)² + (dy*sn + dx*cs)²)`,
        // which is just `|motion| * sqrt(cos²(rel) + aspect² · sin²(rel))`
        // where `rel = angle - motion_angle`. The factor is constant within
        // a segment because the motion vector's direction doesn't change.
        let aspect = sv.get(BrushSetting::EllipticalDabRatio).max(1.0);
        let dab_angle_rad = sv.get(BrushSetting::EllipticalDabAngle).to_radians();

        let smudge_amt = sv.get(BrushSetting::Smudge).clamp(0.0, 1.0);
        let smudge_length = sv.get(BrushSetting::SmudgeLength).clamp(0.0, 1.0);
        let smudge_radius_log = sv.get(BrushSetting::SmudgeRadiusLog);
        let off_random = sv.get(BrushSetting::OffsetByRandom).max(0.0);
        let off_speed = sv.get(BrushSetting::OffsetBySpeed);

        // Running state for the inner loop. `cur_*` advances toward the
        // event's smoothed target one step at a time; libmypaint commits
        // these back into STATE after the final no-draw step below.
        //
        // libmypaint distinguishes STATE.X (smoothed cursor, advances toward
        // the slow-tracked target) from STATE.ACTUAL_X (the dab centre,
        // additionally lagged behind STATE.X by `slow_tracking_per_dab`).
        // `cur_x/cur_y` mirror STATE.X and `cur_ax/cur_ay` mirror
        // STATE.ACTUAL_X — `cur_ax/cur_ay` is where each dab actually lands.
        let mut cur_x = prev_actual_x;
        let mut cur_y = prev_actual_y;
        let mut cur_ax = state.actual_dab_x;
        let mut cur_ay = state.actual_dab_y;
        let mut cur_pressure = entry_pressure;
        let mut dtime_left = dt;
        let mut dabs_moved = state.dist_past_dab;
        let target_x = new_actual_x;
        let target_y = new_actual_y;
        let slow_per_dab = sv.get(BrushSetting::SlowTrackingPerDab).max(0.0);

        let mut dabs_todo = count_dabs_to(
            cur_x, cur_y, target_x, target_y,
            entry_radius, base_radius,
            dpar, dpbr, dps,
            dtime_left,
            dab_angle_rad, aspect,
        );
        let mut painted = false;
        let mut dab_inputs = inputs.clone();

        // The first iteration only consumes `1 - dabs_moved` of a dab so the
        // accumulator picks up wherever the previous event left off. After
        // that every iteration is a full unit dab. Mirrors libmypaint's
        // `step_ddab = (dabs_moved > 0) ? (1 - dabs_moved) : 1.0`.
        while dabs_moved + dabs_todo >= 1.0 {
            let step_ddab = if dabs_moved > 0.0 { 1.0 - dabs_moved } else { 1.0 };
            dabs_moved = 0.0;
            let frac = (step_ddab / dabs_todo.max(1e-6)).clamp(0.0, 1.0);

            let step_dx = frac * (target_x - cur_x);
            let step_dy = frac * (target_y - cur_y);
            let step_dpressure = frac * (pressure - cur_pressure);
            let step_dtime = frac * dtime_left;

            cur_x += step_dx;
            cur_y += step_dy;
            cur_pressure += step_dpressure;

            // Advance tilt state toward the event's target. libmypaint
            // uses `frac * smallest_angular_difference(STATE.ASCENSION,
            // tilt_ascension)` for the ascension delta so a 359° → 1°
            // event lags by ~2°, not ~358°. Declination is a plain
            // additive delta. With `xtilt = ytilt = 0` the targets sit
            // at the libmypaint defaults (ascension 0, declination 90).
            let step_ascension = frac * smallest_angular_diff(state.ascension, tilt_ascension);
            let step_declination = frac * (tilt_declination - state.declination);
            let step_decl_x = frac * (xtilt * 60.0 - state.declination_x);
            let step_decl_y = frac * (ytilt * 60.0 - state.declination_y);
            state.ascension += step_ascension;
            state.declination += step_declination;
            state.declination_x += step_decl_x;
            state.declination_y += step_decl_y;
            // Lag `cur_ax/cur_ay` behind `cur_x/cur_y` by
            // `slow_tracking_per_dab`. libmypaint uses
            // `fac = 1 - exp(-step_ddab / SLOW_TRACKING_PER_DAB)` here, so
            // larger `slow_per_dab` keeps the dab centre stuck closer to its
            // previous spot per step.
            let fac_ax = if slow_per_dab > 1e-3 {
                1.0 - (-step_ddab / slow_per_dab).exp()
            } else {
                1.0
            };
            cur_ax += (cur_x - cur_ax) * fac_ax;
            cur_ay += (cur_y - cur_ay) * fac_ax;

            // Per-step speed slowness (see libmypaint's
            // `update_states_and_setting_values`).
            let fac1 = if slow1 > 1e-3 {
                1.0 - (-step_dtime / slow1).exp()
            } else {
                1.0
            };
            let fac2 = if slow2 > 1e-3 {
                1.0 - (-step_dtime / slow2).exp()
            } else {
                1.0
            };
            state.norm_speed1_slow += (raw_speed - state.norm_speed1_slow) * fac1;
            state.norm_speed2_slow += (raw_speed - state.norm_speed2_slow) * fac2;

            // libmypaint also smooths the *vector* velocity (NORM_DX_SLOW /
            // NORM_DY_SLOW) with its own time constant — used by
            // `offset_by_speed` to push the dab along the actual motion
            // direction, including sign. `time_constant = exp(slow * 0.01) - 1`
            // with a 0.002 floor (a long-standing libmypaint workaround for
            // a Windows-only zero-filtering bug).
            let speed_off_slow = sv.get(BrushSetting::OffsetBySpeedSlowness);
            let speed_off_tc = ((speed_off_slow * 0.01).exp() - 1.0).max(0.002);
            let fac_dx = if step_dtime > 0.0 {
                1.0 - (-step_dtime / speed_off_tc).exp()
            } else {
                1.0
            };
            // norm_dx = step_dx / step_dtime (with viewzoom = 1)
            if step_dtime > 0.0 {
                let norm_dx = step_dx / step_dtime;
                let norm_dy = step_dy / step_dtime;
                state.norm_dx_slow += (norm_dx - state.norm_dx_slow) * fac_dx;
                state.norm_dy_slow += (norm_dy - state.norm_dy_slow) * fac_dx;
            }

            // Direction filter: libmypaint smooths two direction vectors
            // here, both gated on `direction_filter`. The smoothing
            // strength uses `step_in_dabtime = hypot(step_dx, step_dy)`
            // so a wider step pulls the smoothed direction further along
            // toward the current motion. DIRECTION_DX/DY is 180°-folded
            // (so back-and-forth strokes don't flip the input), while
            // DIRECTION_ANGLE_DX/DY tracks the full 360°.
            let dir_filter = sv.get(BrushSetting::DirectionFilter).max(0.0);
            let dir_time_const = (dir_filter * 0.5).exp() - 1.0;
            let step_in_dabtime = (step_dx * step_dx + step_dy * step_dy).sqrt();
            let dir_fac = if dir_time_const > 1e-3 {
                1.0 - (-step_in_dabtime / dir_time_const).exp()
            } else {
                1.0
            };
            // 360° tracker first (uses the raw step vector).
            state.direction_angle_dx += (step_dx - state.direction_angle_dx) * dir_fac;
            state.direction_angle_dy += (step_dy - state.direction_angle_dy) * dir_fac;
            // 180°-folded tracker: pick the closer of ±(step_dx, step_dy)
            // to the previous direction so the smoothed vector doesn't
            // oscillate when the stroke reverses.
            let (mut dx_for_dir, mut dy_for_dir) = (step_dx, step_dy);
            let dx_old = state.direction_dx;
            let dy_old = state.direction_dy;
            let pos_dist = (dx_old - dx_for_dir).powi(2) + (dy_old - dy_for_dir).powi(2);
            let neg_dist = (dx_old + dx_for_dir).powi(2) + (dy_old + dy_for_dir).powi(2);
            if pos_dist > neg_dist {
                dx_for_dir = -dx_for_dir;
                dy_for_dir = -dy_for_dir;
            }
            state.direction_dx += (dx_for_dir - state.direction_dx) * dir_fac;
            state.direction_dy += (dy_for_dir - state.direction_dy) * dir_fac;

            // Advance STATE.STROKE by this step's normalised distance.
            // `norm_dist = |step| / base_radius`; libmypaint uses this for
            // the distance contribution so a brush with a larger base radius
            // takes more pixels to "fill" each unit of stroke. The wrap rule
            // saturates at 1.0 when stroke_holdtime >= ~9.9 (a hold-forever
            // signal), otherwise modulos `1 + stroke_holdtime` so periodic
            // stroke-driven curves cycle.
            let step_dist =
                (step_dx * step_dx + step_dy * step_dy).sqrt() / base_radius.max(1e-3);
            let mut stroke_advance = (state.stroke_state + step_dist * stroke_freq).max(0.0);
            if stroke_advance >= stroke_wrap {
                if stroke_wrap > 10.9 {
                    stroke_advance = 1.0;
                } else {
                    stroke_advance %= stroke_wrap;
                }
            }
            state.stroke_state = stroke_advance;

            dab_inputs.set(BrushInput::Pressure, cur_pressure);
            dab_inputs.set(BrushInput::Stroke, state.stroke_state.min(1.0));
            // AttackAngle is event-level (depends on raw direction, not the
            // per-dab interpolated state) so inheriting from `inputs` is
            // already correct — no per-dab override needed.
            dab_inputs.set(
                BrushInput::Speed1,
                speed_input(
                    state.norm_speed1_slow,
                    self.get(BrushSetting::Speed1Gamma).base_value,
                ),
            );
            dab_inputs.set(
                BrushInput::Speed2,
                speed_input(
                    state.norm_speed2_slow,
                    self.get(BrushSetting::Speed2Gamma).base_value,
                ),
            );
            dab_inputs.set(BrushInput::Random, state.random_input);

            // Smoothed tilt inputs: libmypaint feeds the lagged STATE
            // values into INPUT(TILT_*) and INPUT(ATTACK_ANGLE) at the
            // per-dab evaluation. With viewrotation = 0 the ascension
            // wraps into `(-180, 180]` exactly like libmypaint's
            // `mod_arith(... + 180, 360) - 180`.
            let asc_wrapped = (state.ascension + 180.0).rem_euclid(360.0) - 180.0;
            dab_inputs.set(BrushInput::TiltDeclination, state.declination);
            dab_inputs.set(BrushInput::TiltAscension, asc_wrapped);
            dab_inputs.set(BrushInput::TiltDeclinationX, state.declination_x);
            dab_inputs.set(BrushInput::TiltDeclinationY, state.declination_y);
            dab_inputs.set(
                BrushInput::AttackAngle,
                attack_angle(state.ascension, dx_raw, dy_raw),
            );

            // Custom input: feed the previous-dab smoothed value so the
            // curve in `evaluate` below can reference it (libmypaint pushes
            // the *prior* STATE.CUSTOM_INPUT into INPUT(CUSTOM) and only
            // refreshes the state after the dab is drawn — see the
            // `STATE.CUSTOM_INPUT += ...` block right below).
            dab_inputs.set(BrushInput::Custom, state.custom_input);
            // Smoothed direction inputs (per libmypaint's DIRECTION_DX/DY
            // and DIRECTION_ANGLE_DX/DY) — replace the event-level raw
            // direction we seeded `inputs` with.
            dab_inputs.set(
                BrushInput::Direction,
                direction_input(state.direction_dx, state.direction_dy),
            );
            dab_inputs.set(
                BrushInput::DirectionAngle,
                direction_angle(state.direction_angle_dx, state.direction_angle_dy),
            );

            // libmypaint computes GRIDMAP_X / GRIDMAP_Y from the (lagged)
            // dab centre, scaled by `exp(GRIDMAP_SCALE)` and the per-axis
            // multipliers. The values land in `[0, GRID_SIZE)` so curves
            // can use them as periodic indices. We read BASEVAL for the
            // scale settings — they almost never carry input curves in
            // practice, and treating them as constants per brush avoids a
            // chicken-and-egg with the per-dab `evaluate(...)` below.
            const GRID_SIZE: f32 = 256.0;
            let gscale = self
                .get(BrushSetting::GridmapScale)
                .base_value
                .exp()
                .max(1e-3);
            let gscale_x = self.get(BrushSetting::GridmapScaleX).base_value;
            let gscale_y = self.get(BrushSetting::GridmapScaleY).base_value;
            let scaled_size = gscale * GRID_SIZE;
            let mut gx = (cur_ax * gscale_x).abs().rem_euclid(scaled_size)
                / scaled_size
                * GRID_SIZE;
            let mut gy = (cur_ay * gscale_y).abs().rem_euclid(scaled_size)
                / scaled_size
                * GRID_SIZE;
            if cur_ax < 0.0 {
                gx = GRID_SIZE - gx;
            }
            if cur_ay < 0.0 {
                gy = GRID_SIZE - gy;
            }
            dab_inputs.set(BrushInput::GridmapX, gx.clamp(0.0, GRID_SIZE));
            dab_inputs.set(BrushInput::GridmapY, gy.clamp(0.0, GRID_SIZE));

            let dab_sv = evaluate(self, &dab_inputs);
            let dab_radius = dab_sv.get(BrushSetting::Radius).exp().max(0.1);
            state.actual_radius = dab_radius;

            // Refresh STATE.CUSTOM_INPUT toward the freshly evaluated
            // SETTING(custom_input). libmypaint uses a fixed `0.1`
            // pseudo-`dt` here (the slowness is measured in "10× longer is
            // 10× slower"), so the smoothing strength doesn't depend on
            // the per-dab step time.
            let cust_slow = dab_sv.get(BrushSetting::CustomInputSlowness).max(0.0);
            let cust_fac = if cust_slow > 1e-3 {
                1.0 - (-0.1 / cust_slow).exp()
            } else {
                1.0
            };
            let cust_target = dab_sv.get(BrushSetting::CustomInput);
            state.custom_input += (cust_target - state.custom_input) * cust_fac;

            let mut px = cur_ax;
            let mut py = cur_ay;

            // Toggle libmypaint's `STATE.FLIP` so `offset_angle_2*` can
            // mirror dabs across the stroke direction. Done *before* the
            // offsets so this dab gets the freshly toggled sign.
            state.flip = -state.flip;
            let (off_x, off_y) = directional_offsets(
                &dab_sv,
                base_radius,
                state.flip,
                state.direction_angle_dx,
                state.direction_angle_dy,
                // ASCENSION isn't tracked per-dab yet (libmypaint smooths
                // it like position); use the event-level tilt_ascension
                // we already computed.
                state.ascension,
            );
            px += off_x;
            py += off_y;

            if off_random > 0.0 {
                px += state.rng.next_gauss() * off_random * base_radius;
                py += state.rng.next_gauss() * off_random * base_radius;
            }
            if off_speed != 0.0 {
                // libmypaint: `x += NORM_DX_SLOW * offset_by_speed * 0.1 /
                // viewzoom`. Viewzoom is 1.0 here. The previous hokusai
                // formula scaled by `dab_radius * norm_speed1_slow * 0.04`
                // — different vector source and different scale.
                px += state.norm_dx_slow * off_speed * 0.1;
                py += state.norm_dy_slow * off_speed * 0.1;
            }

            if smudge_amt > 0.0 {
                let smudge_radius =
                    (dab_radius * smudge_radius_log.exp()).max(1.0);
                let sample = surface.get_color(px, py, smudge_radius);
                let fac = smudge_length;
                state.smudge_ra = state.smudge_ra * fac + sample.r * sample.a * (1.0 - fac);
                state.smudge_ga = state.smudge_ga * fac + sample.g * sample.a * (1.0 - fac);
                state.smudge_ba = state.smudge_ba * fac + sample.b * sample.a * (1.0 - fac);
                state.smudge_a = state.smudge_a * fac + sample.a * (1.0 - fac);
            }

            drift_color(state, &dab_sv, step_dtime);

            let dab = build_dab(self, &dab_sv, state, px, py, smudge_amt);
            if surface.draw_dab(&dab) {
                painted = true;
            }
            state.last_dab_x = dab.x;
            state.last_dab_y = dab.y;
            // libmypaint refreshes `random_input` once per dab, *after*
            // drawing.
            state.random_input = state.rng.next_unit();

            dtime_left = (dtime_left - step_dtime).max(0.0);
            dabs_todo = count_dabs_to(
                cur_x, cur_y, target_x, target_y,
                dab_radius, base_radius,
                dpar, dpbr, dps,
                dtime_left,
                dab_angle_rad, aspect,
            );
        }

        // Final no-draw step: libmypaint advances STATE one last time to the
        // event's input pressure/position/dtime so the next event's
        // `count_dabs_to` starts from the right place. We don't need to
        // recompute settings here — they only matter for the per-dab work
        // we've already done — but the speed slowness must absorb the
        // remaining `dtime_left` and `dabs_moved` must carry the fractional
        // leftover.
        if dtime_left > 0.0 {
            let fac1 = if slow1 > 1e-3 {
                1.0 - (-dtime_left / slow1).exp()
            } else {
                1.0
            };
            let fac2 = if slow2 > 1e-3 {
                1.0 - (-dtime_left / slow2).exp()
            } else {
                1.0
            };
            state.norm_speed1_slow += (raw_speed - state.norm_speed1_slow) * fac1;
            state.norm_speed2_slow += (raw_speed - state.norm_speed2_slow) * fac2;
        }
        state.dist_past_dab = dabs_moved + dabs_todo;

        // --- Commit event state ---------------------------------------------
        state.last_event_x = x;
        state.last_event_y = y;
        state.last_event_time += dtime;
        state.actual_x = new_actual_x;
        state.actual_y = new_actual_y;
        state.actual_dab_x = cur_ax;
        state.actual_dab_y = cur_ay;
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

    // libmypaint's `apply_smudge`: when smudge > 0, fold the sampled
    // canvas colour (stored as `SMUDGE_R/G/B/A` — premultiplied by the
    // sample alpha during update) into the brush colour, and derive a
    // dab-level `eraser_target_alpha` from the smudge bucket's alpha so
    // a smudge into translucent canvas erases proportionally.
    //
    //     eraser_target_alpha = (1 - smudge) + smudge * smudge_a
    //     color_r             = (smudge * SMUDGE_R + (1 - smudge) * brush_r)
    //                           / eraser_target_alpha   (straight alpha)
    //
    // Hokusai used to do a straight linear blend with `mix_r = SMUDGE_R /
    // smudge_a` and `alpha_eraser = 1 - eraser`, which left smudge brushes
    // painting fully opaque dabs even when the smudge bucket was nearly
    // transparent. That's the bulk of the divergence on the blender /
    // smear / watercolour brushes in the upstream pack.
    let smudge_amt = smudge_amt.clamp(0.0, 1.0);
    let eraser_target_alpha =
        ((1.0 - smudge_amt) + smudge_amt * state.smudge_a).clamp(0.0, 1.0);
    let color = if smudge_amt <= 0.0 || eraser_target_alpha <= 0.0 {
        crate::color::RgbaF32 {
            r: base.r,
            g: base.g,
            b: base.b,
            a: 1.0,
        }
    } else {
        let col_factor = 1.0 - smudge_amt;
        crate::color::RgbaF32 {
            r: ((smudge_amt * state.smudge_ra + col_factor * base.r) / eraser_target_alpha)
                .clamp(0.0, 1.0),
            g: ((smudge_amt * state.smudge_ga + col_factor * base.g) / eraser_target_alpha)
                .clamp(0.0, 1.0),
            b: ((smudge_amt * state.smudge_ba + col_factor * base.b) / eraser_target_alpha)
                .clamp(0.0, 1.0),
            a: 1.0,
        }
    };

    // libmypaint composes the final opacity as opaque * opaque_multiply.
    // Many stock brushes (charcoal, pencil, …) drive opaque_multiply from
    // pressure, so skipping it makes them look wrong at non-full pressure.
    // libmypaint defaults opaque_multiply to 1.0; treat a wholly-default
    // setting (no base value and no input curves) as that identity.
    let opaque_raw = sv.get(BrushSetting::Opaque).clamp(0.0, 2.0);
    let opaque_mult = opaque_multiplier(brush, sv);
    let mut opaque = (opaque_raw * opaque_mult).clamp(0.0, 1.0);

    // libmypaint's `opaque_linearize` compensates for the fact that
    // overlapping dabs accumulate alpha non-linearly: the per-dab alpha
    // is rooted by `1/dabs_per_pixel` so the *aggregate* opacity at the
    // dab center matches `opaque`. Brushes like the stock round brush
    // (`opaque_linearize=0.44`) rely on this to dim their feathered edges
    // without going full opaque — without it, hokusai's tails of a
    // pressure ramp keep painting at the unmodulated `opaque` value while
    // libmypaint's drop to near zero.
    let opaque_linearize = brush
        .get(BrushSetting::OpaqueLinearize)
        .base_value
        .max(0.0);
    if opaque_linearize > 0.0 && opaque > 0.0 {
        let dpar = brush.get(BrushSetting::DabsPerActualRadius).base_value;
        let dpbr = brush.get(BrushSetting::DabsPerBasicRadius).base_value;
        let mut dabs_per_pixel = (dpar + dpbr) * 2.0;
        if dabs_per_pixel < 1.0 {
            dabs_per_pixel = 1.0;
        }
        dabs_per_pixel = 1.0 + opaque_linearize * (dabs_per_pixel - 1.0);
        let beta = 1.0 - opaque;
        let beta_dab = beta.powf(1.0 / dabs_per_pixel);
        opaque = (1.0 - beta_dab).clamp(0.0, 1.0);
    }

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
    if current_fadeout < aa_min {
        let optical = radius - (1.0 - hardness) * radius * 0.5;
        let hardness_new = (optical - aa_min * 0.5) / (optical + aa_min * 0.5);
        // libmypaint applies the result unconditionally; sub-pixel dabs end
        // up with negative hardness, which `draw_dab_default` rejects to
        // match the upstream `op->hardness == 0` early-out. We only guard
        // against `hardness_new == 1` here to avoid div-by-zero on the
        // radius assignment.
        if hardness_new < 1.0 {
            radius = aa_min / (1.0 - hardness_new);
            hardness = hardness_new;
        }
    }

    // libmypaint folds the smudge-derived `eraser_target_alpha` into the
    // dab's source alpha BEFORE the eraser setting is applied: the
    // smudge bucket can already be partially transparent, and a smudge
    // brush is expected to "drag" that transparency along with the
    // colour. `alpha_eraser` is what the renderer multiplies the
    // per-pixel mask by, so passing the combined value here gives the
    // libmypaint blend.
    let eraser = sv.get(BrushSetting::Eraser).clamp(0.0, 1.0);
    let alpha_eraser = (eraser_target_alpha * (1.0 - eraser)).clamp(0.0, 1.0);

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
        // libmypaint's `prepare_and_draw_dab` scales by 100 and clamps to
        // `[1, 128]` before handing the value to the posterize blend, so
        // the `.myb` setting `posterize_num = 0.02` (Posterizer) becomes a
        // 2-step quantisation rather than the 1-step degenerate hokusai
        // used to compute via `max(1.0)`.
        posterize_num: (sv.get(BrushSetting::PosterizeNum) * 100.0)
            .round()
            .clamp(1.0, 128.0),
        paint: sv.get(BrushSetting::Paint).clamp(0.0, 1.0),
        // AA has been baked into `radius` and `hardness` above.
        anti_aliasing: 0.0,
    }
}

/// libmypaint's per-speed input mapping. The brush's `speedN_gamma` setting
/// is `ln(gamma)`; with `gamma`, `m`, and `q` derived to anchor the curve at
/// `(speed=45, value=0.5)` with slope `0.015`, the resulting input is
/// `log(gamma + speed) * m + q`.
/// Port of libmypaint's `count_dabs_to` (`legacy_dab_count`): dabs to draw
/// to reach `(tgt_x, tgt_y)` over `dt_left` seconds, given the current
/// `actual_radius`. Mirrors the elliptical-distance correction libmypaint
/// applies via `STATE.ACTUAL_ELLIPTICAL_DAB_RATIO` so thin brushes still
/// receive enough dabs to cover their minor-axis cross-section.
#[allow(clippy::too_many_arguments)]
fn count_dabs_to(
    cur_x: f32,
    cur_y: f32,
    tgt_x: f32,
    tgt_y: f32,
    actual_radius: f32,
    base_radius: f32,
    dpar: f32,
    dpbr: f32,
    dps: f32,
    dt_left: f32,
    dab_angle_rad: f32,
    aspect: f32,
) -> f32 {
    let dx = tgt_x - cur_x;
    let dy = tgt_y - cur_y;
    let dist = if aspect > 1.0 {
        let cs = dab_angle_rad.cos();
        let sn = dab_angle_rad.sin();
        let yyr = (dy * cs - dx * sn) * aspect;
        let xxr = dy * sn + dx * cs;
        (yyr * yyr + xxr * xxr).sqrt()
    } else {
        (dx * dx + dy * dy).sqrt()
    };
    let num_actual = if actual_radius > 0.0 {
        dist / actual_radius * dpar
    } else {
        0.0
    };
    let num_basic = if base_radius > 0.0 {
        dist / base_radius * dpbr
    } else {
        0.0
    };
    let num_time = dt_left.max(0.0) * dps;
    num_actual + num_basic + num_time
}

/// Port of libmypaint's `directional_offsets`. Sums the constant
/// `offset_x` / `offset_y` shift with up to six directional offsets:
/// one each (and a FLIP-mirrored partner) aligned with the smoothed
/// stroke direction, the pen ascension, and the view rotation. The
/// final pair is scaled by `base_radius * exp(offset_multiplier)` and
/// clamped to ±3240 px to match libmypaint's safety net against runaway
/// memory use from extreme settings.
///
/// `viewrotation` is hard-coded to 0 — hokusai's `stroke_to` doesn't
/// take a canvas rotation, so the `*_view` directions reduce to the
/// world-x axis.
#[allow(clippy::too_many_arguments)]
fn directional_offsets(
    sv: &SettingValues,
    base_radius: f32,
    flip: f32,
    direction_angle_dx: f32,
    direction_angle_dy: f32,
    ascension_deg: f32,
) -> (f32, f32) {
    let offset_mult = sv.get(BrushSetting::OffsetMultiplier).exp();
    if !offset_mult.is_finite() {
        return (0.0, 0.0);
    }

    let mut dx = sv.get(BrushSetting::OffsetX);
    let mut dy = sv.get(BrushSetting::OffsetY);

    let offset_angle_adj = sv.get(BrushSetting::OffsetAngleAdj);
    let stroke_angle_deg = direction_angle_dy
        .atan2(direction_angle_dx)
        .to_degrees()
        - 90.0;
    let stroke_angle_deg = stroke_angle_deg.rem_euclid(360.0);
    let viewrotation = 0.0_f32;

    let offset_angle = sv.get(BrushSetting::OffsetAngle);
    if offset_angle != 0.0 {
        let a = (stroke_angle_deg + offset_angle_adj).to_radians();
        dx += a.cos() * offset_angle;
        dy += a.sin() * offset_angle;
    }

    let offset_angle_asc = sv.get(BrushSetting::OffsetAngleAsc);
    if offset_angle_asc != 0.0 {
        let a = (ascension_deg - viewrotation + offset_angle_adj).to_radians();
        dx += a.cos() * offset_angle_asc;
        dy += a.sin() * offset_angle_asc;
    }

    let view_offset = sv.get(BrushSetting::OffsetAngleView);
    if view_offset != 0.0 {
        let a = (viewrotation + offset_angle_adj).to_radians();
        dx += (-a).cos() * view_offset;
        dy += (-a).sin() * view_offset;
    }

    let offset_dir_mirror = sv.get(BrushSetting::OffsetAngle2).max(0.0);
    if offset_dir_mirror != 0.0 {
        let a = (stroke_angle_deg + offset_angle_adj * flip).to_radians();
        let factor = offset_dir_mirror * flip;
        dx += a.cos() * factor;
        dy += a.sin() * factor;
    }

    let offset_asc_mirror = sv.get(BrushSetting::OffsetAngle2Asc).max(0.0);
    if offset_asc_mirror != 0.0 {
        let a = (ascension_deg - viewrotation + offset_angle_adj * flip).to_radians();
        let factor = offset_asc_mirror * flip;
        dx += a.cos() * factor;
        dy += a.sin() * factor;
    }

    let offset_view_mirror = sv.get(BrushSetting::OffsetAngle2View).max(0.0);
    if offset_view_mirror != 0.0 {
        let a = (viewrotation + offset_angle_adj).to_radians();
        let factor = offset_view_mirror * flip;
        dx += (-a).cos() * factor;
        dy += (-a).sin() * factor;
    }

    const LIM: f32 = 3240.0;
    let scale = base_radius * offset_mult;
    ((dx * scale).clamp(-LIM, LIM), (dy * scale).clamp(-LIM, LIM))
}

/// Smallest signed angular difference `b - a` (in degrees), wrapped to
/// `(-180, 180]`. Used to advance `STATE.ASCENSION` / `BARREL_ROTATION`
/// toward their event targets without taking the long way around the
/// circle on wrap-overs.
fn smallest_angular_diff(a: f32, b: f32) -> f32 {
    let mut d = b - a;
    d = (d + 180.0).rem_euclid(360.0) - 180.0;
    d
}

/// libmypaint's `INPUT(ATTACK_ANGLE)`: the smallest angular difference
/// between the pen's ascension direction and the stroke direction (offset
/// by 90°), both in degrees, wrapped to `(-180, 180]`.
fn attack_angle(ascension_deg: f32, dx_raw: f32, dy_raw: f32) -> f32 {
    if dx_raw == 0.0 && dy_raw == 0.0 {
        return 0.0;
    }
    let direction_deg = dy_raw.atan2(dx_raw).to_degrees();
    // `mod_arith(DEGREES(dir) + 90, 360)` in libmypaint.
    let target = ((direction_deg + 90.0).rem_euclid(360.0) + 360.0).rem_euclid(360.0);
    // Smallest signed angular difference.
    let mut d = ascension_deg - target;
    d = (d + 180.0).rem_euclid(360.0) - 180.0;
    d
}

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
        // 20 px / exp(1) ≈ 20 / 2.718 = 7.36 radii of travel. With DPAR=2
        // that's ~14.7 dabs per `count_dabs_to`. libmypaint's per-iteration
        // re-evaluation lands the integer count somewhere in this band.
        let brush = make_brush(1.0, 2.0);
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        brush.stroke_to(&mut state, &mut surf, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        brush.stroke_to(&mut state, &mut surf, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        assert!(
            (12..=16).contains(&surf.count),
            "expected ~14 dabs, got {}",
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
    fn stroke_threshold_drives_stroke_state_reset() {
        // libmypaint's stroke_threshold does *not* suppress dabs. It only
        // gates `STATE.STROKE_STARTED`: when pressure rises above the
        // threshold the stroke restarts (`stroke_state` → 0); when it falls
        // back below `threshold * 0.9` the started flag clears so the next
        // rise resets again.
        let mut brush = make_brush(1.0, 2.0);
        brush.set(BrushSetting::StrokeThreshold, SettingValue::constant(0.5));
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        // Pressure below threshold: started stays false, but dabs still land.
        brush.stroke_to(&mut state, &mut surf, 0.0, 0.0, 0.3, 0.0, 0.0, 0.01);
        brush.stroke_to(&mut state, &mut surf, 20.0, 0.0, 0.3, 0.0, 0.0, 0.01);
        assert!(surf.count > 0, "stroke_threshold no longer gates dab emission");
        assert!(!state.stroke_started, "0.3 < threshold 0.5, started stays off");

        // Above threshold (after a seed pass): started flips on and
        // stroke_state restarts at 0.
        let mut s2 = BrushState::default();
        let mut surf2 = CountingSurface { count: 0 };
        // First call always goes through the seed branch.
        brush.stroke_to(&mut s2, &mut surf2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.01);
        s2.stroke_state = 0.7;
        brush.stroke_to(&mut s2, &mut surf2, 1.0, 0.0, 0.8, 0.0, 0.0, 0.01);
        assert!(s2.stroke_started, "pressure above threshold sets started=true");
        assert_eq!(s2.stroke_state, 0.0, "rising-edge reset wipes prior stroke_state");
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
    fn tilt_declination_follows_libmypaint_convention() {
        // libmypaint: `tilt_declination = 90` when the pen is straight up,
        // dropping to ~30 at the steepest tilt (`90 - hypot(x, y) * 60`).
        // With a curve mapping declination 0 → 0 and 90 → 1, the upright
        // pose feeds the *larger* contribution to the radius curve, so the
        // tilted stroke should end up *smaller* than the upright one.
        let mut tilt_brush = make_brush(1.0, 2.0);
        tilt_brush.set(
            BrushSetting::Radius,
            SettingValue {
                base_value: 1.0,
                inputs: vec![crate::mapping::InputMapping {
                    input: BrushInput::TiltDeclination,
                    points: vec![(0.0, 0.0), (90.0, 1.0)],
                }],
                unknown_inputs: Default::default(),
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
            r_upright > r_tilted,
            "upright pen has higher declination → bigger radius via curve: {r_upright} <= {r_tilted}"
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
