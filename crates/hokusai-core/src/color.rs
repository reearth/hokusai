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
