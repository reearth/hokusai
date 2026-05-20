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
            // libmypaint's stroke_to_internal runs the tracking_noise block
            // (mypaint-brush.c:1373-1389) BEFORE the `dtime > max_dtime`
            // brush_reset path (line 1396). When TRACKING_NOISE is set
            // that means each fresh-stroke / warm-up event consumes 2
            // `rand_gauss` draws (= 8 `rng_double_next` units) AND folds
            // the noisy x/y into STATE.X/Y via the brush_reset's explicit
            // `STATE.X = x; STATE.Y = y;` assignment at line 1404-1405
            // (after the reset's memset). hokusai used to skip straight
            // to the reset path with the raw `x, y`, losing both the RNG
            // sequence offset AND the per-stroke initial noise vector.
            let base_radius_init = self
                .get(BrushSetting::Radius)
                .base_value
                .exp()
                .clamp(0.2, 1000.0);
            let noise_init = base_radius_init * self.get(BrushSetting::TrackingNoise).base_value;
            let (mut seed_x, mut seed_y) = (x, y);
            if noise_init > 0.001 {
                seed_x += state.rng.next_gauss() * noise_init;
                seed_y += state.rng.next_gauss() * noise_init;
            }
            // libmypaint then sets `self->random_input = rng_double_next()`
            // inside the reset branch — match that so the first real event
            // sees the same `INPUT(RANDOM)` value.
            state.random_input = state.rng.next_unit();
            // libmypaint stores the post-noise x/y as STATE.X/Y after the
            // reset (mypaint-brush.c:1404-1405). Use seed_x/seed_y so the
            // next event's interpolation starts from the noisy seed.
            state.last_event_x = seed_x;
            state.last_event_y = seed_y;
            state.last_event_time += dtime;
            state.actual_x = seed_x;
            state.actual_y = seed_y;
            state.actual_dab_x = seed_x;
            state.actual_dab_y = seed_y;
            state.last_dab_x = seed_x;
            state.last_dab_y = seed_y;
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
            state.prev_col_r = 0.0;
            state.prev_col_g = 0.0;
            state.prev_col_b = 0.0;
            state.prev_col_a = 0.0;
            state.prev_col_recentness = 0.0;
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
            // libmypaint's brush_reset (mypaint-brush.c:159) zeroes the
            // entire STATE struct via memset. tilt_declination /
            // tilt_ascension only ramp toward their no-tilt defaults
            // (90 / 0) once the per-dab step deltas in
            // update_states_and_setting_values run. hokusai used to
            // seed declination at 90 here, so curves keyed on
            // tilt_declination saw the saturated 90° value from the
            // first dab onwards instead of libmypaint's ramp.
            state.ascension = 0.0;
            state.declination = 0.0;
            state.declination_x = 0.0;
            state.declination_y = 0.0;
            // Silence unused warnings — xtilt/ytilt/m are still in scope
            // for later seed steps (state.X / state.Y).
            let _ = (xtilt, ytilt, m);
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
        let stroke_threshold = self.get(BrushSetting::StrokeThreshold).base_value.max(0.0);
        const STROKE_EPS: f32 = 0.0001;
        if !state.stroke_started && pressure > stroke_threshold + STROKE_EPS {
            state.stroke_started = true;
            state.stroke_state = 0.0;
        } else if state.stroke_started && pressure <= stroke_threshold * 0.9 + STROKE_EPS {
            state.stroke_started = false;
        }
        // STROKE advance now happens per-dab using dab_sv's evaluated
        // STROKE_DURATION_LOGARITHMIC and STROKE_HOLDTIME — see the
        // matching block right after the per-dab `evaluate` call below.

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
        let base_radius = self
            .get(BrushSetting::Radius)
            .base_value
            .exp()
            .clamp(0.2, 1000.0);

        // --- Resolve actual radius ------------------------------------------
        // libmypaint's `radius_logarithmic` is stored as `ln(radius)`, so the
        // brush's effective radius in pixels is `exp(value)`. Using `exp2`
        // here previously made every dab ~2.6× smaller than libmypaint's.
        let radius = sv.get(BrushSetting::Radius).exp().clamp(0.2, 1000.0);

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
        // by `base_radius * BASEVAL(TRACKING_NOISE)`. The matching skip
        // mechanism coalesces fast-arriving events so the noise sample rate
        // tracks cursor *distance*, not input frequency — without it,
        // brushes with `tracking_noise > 0` produce a denser scatter when
        // the app feeds many small events per stroke.
        if state.skip_distance > 0.001 {
            let dx_skip = state.skip_last_x - x;
            let dy_skip = state.skip_last_y - y;
            let dist = (dx_skip * dx_skip + dy_skip * dy_skip).sqrt();
            state.skip_last_x = x;
            state.skip_last_y = y;
            state.skipped_dtime += dtime;
            state.skip_distance -= dist;

            // If we haven't moved past the skip threshold yet, drop this
            // event entirely (no dab, no state advance). The dtime
            // accumulates so a delayed event still walks the brush through
            // the right amount of time.
            if state.skip_distance > 0.001 && state.skipped_dtime < 5.0 {
                state.last_pressure = entry_pressure; // restore — we never used the new pressure
                return false;
            }

            // Skip resolved: pretend we received one large event spanning
            // the accumulated dtime. libmypaint replaces `dtime` here too.
            // (We don't propagate the modified dtime through hokusai's
            // float `dt` since the skip path is rare and `dtime` is only
            // used for speed smoothing; using the original event dtime is
            // a reasonable approximation.)
            state.skip_distance = 0.0;
            state.skip_last_x = 0.0;
            state.skip_last_y = 0.0;
            state.skipped_dtime = 0.0;
        }

        let (mut noisy_x, mut noisy_y) = (x, y);
        let noise_mag = base_radius * self.get(BrushSetting::TrackingNoise).base_value.max(0.0);
        if noise_mag > 0.001 {
            // Arm the next skip window so subsequent events that arrive
            // before the cursor has travelled `0.5 * noise` pixels get
            // coalesced. Setting the bookkeeping fields before the RNG
            // calls matches libmypaint's order in `mypaint-brush.c`.
            state.skip_distance = 0.5 * noise_mag;
            state.skip_last_x = x;
            state.skip_last_y = y;
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
        // libmypaint's state_based_dab_count uses STATE.DABS_PER_*, which
        // are assigned SETTING(...) per-dab in update_states_and_setting_values
        // (mypaint-brush.c:628-630). Read from the event-level SV here so
        // brushes with curve-driven dabs_per_* (Round#1 has brush_radius on
        // DPAR; some scatter brushes use pressure on DPS) at least see the
        // event-time curve evaluation rather than the brush's stored base.
        let dpar = sv.get(BrushSetting::DabsPerActualRadius).max(0.0);
        let dpbr = sv.get(BrushSetting::DabsPerBasicRadius).max(0.0);
        let dps = sv.get(BrushSetting::DabsPerSecond).max(0.0);

        // Elliptical correction: libmypaint computes `count_dabs_to`'s
        // distance via `sqrt(((dy*cs - dx*sn) * aspect)² + (dy*sn + dx*cs)²)`,
        // which is just `|motion| * sqrt(cos²(rel) + aspect² · sin²(rel))`
        // where `rel = angle - motion_angle`. The factor is constant within
        // a segment because the motion vector's direction doesn't change.
        let aspect = sv.get(BrushSetting::EllipticalDabRatio).max(1.0);
        let dab_angle_rad = sv.get(BrushSetting::EllipticalDabAngle).to_radians();

        // libmypaint reads SMUDGE / SMUDGE_LENGTH per-dab; only SMUDGE_RADIUS_LOG
        // is treated as constant by `update_smudge_color`. Match that — the
        // per-dab versions are pulled from `dab_sv` inside the loop.
        // smudge_radius_log is now read per-dab from dab_sv inside the loop
        // — libmypaint reads SETTING(SMUDGE_RADIUS_LOG) inside
        // update_smudge_color (mypaint-brush.c line 854).
        // libmypaint's gate for entering `update_smudge_color` is
        // `SMUDGE != 0 || mapping not constant`. We need to know if the
        // mapping has any inputs so a curve-driven smudge that momentarily
        // evaluates to 0 still gets its recentness counter decayed.
        let smudge_setting = self.get(BrushSetting::Smudge);
        let smudge_mapping_active =
            smudge_setting.base_value != 0.0 || !smudge_setting.inputs.is_empty();
        // libmypaint's `if (offset_by_random)` consumes 2 PRNG draws on
        // ANY non-zero setting (curve-evaluated), then clamps the
        // amplitude with `MAX(0, ...)`. Hokusai used to gate the whole
        // block on `> 0`, so brushes whose pressure / custom curve pulled
        // the value below zero on some dabs (Posterizer, several scatter
        // brushes) silently lost two `next_gauss` consumptions vs the
        // libmypaint reference and the RNG sequences then diverged.
        // off_random / off_speed are now read per-dab from dab_sv (libmypaint
        // reads SETTING(...) per-dab in prepare_and_draw_dab). The block below
        // that gated RNG consumption on off_random_raw moves into the dab
        // loop too.

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
        // SLOW_TRACKING_PER_DAB / SPEED*_SLOWNESS / OFFSET_BY_SPEED_SLOWNESS /
        // DIRECTION_FILTER are read per-dab from dab_sv after evaluation.

        let mut dabs_todo = count_dabs_to(
            cur_x,
            cur_y,
            target_x,
            target_y,
            entry_radius,
            base_radius,
            dpar,
            dpbr,
            dps,
            dtime_left,
            dab_angle_rad,
            aspect,
        );
        let mut painted = false;
        let mut dab_inputs = inputs;

        // The first iteration only consumes `1 - dabs_moved` of a dab so the
        // accumulator picks up wherever the previous event left off. After
        // that every iteration is a full unit dab. Mirrors libmypaint's
        // `step_ddab = (dabs_moved > 0) ? (1 - dabs_moved) : 1.0`.
        while dabs_moved + dabs_todo >= 1.0 {
            let step_ddab = if dabs_moved > 0.0 {
                1.0 - dabs_moved
            } else {
                1.0
            };
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
            // libmypaint's update_states_and_setting_values evaluates ALL
            // SETTINGS using PRE-update STATE values (line 728), then
            // updates STATE (slow_tracking_per_dab / norm_speed_slow /
            // norm_dx/dy_slow / direction_dx/dy) using the freshly
            // evaluated SETTING smoothing factors. Tilt is the exception
            // — it's a raw event delta and stays before evaluation.
            // The state-update block moves to AFTER dab_sv = evaluate so
            // INPUT(DIRECTION) / INPUT(SPEED*) sample the lagged values.

            // libmypaint's update_states_and_setting_values evaluates ALL
            // settings using the PRE-advance STROKE, and only advances
            // STATE.STROKE afterwards using the freshly-evaluated
            // SETTING(STROKE_DURATION_LOGARITHMIC). Mirror that: feed the
            // current state.stroke_state into dab_inputs, evaluate dab_sv,
            // then advance below using dab_sv's per-dab values.
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
            // libmypaint uses the SMOOTHED direction state (DIRECTION_DX/DY,
            // dir_angle_360 = atan2(DIRECTION_DY, DIRECTION_DX)) for the
            // attack_angle input — see mypaint-brush.c:712. Hokusai
            // previously used raw event direction here, which diverges
            // when direction_filter is non-zero (Posterizer drives
            // offset_angle_2 and offset_angle_adj via this input).
            dab_inputs.set(
                BrushInput::AttackAngle,
                attack_angle(
                    state.ascension,
                    state.direction_angle_dx,
                    state.direction_angle_dy,
                ),
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
            // dab centre, scaled by `exp(SETTING(GRIDMAP_SCALE))` and the
            // per-axis SETTING multipliers (mypaint-brush.c:644-646), all
            // per-dab. Read from the event-level SV so brushes that drive
            // gridmap_scale from brush_radius (HalfTone#1) at least see
            // the curve evaluation rather than the brush's stored 0 base.
            // brush_radius is event-constant, so a single SV read per
            // segment is equivalent to libmypaint's per-dab SETTING here.
            const GRID_SIZE: f32 = 256.0;
            let gscale = sv.get(BrushSetting::GridmapScale).exp().max(1e-3);
            let gscale_x = sv.get(BrushSetting::GridmapScaleX);
            let gscale_y = sv.get(BrushSetting::GridmapScaleY);
            let scaled_size = gscale * GRID_SIZE;
            let mut gx =
                (cur_ax * gscale_x).abs().rem_euclid(scaled_size) / scaled_size * GRID_SIZE;
            let mut gy =
                (cur_ay * gscale_y).abs().rem_euclid(scaled_size) / scaled_size * GRID_SIZE;
            if cur_ax < 0.0 {
                gx = GRID_SIZE - gx;
            }
            if cur_ay < 0.0 {
                gy = GRID_SIZE - gy;
            }
            dab_inputs.set(BrushInput::GridmapX, gx.clamp(0.0, GRID_SIZE));
            dab_inputs.set(BrushInput::GridmapY, gy.clamp(0.0, GRID_SIZE));

            let dab_sv = evaluate(self, &dab_inputs);
            let dab_radius = dab_sv.get(BrushSetting::Radius).exp().clamp(0.2, 1000.0);
            state.actual_radius = dab_radius;

            // ===== Post-evaluate STATE updates (libmypaint:732-797) =====
            let slow_per_dab_d = dab_sv.get(BrushSetting::SlowTrackingPerDab).max(0.0);
            let fac_ax = if slow_per_dab_d > 1e-3 {
                1.0 - (-step_ddab / slow_per_dab_d).exp()
            } else {
                1.0
            };
            cur_ax += (cur_x - cur_ax) * fac_ax;
            cur_ay += (cur_y - cur_ay) * fac_ax;

            let slow1_d = dab_sv.get(BrushSetting::Speed1Slowness).max(0.0);
            let slow2_d = dab_sv.get(BrushSetting::Speed2Slowness).max(0.0);
            let fac1 = if slow1_d > 1e-3 {
                1.0 - (-step_dtime / slow1_d).exp()
            } else {
                1.0
            };
            let fac2 = if slow2_d > 1e-3 {
                1.0 - (-step_dtime / slow2_d).exp()
            } else {
                1.0
            };
            state.norm_speed1_slow += (raw_speed - state.norm_speed1_slow) * fac1;
            state.norm_speed2_slow += (raw_speed - state.norm_speed2_slow) * fac2;

            let speed_off_slow_d = dab_sv.get(BrushSetting::OffsetBySpeedSlowness);
            let speed_off_tc = ((speed_off_slow_d * 0.01).exp() - 1.0).max(0.002);
            let fac_dx = if step_dtime > 0.0 {
                1.0 - (-step_dtime / speed_off_tc).exp()
            } else {
                1.0
            };
            if step_dtime > 0.0 {
                let norm_dx = step_dx / step_dtime;
                let norm_dy = step_dy / step_dtime;
                state.norm_dx_slow += (norm_dx - state.norm_dx_slow) * fac_dx;
                state.norm_dy_slow += (norm_dy - state.norm_dy_slow) * fac_dx;
            }

            let dir_filter_d = dab_sv.get(BrushSetting::DirectionFilter).max(0.0);
            let dir_time_const = (dir_filter_d * 0.5).exp() - 1.0;
            let step_in_dabtime = (step_dx * step_dx + step_dy * step_dy).sqrt();
            let dir_fac = if dir_time_const > 1e-3 {
                1.0 - (-step_in_dabtime / dir_time_const).exp()
            } else {
                1.0
            };
            state.direction_angle_dx += (step_dx - state.direction_angle_dx) * dir_fac;
            state.direction_angle_dy += (step_dy - state.direction_angle_dy) * dir_fac;
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

            // Advance STATE.STROKE by this step's normalised distance,
            // using the per-dab evaluated stroke_duration / stroke_holdtime
            // (libmypaint reads them as SETTING, not BASEVAL). The wrap rule
            // saturates at 1.0 when stroke_holdtime >= ~9.9 (a hold-forever
            // signal), otherwise modulos `1 + stroke_holdtime` so periodic
            // stroke-driven curves cycle.
            {
                let stroke_freq = (-dab_sv.get(BrushSetting::StrokeDurationLogarithmic)).exp();
                let stroke_wrap = 1.0 + dab_sv.get(BrushSetting::StrokeHoldtime).max(0.0);
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
            }

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

            // Truthy check matches libmypaint — any non-zero curve value
            // burns 2 PRNG draws so the sequence stays in lock-step even
            // when the curve dips negative (where amplitude clamps to 0).
            let off_random_raw = dab_sv.get(BrushSetting::OffsetByRandom);
            if off_random_raw != 0.0 {
                let off_random = off_random_raw.max(0.0);
                px += state.rng.next_gauss() * off_random * base_radius;
                py += state.rng.next_gauss() * off_random * base_radius;
            }
            // libmypaint's `radius_by_random`: gaussian-jitter
            // `radius_logarithmic` by `noise * setting`, clamp the result
            // into `[ACTUAL_RADIUS_MIN, ACTUAL_RADIUS_MAX]`, and scale
            // opaque by `(orig_radius / new_radius)²` when the new radius
            // is bigger so the perceived ink density stays even. Consumes
            // one PRNG draw — placed *after* `offset_by_random` to keep
            // the consumption order aligned with `prepare_and_draw_dab`.
            // Same truthy gating as offset_by_random above.
            let mut dab_opaque_scale = 1.0_f32;
            let rad_random_raw = dab_sv.get(BrushSetting::RadiusByRandom);
            let rad_random = rad_random_raw.max(0.0);
            if rad_random_raw != 0.0 {
                let noise = state.rng.next_gauss() * rad_random;
                let new_log = dab_sv.get(BrushSetting::Radius) + noise;
                let new_radius = new_log.exp().clamp(0.2, 1000.0);
                let alpha_correction = (dab_radius / new_radius).powi(2);
                if alpha_correction <= 1.0 {
                    dab_opaque_scale = alpha_correction;
                }
                // libmypaint stores the perturbed radius back into
                // STATE.ACTUAL_RADIUS so the next `count_dabs_to` call sees
                // it. `build_dab` reads from `state.actual_radius` for the
                // dab geometry, so updating it here is what feeds the new
                // size through to the renderer.
                state.actual_radius = new_radius;
            }
            let off_speed = dab_sv.get(BrushSetting::OffsetBySpeed);
            if off_speed != 0.0 {
                // libmypaint: `x += NORM_DX_SLOW * offset_by_speed * 0.1 /
                // viewzoom`. Viewzoom is 1.0 here.
                px += state.norm_dx_slow * off_speed * 0.1;
                py += state.norm_dy_slow * off_speed * 0.1;
            }

            // libmypaint's `update_smudge_color`: lazy canvas resample
            // (controlled by `smudge_length_log` / recentness counter),
            // then the legacy/spectral mix into the smudge bucket. The
            // sampled colour can also gate-out the whole dab via
            // `smudge_transparency`.
            // libmypaint evaluates SMUDGE and SMUDGE_LENGTH per-dab.
            let smudge_amt = dab_sv.get(BrushSetting::Smudge).clamp(0.0, 1.0);
            let smudge_length = dab_sv.get(BrushSetting::SmudgeLength).clamp(0.0, 1.0);
            let mut skip_dab = false;
            // libmypaint: enter update_smudge_color when
            //   smudge_length < 1.0 && (SMUDGE != 0 || mapping not constant)
            // — not gated on smudge_amt itself, so a curve-driven smudge
            // that hits 0 at one dab still decays its recentness counter
            // and stays in step with the reference.
            if smudge_length < 1.0 && smudge_mapping_active {
                let smudge_length_log = dab_sv.get(BrushSetting::SmudgeLengthLog);
                let mut update_factor = smudge_length.max(0.01);

                // Decay the existing recentness; if it falls below the
                // libmypaint threshold we resample the canvas.
                let recentness = state.prev_col_recentness * update_factor;
                state.prev_col_recentness = recentness;
                let threshold = (0.5 * update_factor).powf(smudge_length_log).min(1.0) + 1e-16;

                let (sr, sg, sb, sa) = if recentness < threshold {
                    // First call after a long pause initialises the
                    // bucket directly with the sample.
                    if recentness == 0.0 {
                        update_factor = 0.0;
                    }
                    state.prev_col_recentness = 1.0;

                    let smudge_radius_log = dab_sv.get(BrushSetting::SmudgeRadiusLog);
                    // libmypaint feeds the post-radius_by_random radius
                    // into update_smudge_color (`radius` at
                    // mypaint-brush.c:1044, reassigned in the
                    // radius_by_random branch a few lines earlier). For
                    // brushes whose noise meaningfully shifts the radius
                    // the smudge sample needs to scale with it.
                    let smudge_radius =
                        (state.actual_radius * smudge_radius_log.exp()).clamp(0.2, 1000.0);
                    // libmypaint's `update_smudge_color` passes
                    // `legacy_smudge ? -1.0 : paint_factor` to
                    // `surface2_get_color`. paint_mode > 0 triggers
                    // the spectral averaging; otherwise legacy
                    // sampling (mask-weighted linear, straight alpha).
                    let paint_for_sample = dab_sv.get(BrushSetting::Paint).clamp(0.0, 1.0);
                    let sample = if paint_for_sample > 0.0 {
                        surface.get_color_pigment(px, py, smudge_radius, paint_for_sample)
                    } else {
                        surface.get_color(px, py, smudge_radius)
                    };

                    // `smudge_transparency` gates the dab on the
                    // sampled canvas alpha. Positive: skip when sample
                    // is *more* transparent than the threshold;
                    // negative: skip when *more* opaque than the
                    // mirror threshold.
                    let smudge_op_lim = dab_sv.get(BrushSetting::SmudgeTransparency);
                    if (smudge_op_lim > 0.0 && sample.a < smudge_op_lim)
                        || (smudge_op_lim < 0.0 && sample.a > -smudge_op_lim)
                    {
                        skip_dab = true;
                    }
                    state.prev_col_r = sample.r;
                    state.prev_col_g = sample.g;
                    state.prev_col_b = sample.b;
                    state.prev_col_a = sample.a;
                    (sample.r, sample.g, sample.b, sample.a)
                } else {
                    (
                        state.prev_col_r,
                        state.prev_col_g,
                        state.prev_col_b,
                        state.prev_col_a,
                    )
                };

                if !skip_dab {
                    let fac = update_factor;
                    let paint_mode = dab_sv.get(BrushSetting::Paint).clamp(0.0, 1.0);
                    if paint_mode > 0.0 {
                        if sa > 0.01 {
                            let prev = [
                                state.smudge_ra,
                                state.smudge_ga,
                                state.smudge_ba,
                                state.smudge_a,
                            ];
                            let cur = [sr, sg, sb, sa];
                            let mixed = crate::spectral::mix_colors(prev, cur, fac, paint_mode);
                            state.smudge_ra = mixed[0];
                            state.smudge_ga = mixed[1];
                            state.smudge_ba = mixed[2];
                            state.smudge_a = mixed[3];
                        } else {
                            state.smudge_a = (state.smudge_a + sa) * 0.5;
                        }
                    } else {
                        // Legacy smudge: SMUDGE_R += (1-fac)*a*r.
                        let fac_new = (1.0 - fac) * sa;
                        state.smudge_ra = state.smudge_ra * fac + sr * fac_new;
                        state.smudge_ga = state.smudge_ga * fac + sg * fac_new;
                        state.smudge_ba = state.smudge_ba * fac + sb * fac_new;
                        state.smudge_a = (state.smudge_a * fac + (1.0 - fac) * sa).clamp(0.0, 1.0);
                    }
                }
            }

            // libmypaint computes the dab colour entirely inside
            // `prepare_and_draw_dab`: brush base → apply_smudge → eraser
            // → HSV/HSL dynamics, in that order. We do the same in
            // `build_dab` now (it took `&mut state` for `state.actual_*`
            // bookkeeping) — no separate per-dab drift pass.
            let dab = build_dab(self, &dab_sv, state, px, py, smudge_amt, dab_opaque_scale);
            if !skip_dab && surface.draw_dab(&dab) {
                painted = true;
            }
            state.last_dab_x = dab.x;
            state.last_dab_y = dab.y;
            // libmypaint refreshes `random_input` once per dab, *after*
            // drawing.
            state.random_input = state.rng.next_unit();

            dtime_left = (dtime_left - step_dtime).max(0.0);
            // libmypaint's count_dabs_to reads STATE.ACTUAL_ELLIPTICAL_DAB_*
            // (which were assigned SETTING() values per-dab in
            // update_states_and_setting_values lines 807-809), so the
            // recount inside the loop uses the freshly-evaluated per-dab
            // aspect / angle. Match that here.
            let next_aspect = dab_sv.get(BrushSetting::EllipticalDabRatio).max(1.0);
            let next_angle_rad = dab_sv.get(BrushSetting::EllipticalDabAngle).to_radians();
            dabs_todo = count_dabs_to(
                cur_x,
                cur_y,
                target_x,
                target_y,
                dab_radius,
                base_radius,
                dpar,
                dpbr,
                dps,
                dtime_left,
                next_angle_rad,
                next_aspect,
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

// Color dynamics are folded directly into `build_dab` so they apply
// *after* `apply_smudge` — matching libmypaint's order in
// `prepare_and_draw_dab`.

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
    state: &mut BrushState,
    px: f32,
    py: f32,
    smudge_amt: f32,
    // Per-dab opacity scaler from `radius_by_random`: when the noise
    // produced a larger radius, libmypaint scales `opaque` by
    // `(orig_radius / new_radius)²` to keep ink density even.
    opaque_scale: f32,
) -> Dab {
    // libmypaint's `prepare_and_draw_dab` does the full colour
    // pipeline per dab in this exact order:
    //   brush BASEVAL(COLOR_*) → apply_smudge → eraser → HSV/HSL dynamics
    // Hokusai used to keep `state.actual_*` as a running accumulator and
    // do drift before smudge, which only matters when the brush actually
    // sets `change_color_*` AND `smudge` together — but several brushes
    // in the upstream pack do exactly that. Reordering here matches
    // libmypaint.
    let base_h = brush.get(BrushSetting::ColorH).base_value;
    let base_s = brush.get(BrushSetting::ColorS).base_value;
    let base_v = brush.get(BrushSetting::ColorV).base_value;
    let base = hsv_to_rgb(Hsv {
        h: base_h,
        s: base_s,
        v: base_v,
    });

    // 1) apply_smudge: derive eraser_target_alpha + mix the brush
    //    colour with the smudge bucket. See the libmypaint
    //    `apply_smudge` helper for the legacy / spectral split.
    let smudge_amt = smudge_amt.clamp(0.0, 1.0);
    let paint_mode = sv.get(BrushSetting::Paint).clamp(0.0, 1.0);
    let eraser_target_alpha = ((1.0 - smudge_amt) + smudge_amt * state.smudge_a).clamp(0.0, 1.0);
    let mixed_rgb = if smudge_amt <= 0.0 || eraser_target_alpha <= 0.0 {
        [base.r, base.g, base.b]
    } else if paint_mode > 0.0 {
        let smudge_color = [
            state.smudge_ra,
            state.smudge_ga,
            state.smudge_ba,
            state.smudge_a,
        ];
        let brush_color = [base.r, base.g, base.b, 1.0];
        let mixed = crate::spectral::mix_colors(smudge_color, brush_color, smudge_amt, paint_mode);
        [
            mixed[0].clamp(0.0, 1.0),
            mixed[1].clamp(0.0, 1.0),
            mixed[2].clamp(0.0, 1.0),
        ]
    } else {
        let col_factor = 1.0 - smudge_amt;
        [
            ((smudge_amt * state.smudge_ra + col_factor * base.r) / eraser_target_alpha)
                .clamp(0.0, 1.0),
            ((smudge_amt * state.smudge_ga + col_factor * base.g) / eraser_target_alpha)
                .clamp(0.0, 1.0),
            ((smudge_amt * state.smudge_ba + col_factor * base.b) / eraser_target_alpha)
                .clamp(0.0, 1.0),
        ]
    };

    // 2) HSV / HSL color dynamics on the *post-smudge* colour. Matches
    //    libmypaint's order — running drift was already removed; this
    //    is the per-dab delta on whatever apply_smudge produced.
    let dh = sv.get(BrushSetting::ChangeColorH);
    let dv = sv.get(BrushSetting::ChangeColorV);
    let dhsv_s = sv.get(BrushSetting::ChangeColorHsvS);
    let dl = sv.get(BrushSetting::ChangeColorL);
    let dhsl_s = sv.get(BrushSetting::ChangeColorHslS);
    let mut color_r = mixed_rgb[0];
    let mut color_g = mixed_rgb[1];
    let mut color_b = mixed_rgb[2];
    if dh != 0.0 || dhsv_s != 0.0 || dv != 0.0 {
        let mut hsv = crate::color::rgb_to_hsv(color_r, color_g, color_b);
        hsv.h = (hsv.h + dh).rem_euclid(1.0);
        hsv.s = (hsv.s + hsv.s * hsv.v * dhsv_s).clamp(0.0, 1.0);
        hsv.v = (hsv.v + dv).clamp(0.0, 1.0);
        let rgb = hsv_to_rgb(hsv);
        color_r = rgb.r;
        color_g = rgb.g;
        color_b = rgb.b;
    }
    if dl != 0.0 || dhsl_s != 0.0 {
        let mut hsl = crate::color::rgb_to_hsl(color_r, color_g, color_b);
        hsl.l = (hsl.l + dl).clamp(0.0, 1.0);
        let edge = (1.0 - hsl.l).abs().min(hsl.l.abs());
        hsl.s = (hsl.s + hsl.s * edge * 2.0 * dhsl_s).clamp(0.0, 1.0);
        let rgb = crate::color::hsl_to_rgb(hsl);
        color_r = rgb.r;
        color_g = rgb.g;
        color_b = rgb.b;
    }

    // Diagnostic / test bookkeeping: the legacy `state.actual_*` fields
    // now record the last dab's post-dynamics colour.
    let hsv_out = crate::color::rgb_to_hsv(color_r, color_g, color_b);
    state.actual_h = hsv_out.h;
    state.actual_s = hsv_out.s;
    state.actual_v = hsv_out.v;

    let color = crate::color::RgbaF32 {
        r: color_r,
        g: color_g,
        b: color_b,
        a: 1.0,
    };

    // libmypaint composes the final opacity as opaque * opaque_multiply.
    // Many stock brushes (charcoal, pencil, …) drive opaque_multiply from
    // pressure, so skipping it makes them look wrong at non-full pressure.
    // libmypaint defaults opaque_multiply to 1.0; treat a wholly-default
    // setting (no base value and no input curves) as that identity.
    // libmypaint applies only MAX(0.0, ...) to OPAQUE before multiplying
    // with OPAQUE_MULTIPLY (mypaint-brush.c:957). The final product is
    // clamped to [0, 1] downstream.
    let opaque_raw = sv.get(BrushSetting::Opaque).max(0.0);
    let opaque_mult = opaque_multiplier(brush, sv);
    let mut opaque = (opaque_raw * opaque_mult * opaque_scale).clamp(0.0, 1.0);

    // libmypaint's `opaque_linearize` compensates for the fact that
    // overlapping dabs accumulate alpha non-linearly: the per-dab alpha
    // is rooted by `1/dabs_per_pixel` so the *aggregate* opacity at the
    // dab center matches `opaque`. Brushes like the stock round brush
    // (`opaque_linearize=0.44`) rely on this to dim their feathered edges
    // without going full opaque — without it, hokusai's tails of a
    // pressure ramp keep painting at the unmodulated `opaque` value while
    // libmypaint's drop to near zero.
    let opaque_linearize = brush.get(BrushSetting::OpaqueLinearize).base_value.max(0.0);
    if opaque_linearize > 0.0 && opaque > 0.0 {
        // libmypaint's non-legacy path reads DABS_PER_* from STATE
        // (mypaint-brush.c:970), which is the per-dab SETTING value
        // (assigned from SETTING(...) at line 628-630). The legacy
        // path uses BASEVAL. We always sample SETTING via dab_sv
        // here so brushes whose dabs_per_* are curve-driven (Round#1
        // uses brush_radius on DABS_PER_ACTUAL_RADIUS) compute the
        // opacity correction the same way the non-legacy reference
        // does.
        let dpar = sv.get(BrushSetting::DabsPerActualRadius);
        let dpbr = sv.get(BrushSetting::DabsPerBasicRadius);
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

    // libmypaint's `snap_to_pixel`: pull the dab centre toward
    // (floor(x)+0.5, floor(y)+0.5) and quantise the radius to half-pixels
    // by the snap fraction. At snap=1.0 the dab lands exactly on a pixel
    // centre with a radius that doesn't bleed into a 4th neighbour.
    let mut px = px;
    let mut py = py;
    let snap = sv.get(BrushSetting::SnapToPixel).clamp(0.0, 1.0);
    if snap > 0.0 {
        let snapped_x = px.floor() + 0.5;
        let snapped_y = py.floor() + 0.5;
        px += (snapped_x - px) * snap;
        py += (snapped_y - py) * snap;
        let mut snapped_radius = (radius * 2.0).round() * 0.5;
        if snapped_radius < 0.5 {
            snapped_radius = 0.5;
        }
        if snap > 0.9999 {
            // libmypaint sheds a hair off the quantised radius so the
            // mask doesn't touch the 4th neighbour pixel through fp slop.
            snapped_radius -= 0.0001;
        }
        radius += (snapped_radius - radius) * snap;
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
    let stroke_angle_deg = direction_angle_dy.atan2(direction_angle_dx).to_degrees() - 90.0;
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

/// libmypaint's `INPUT(DIRECTION)` — 180°-folded direction in *degrees*.
/// `mod_arith(degrees(atan2(dy, dx)) + viewrotation + 180, 180)` with
/// `viewrotation = 0`. The output range is `[0, 180)`; declared as
/// `hard_minimum=0, hard_maximum=180` in libmypaint's
/// `brushsettings.json`.
fn direction_input(dx: f32, dy: f32) -> f32 {
    if dx == 0.0 && dy == 0.0 {
        0.0
    } else {
        (dy.atan2(dx).to_degrees() + 180.0).rem_euclid(180.0)
    }
}

/// libmypaint's `INPUT(DIRECTION_ANGLE)` — full 360° direction in *degrees*.
/// `fmodf(degrees(atan2(dy, dx)) + viewrotation + 360, 360)` with
/// `viewrotation = 0`. Output range `[0, 360)`.
fn direction_angle(dx: f32, dy: f32) -> f32 {
    if dx == 0.0 && dy == 0.0 {
        0.0
    } else {
        (dy.atan2(dx).to_degrees() + 360.0).rem_euclid(360.0)
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
        assert!(
            surf.count > 0,
            "stroke_threshold no longer gates dab emission"
        );
        assert!(
            !state.stroke_started,
            "0.3 < threshold 0.5, started stays off"
        );

        // Above threshold (after a seed pass): started flips on and
        // stroke_state restarts at 0.
        let mut s2 = BrushState::default();
        let mut surf2 = CountingSurface { count: 0 };
        // First call always goes through the seed branch.
        brush.stroke_to(&mut s2, &mut surf2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.01);
        s2.stroke_state = 0.7;
        brush.stroke_to(&mut s2, &mut surf2, 1.0, 0.0, 0.8, 0.0, 0.0, 0.01);
        assert!(
            s2.stroke_started,
            "pressure above threshold sets started=true"
        );
        assert_eq!(
            s2.stroke_state, 0.0,
            "rising-edge reset wipes prior stroke_state"
        );
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
        // libmypaint's HSV drift operates on `rgb_to_hsv(color)` — pure
        // black/grey has no defined hue, so the delta needs a saturated
        // base colour to be observable. We seed the brush as red and
        // verify the per-dab change_color_h actually rotates it.
        let mut brush = make_brush(1.0, 2.0);
        brush.set(BrushSetting::ColorH, SettingValue::constant(0.0)); // red
        brush.set(BrushSetting::ColorS, SettingValue::constant(1.0));
        brush.set(BrushSetting::ColorV, SettingValue::constant(1.0));
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
