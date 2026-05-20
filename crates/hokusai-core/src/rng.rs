// The loops in this module mirror libmypaint's `rng-double.c` line-for-line;
// rewriting them with iterator combinators would obscure the C correspondence.
#![allow(clippy::needless_range_loop, clippy::manual_memcpy)]

//! libmypaint-compatible PRNG.
//!
//! Port of Knuth's TAOCP 3.6 exercise 3.6-15 lagged-Fibonacci-via-`mod_sum`
//! generator as shipped in libmypaint as `rng-double.c`. libmypaint
//! initialises this with seed `1000` for every fresh brush, then consumes
//! it for the `Random` input, dab-position jitter (`offset_by_random`,
//! `tracking_noise`) and `radius_by_random`. Matching the sequence is
//! required for any byte-exact parity on those brushes.
//!
//! The previous MT19937-based generator produced statistically similar
//! gaussians but the wrong sequence and a 1/√3 too-small standard
//! deviation (`rand_gauss` here uses `sum * √3 − 2√3`, not the older
//! `sum * 0.5 − 1`). That mismatch alone showed up as ≈ 0.8–1.0 MAD on
//! charcoal fixtures.

const KK: usize = 10; // long lag
const LL: usize = 7; // short lag
const TT: i32 = 7; // guaranteed separation between streams
const QUALITY: usize = 19; // buffer fill per cycle (>= KK + LL - 1)

#[inline]
fn mod_sum(x: f64, y: f64) -> f64 {
    let s = x + y;
    s - s.trunc()
}

#[derive(Debug, Clone)]
pub struct BrushRng {
    ran_u: [f64; KK],
    buf: [f64; QUALITY],
    buf_pos: usize,
}

impl BrushRng {
    /// Create a generator with libmypaint's default seed of `1000`.
    pub fn new(seed: u32) -> Self {
        let mut rng = Self {
            ran_u: [0.0; KK],
            buf: [0.0; QUALITY],
            // QUALITY signals "buffer exhausted; cycle on next read".
            buf_pos: QUALITY,
        };
        rng.set_seed(seed as i64);
        rng
    }

    fn set_seed(&mut self, seed: i64) {
        let mut u = [0.0_f64; KK + KK - 1];
        let ulp = (1.0_f64 / (1u64 << 30) as f64) / (1u64 << 22) as f64; // 2^-52
        let mut ss = 2.0 * ulp * ((seed & 0x3fff_ffff) as f64 + 2.0);
        for j in 0..KK {
            u[j] = ss;
            ss += ss;
            if ss >= 1.0 {
                ss -= 1.0 - 2.0 * ulp;
            }
        }
        u[1] += ulp; // make u[1] (and only u[1]) "odd"
        let mut s = seed & 0x3fff_ffff;
        let mut t = TT - 1;
        loop {
            if t == 0 {
                break;
            }
            // "square"
            for j in (1..KK).rev() {
                u[j + j] = u[j];
                u[j + j - 1] = 0.0;
            }
            for j in (KK..=KK + KK - 2).rev() {
                u[j - (KK - LL)] = mod_sum(u[j - (KK - LL)], u[j]);
                u[j - KK] = mod_sum(u[j - KK], u[j]);
            }
            if s & 1 != 0 {
                // "multiply by z" — cyclic shift the buffer
                for j in (1..=KK).rev() {
                    u[j] = u[j - 1];
                }
                u[0] = u[KK];
                u[LL] = mod_sum(u[LL], u[KK]);
            }
            if s != 0 {
                s >>= 1;
            } else {
                t -= 1;
            }
        }
        for j in 0..LL {
            self.ran_u[j + KK - LL] = u[j];
        }
        for j in LL..KK {
            self.ran_u[j - LL] = u[j];
        }
        // Warm-up: run `get_array` 10 times into a scratch buffer
        // (libmypaint discards these values).
        let mut warmup = [0.0_f64; KK + KK - 1];
        for _ in 0..10 {
            get_array(&mut self.ran_u, &mut warmup, KK + KK - 1);
        }
        // Force a buffer cycle on the next `next_unit_f64` call.
        self.buf_pos = QUALITY;
    }

    fn cycle(&mut self) -> f64 {
        get_array(&mut self.ran_u, &mut self.buf, QUALITY);
        self.buf_pos = 1;
        self.buf[0]
    }

    /// Uniform [0, 1) as `f64` — libmypaint's `rng_double_next`.
    pub fn next_unit_f64(&mut self) -> f64 {
        // libmypaint stops reading from the buffer once it has consumed `KK`
        // values (it overwrites `buf[KK]` with a sentinel). Mirror that.
        if self.buf_pos >= KK {
            return self.cycle();
        }
        let v = self.buf[self.buf_pos];
        self.buf_pos += 1;
        v
    }

    /// Uniform [0, 1) as `f32`.
    pub fn next_unit(&mut self) -> f32 {
        self.next_unit_f64() as f32
    }

    /// libmypaint's `rand_gauss`: sum of four `rng_double_next` samples,
    /// scaled by `sqrt(3)` and shifted by `2*sqrt(3)` to approximate a
    /// standard normal (mean 0, stddev ≈ 1).
    pub fn next_gauss(&mut self) -> f32 {
        let s = self.next_unit_f64()
            + self.next_unit_f64()
            + self.next_unit_f64()
            + self.next_unit_f64();
        // Constants come straight from libmypaint's helpers.c — they are
        // sqrt(3) and 2*sqrt(3) truncated to 11 decimals.
        const SCALE: f64 = 1.732_050_807_57;
        const OFFSET: f64 = 3.464_101_615_14;
        (s * SCALE - OFFSET) as f32
    }
}

impl Default for BrushRng {
    fn default() -> Self {
        // libmypaint seeds the brush's RNG with 1000.
        Self::new(1000)
    }
}

fn get_array(ran_u: &mut [f64; KK], aa: &mut [f64], n: usize) {
    for j in 0..KK {
        aa[j] = ran_u[j];
    }
    for j in KK..n {
        aa[j] = mod_sum(aa[j - KK], aa[j - LL]);
    }
    // libmypaint then walks `i` over [0, KK) while `j` continues from `n`,
    // refilling `ran_u`. The first LL entries fold two `aa` values; the
    // remaining KK - LL fold the freshly written `ran_u`.
    for i in 0..LL {
        let j = n + i;
        ran_u[i] = mod_sum(aa[j - KK], aa[j - LL]);
    }
    for i in LL..KK {
        let j = n + i;
        ran_u[i] = mod_sum(aa[j - KK], ran_u[i - LL]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_unit_in_range() {
        let mut r = BrushRng::new(1000);
        for _ in 0..1000 {
            let v = r.next_unit_f64();
            assert!((0.0..1.0).contains(&v), "out of range: {v}");
        }
    }

    /// `rand_gauss` should land in roughly `mean=0, stddev=1` —
    /// the libmypaint formula sums four `u[0,1)` then scales by `sqrt(3)`.
    #[test]
    fn gauss_distribution() {
        let mut r = BrushRng::new(1000);
        let mut sum = 0.0_f64;
        let mut sum_sq = 0.0_f64;
        let n = 20_000;
        for _ in 0..n {
            let v = r.next_gauss() as f64;
            sum += v;
            sum_sq += v * v;
        }
        let mean = sum / n as f64;
        let var = sum_sq / n as f64 - mean * mean;
        let std = var.sqrt();
        assert!(mean.abs() < 0.05, "mean = {mean}");
        assert!((std - 1.0).abs() < 0.05, "stddev = {std}, expected ~1.0");
    }
}
