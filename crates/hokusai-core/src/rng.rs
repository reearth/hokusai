//! Mersenne Twister PRNG (MT19937).
//!
//! libmypaint uses GLib's `GRand`, which is itself MT19937. The state
//! transition implemented here matches the reference MT19937 paper
//! (Matsumoto & Nishimura 1998). Bit-exact parity with `GRand`'s
//! `g_rand_double` / `g_rand_int_range` mappings is a separate concern —
//! tracked in M3 once we have libmypaint output to compare against.

const N: usize = 624;
const M: usize = 397;
const MATRIX_A: u32 = 0x9908_B0DF;
const UPPER_MASK: u32 = 0x8000_0000;
const LOWER_MASK: u32 = 0x7FFF_FFFF;

#[derive(Debug, Clone)]
pub struct BrushRng {
    mt: [u32; N],
    index: usize,
}

impl BrushRng {
    /// Seed using the standard MT19937 initialiser
    /// (Knuth's `mt[i] = 1812433253 * (mt[i-1] ^ (mt[i-1] >> 30)) + i`).
    pub fn new(seed: u32) -> Self {
        let mut mt = [0u32; N];
        mt[0] = seed;
        for i in 1..N {
            mt[i] = 1812433253u32
                .wrapping_mul(mt[i - 1] ^ (mt[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        Self { mt, index: N }
    }

    fn generate(&mut self) {
        for i in 0..N {
            let y = (self.mt[i] & UPPER_MASK) | (self.mt[(i + 1) % N] & LOWER_MASK);
            let mag = if y & 1 != 0 { MATRIX_A } else { 0 };
            self.mt[i] = self.mt[(i + M) % N] ^ (y >> 1) ^ mag;
        }
        self.index = 0;
    }

    pub fn next_u32(&mut self) -> u32 {
        if self.index >= N {
            self.generate();
        }
        let mut y = self.mt[self.index];
        self.index += 1;
        // Tempering.
        y ^= y >> 11;
        y ^= (y << 7) & 0x9D2C_5680;
        y ^= (y << 15) & 0xEFC6_0000;
        y ^= y >> 18;
        y
    }

    /// Uniform [0, 1).
    pub fn next_unit(&mut self) -> f32 {
        // 24-bit mantissa — matches the precision libmypaint actually uses.
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Uniform [0, 1) as f64.
    pub fn next_unit_f64(&mut self) -> f64 {
        // Combine two u32 outputs as MT19937 does for double precision.
        let a = (self.next_u32() >> 5) as u64; // 27 bits
        let b = (self.next_u32() >> 6) as u64; // 26 bits
        (a as f64 * 67108864.0 + b as f64) / 9007199254740992.0
    }

    /// libmypaint's `rand_gauss`: sum of four `g_rand_double` samples,
    /// then `* 0.5 - 1.0`. This is intentionally **not** a true N(0,1) —
    /// it has stddev ~0.289 — but matches the upstream distribution
    /// brushes were authored against.
    pub fn next_gauss(&mut self) -> f32 {
        let s = self.next_unit_f64()
            + self.next_unit_f64()
            + self.next_unit_f64()
            + self.next_unit_f64();
        (s * 0.5 - 1.0) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference MT19937 output for seed 5489 (Matsumoto's published values).
    #[test]
    fn matches_reference_seed_5489() {
        let mut r = BrushRng::new(5489);
        let expected = [
            0xD091BB5C_u32,
            0x22AE9EF6,
            0xE7E1FAEE,
            0xD5C31F79,
            0x2082352C,
        ];
        for &e in &expected {
            assert_eq!(r.next_u32(), e);
        }
    }

    #[test]
    fn unit_in_range() {
        let mut r = BrushRng::new(42);
        for _ in 0..1000 {
            let v = r.next_unit();
            assert!((0.0..1.0).contains(&v));
        }
    }

    #[test]
    fn gauss_distribution_matches_libmypaint() {
        let mut r = BrushRng::new(123);
        let mut sum = 0.0f64;
        let mut sum_sq = 0.0f64;
        let n = 20_000;
        for _ in 0..n {
            let v = r.next_gauss() as f64;
            sum += v;
            sum_sq += v * v;
        }
        let mean = sum / n as f64;
        let var = sum_sq / n as f64 - mean * mean;
        let std = var.sqrt();
        // Mean ≈ 0, stddev ≈ 1/sqrt(12) ≈ 0.289 (libmypaint's choice).
        assert!(mean.abs() < 0.02, "mean = {mean}");
        assert!(
            (std - 0.289).abs() < 0.02,
            "stddev = {std}, expected ~0.289"
        );
    }
}
