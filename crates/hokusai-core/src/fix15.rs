//! Fixed-point pixel math matching libmypaint's `fix15` representation.
//!
//! Values in [0.0, 1.0] are stored as `u16` in [0, 32768]. Multiplies use a
//! `u32` intermediate with rounding identical to libmypaint's
//! `((a * b) + (1 << 14)) >> 15`, which is the basis of pixel-level parity.

pub const FIX15_ONE: u32 = 1 << 15;
pub const FIX15_HALF: u32 = 1 << 14;
pub const FIX15_MAX_U16: u16 = FIX15_ONE as u16;

// Float ops aren't allowed in `const fn` until Rust 1.82; keep these
// plain `fn` so the workspace MSRV (1.77) holds. The compiler still
// inlines them in release builds.
#[inline]
pub fn from_f32(v: f32) -> u16 {
    let scaled = v * FIX15_ONE as f32;
    let clamped = if scaled < 0.0 {
        0.0
    } else if scaled > FIX15_ONE as f32 {
        FIX15_ONE as f32
    } else {
        scaled
    };
    (clamped + 0.5) as u16
}

#[inline]
pub fn to_f32(v: u16) -> f32 {
    v as f32 / FIX15_ONE as f32
}

/// `(a * b + 0.5) / 1.0` in fix15 with libmypaint-compatible rounding.
#[inline]
pub const fn mul(a: u32, b: u32) -> u32 {
    (a * b + FIX15_HALF) >> 15
}

/// Saturating add (clamps to FIX15_ONE).
#[inline]
pub const fn add_sat(a: u32, b: u32) -> u32 {
    let s = a + b;
    if s > FIX15_ONE {
        FIX15_ONE
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_times_one_is_one() {
        assert_eq!(mul(FIX15_ONE, FIX15_ONE), FIX15_ONE);
    }

    #[test]
    fn half_times_half_is_quarter() {
        let half = FIX15_ONE / 2;
        let q = mul(half, half);
        assert!((q as i32 - (FIX15_ONE as i32 / 4)).abs() <= 1);
    }
}
