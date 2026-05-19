//! Deterministic PRNG matching libmypaint's `helpers.c` `rand_gauss`/`g_rand`.
//!
//! libmypaint relies on GLib's `GRand` (a Mersenne Twister variant). For
//! pixel-level parity we'll port the exact algorithm in M2; for now this stub
//! exposes the surface the stroke engine will call into.

#[derive(Debug, Clone)]
pub struct BrushRng {
    state: u64,
}

impl BrushRng {
    pub const fn new(seed: u64) -> Self {
        Self { state: seed | 1 }
    }

    /// xorshift64* placeholder. Will be replaced by GRand-compatible MT in M2.
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        ((x.wrapping_mul(0x2545_F491_4F6C_DD1D)) >> 32) as u32
    }

    /// Uniform [0, 1).
    pub fn next_unit(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }
}
