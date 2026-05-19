//! Color types. Internal pipeline is linear sRGB; HSV is used at brush
//! configuration boundaries (matches libmypaint).

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RgbaF32 {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl RgbaF32 {
    pub const TRANSPARENT: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Hsv {
    pub h: f32, // [0, 1)
    pub s: f32, // [0, 1]
    pub v: f32, // [0, 1]
}

// sRGB transfer fns. libmypaint uses the standard piecewise IEC 61966-2-1.
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Hsl {
    pub h: f32, // [0, 1)
    pub s: f32, // [0, 1]
    pub l: f32, // [0, 1]
}

pub fn rgb_to_hsv(r: f32, g: f32, b: f32) -> Hsv {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let v = max;
    let s = if max <= 0.0 { 0.0 } else { d / max };
    let h = if d <= 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    Hsv { h, s, v }
}

pub fn rgb_to_hsl(r: f32, g: f32, b: f32) -> Hsl {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    let d = max - min;
    let s = if d <= 0.0 {
        0.0
    } else if l < 0.5 {
        d / (max + min)
    } else {
        d / (2.0 - max - min)
    };
    let h = if d <= 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    Hsl { h, s, l }
}

pub fn hsl_to_rgb(hsl: Hsl) -> RgbaF32 {
    if hsl.s <= 0.0 {
        return RgbaF32::new(hsl.l, hsl.l, hsl.l, 1.0);
    }
    let q = if hsl.l < 0.5 {
        hsl.l * (1.0 + hsl.s)
    } else {
        hsl.l + hsl.s - hsl.l * hsl.s
    };
    let p = 2.0 * hsl.l - q;
    let h = hsl.h.rem_euclid(1.0);
    let to_rgb = |t: f32| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    RgbaF32::new(to_rgb(h + 1.0 / 3.0), to_rgb(h), to_rgb(h - 1.0 / 3.0), 1.0)
}

pub fn hsv_to_rgb(hsv: Hsv) -> RgbaF32 {
    let h = (hsv.h.rem_euclid(1.0)) * 6.0;
    let i = h.floor() as i32;
    let f = h - i as f32;
    let p = hsv.v * (1.0 - hsv.s);
    let q = hsv.v * (1.0 - hsv.s * f);
    let t = hsv.v * (1.0 - hsv.s * (1.0 - f));
    let (r, g, b) = match i.rem_euclid(6) {
        0 => (hsv.v, t, p),
        1 => (q, hsv.v, p),
        2 => (p, hsv.v, t),
        3 => (p, q, hsv.v),
        4 => (t, p, hsv.v),
        _ => (hsv.v, p, q),
    };
    RgbaF32::new(r, g, b, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn hsv_roundtrip() {
        for &(r, g, b) in &[
            (1.0, 0.0, 0.0),
            (0.0, 1.0, 0.0),
            (0.0, 0.0, 1.0),
            (0.3, 0.6, 0.9),
            (0.5, 0.5, 0.5),
        ] {
            let back = hsv_to_rgb(rgb_to_hsv(r, g, b));
            assert!(
                approx(back.r, r) && approx(back.g, g) && approx(back.b, b),
                "HSV roundtrip lost ({r},{g},{b}) → ({},{},{})",
                back.r,
                back.g,
                back.b
            );
        }
    }

    #[test]
    fn hsl_roundtrip() {
        for &(r, g, b) in &[
            (1.0, 0.0, 0.0),
            (0.0, 1.0, 0.5),
            (0.2, 0.4, 0.7),
            (0.5, 0.5, 0.5),
            (0.0, 0.0, 0.0),
        ] {
            let back = hsl_to_rgb(rgb_to_hsl(r, g, b));
            assert!(
                approx(back.r, r) && approx(back.g, g) && approx(back.b, b),
                "HSL roundtrip lost ({r},{g},{b}) → ({},{},{})",
                back.r,
                back.g,
                back.b
            );
        }
    }
}
