//! Keeping the small white light in the eye (the reflection of a lamp or window)
//! its own size and shape while the iris is turned.
//!
//! The warp model moves the iris by squeezing and stretching the picture. The
//! reflection sits on the iris, so it gets squeezed too: at a strong turn it
//! shrinks or nearly vanishes, and an eye without its light looks flat and
//! unnatural (Berkay, live, 2026-11-15).
//!
//! A real reflection does not behave like paint on the iris. It sits on the
//! clear dome in front of it and, when the eye turns, slides only about half as
//! far as the iris does, without changing shape. So it is handled apart:
//! 1. [`lift`] finds the reflection, takes it out of the eye picture (leaving the
//!    dark iris that was under it) and keeps it as a separate layer of light;
//! 2. the eye without its reflection is turned as usual;
//! 3. the layer is laid back on top, moved as one piece by half the iris's move.

use crate::eye::{Eye, EyeCrop};

/// A reflection is a bright spot narrower than this share of the iris's width.
/// Anything wider (the white of the eye, a lid) is not a reflection.
const MAX_WIDTH: f32 = 0.34;
/// Looked for only on the iris itself: in full up to this share of the iris's
/// width from its middle...
const DISC_FULL: f32 = 0.4;
/// ...fading to nothing at the iris's edge (half its width). Just outside it
/// there are small bright corners of eye-white between the iris and a lid, which
/// look like reflections to this test but must move with the eye, not stay put.
const DISC_ZERO: f32 = 0.5;
/// How much brighter than its surroundings a spot must be (0 to 255) to count
/// at all, and to count in full. Iris texture and camera noise stay below this.
const LIGHT_ZERO: f32 = 14.0;
const LIGHT_FULL: f32 = 34.0;
/// The share of the iris's move that the reflection follows. From the eye's
/// shape: the dome's centre is about half as far from the eye's turning point
/// as the iris is.
pub const FOLLOW: f32 = 0.5;

/// The reflection of one eye as a layer of extra light, cut from the frame.
#[derive(Clone, Debug)]
pub struct Highlight {
    /// Frame position of the layer's top-left pixel, and its size.
    x: u32,
    y: u32,
    w: usize,
    h: usize,
    /// `w * h * 3` values: how much light to add per colour, 0 to 255.
    layer: Vec<f32>,
    /// Frame position of the middle of the light.
    pub centre: [f32; 2],
}

impl Highlight {
    /// The extra light at a frame position, blended from the four layer pixels
    /// around it. Nothing outside the layer.
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> [f32; 3] {
        let fx = x - 0.5 - self.x as f32;
        let fy = y - 0.5 - self.y as f32;
        if fx <= -1.0 || fy <= -1.0 || fx >= self.w as f32 || fy >= self.h as f32 {
            return [0.0; 3];
        }
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        // a layer pixel, or nothing if that spot is outside the layer
        let px = |dx: f32, dy: f32| -> [f32; 3] {
            let (cx, cy) = (x0 + dx, y0 + dy);
            if cx < 0.0 || cy < 0.0 || cx >= self.w as f32 || cy >= self.h as f32 {
                return [0.0; 3];
            }
            let i = (cy as usize * self.w + cx as usize) * 3;
            [self.layer[i], self.layer[i + 1], self.layer[i + 2]]
        };
        let (a, b, c, d) = (px(0.0, 0.0), px(1.0, 0.0), px(0.0, 1.0), px(1.0, 1.0));
        let mut out = [0.0f32; 3];
        for (i, o) in out.iter_mut().enumerate() {
            let top = a[i] + (b[i] - a[i]) * tx;
            let bottom = c[i] + (d[i] - c[i]) * tx;
            *o = top + (bottom - top) * ty;
        }
        out
    }
}

/// For every pixel, the smallest (or largest) value within `reach` pixels along
/// its row, then the same down its column: a square window, done in two cheap passes.
fn spread(src: &[f32], w: usize, h: usize, reach: usize, pick: fn(f32, f32) -> f32) -> Vec<f32> {
    let mut rows = vec![0.0f32; src.len()];
    for y in 0..h {
        for x in 0..w {
            let (lo, hi) = (x.saturating_sub(reach), (x + reach).min(w - 1));
            for c in 0..3 {
                let mut best = src[(y * w + lo) * 3 + c];
                for k in lo + 1..=hi {
                    best = pick(best, src[(y * w + k) * 3 + c]);
                }
                rows[(y * w + x) * 3 + c] = best;
            }
        }
    }
    let mut out = vec![0.0f32; src.len()];
    for y in 0..h {
        let (lo, hi) = (y.saturating_sub(reach), (y + reach).min(h - 1));
        for x in 0..w {
            for c in 0..3 {
                let mut best = rows[(lo * w + x) * 3 + c];
                for k in lo + 1..=hi {
                    best = pick(best, rows[(k * w + x) * 3 + c]);
                }
                out[(y * w + x) * 3 + c] = best;
            }
        }
    }
    out
}

/// 0 at or below `zero`, 1 at or above `full`, smooth between.
fn smooth_rise(x: f32, zero: f32, full: f32) -> f32 {
    let t = ((x - zero) / (full - zero)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Find the reflection on `eye`'s iris, take it out of `crop` and return it.
/// `None` (and `crop` untouched) if there is no reflection to speak of.
pub fn lift(crop: &mut EyeCrop, eye: &Eye) -> Option<Highlight> {
    let d = eye.iris_diameter;
    if !d.is_finite() || d < 6.0 {
        return None; // too small in the picture to tell a reflection from noise
    }
    // "Narrower than MAX_WIDTH" as a window: a spot that fits inside a window of
    // 2 * reach + 1 pixels disappears when the darkest value in the window is taken.
    let reach = ((MAX_WIDTH * d - 1.0) / 2.0).ceil().clamp(1.0, 16.0) as usize;
    // The square to work on: the disc, plus room for the window to look around.
    let half = DISC_ZERO * d + reach as f32 + 1.0;
    let (cw, ch) = (crop.rect.w as f32, crop.rect.h as f32);
    let (ix, iy) = (
        eye.iris[0] - crop.rect.x as f32,
        eye.iris[1] - crop.rect.y as f32,
    );
    let x0 = (ix - half).floor().clamp(0.0, cw) as usize;
    let y0 = (iy - half).floor().clamp(0.0, ch) as usize;
    let x1 = (ix + half).ceil().clamp(0.0, cw) as usize;
    let y1 = (iy + half).ceil().clamp(0.0, ch) as usize;
    if x1 <= x0 + 2 || y1 <= y0 + 2 {
        return None;
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let stride = crop.rect.w as usize;

    let mut picture = vec![0.0f32; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                picture[(y * w + x) * 3 + c] =
                    crop.rgb[((y0 + y) * stride + x0 + x) * 3 + c] as f32;
            }
        }
    }
    // Darkest-in-window, then brightest-in-window of that: the picture with every
    // small bright spot removed and everything larger left as it was.
    let without = spread(
        &spread(&picture, w, h, reach, f32::min),
        w,
        h,
        reach,
        f32::max,
    );

    let mut layer = vec![0.0f32; w * h * 3];
    let (mut sum, mut sum_x, mut sum_y) = (0.0f32, 0.0f32, 0.0f32);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 3;
            let extra = [
                picture[i] - without[i],
                picture[i + 1] - without[i + 1],
                picture[i + 2] - without[i + 2],
            ];
            // brightness the way the eye weighs the three colours
            let light = 0.299 * extra[0] + 0.587 * extra[1] + 0.114 * extra[2];
            let (px, py) = ((x0 + x) as f32 + 0.5 - ix, (y0 + y) as f32 + 0.5 - iy);
            let from_middle = px.hypot(py) / d;
            let keep = smooth_rise(light, LIGHT_ZERO, LIGHT_FULL)
                * (1.0 - smooth_rise(from_middle, DISC_FULL, DISC_ZERO));
            if keep <= 0.0 {
                continue;
            }
            for c in 0..3 {
                layer[i + c] = extra[c].max(0.0) * keep;
            }
            let weight = light * keep;
            sum += weight;
            sum_x += weight * ((x0 + x) as f32 + 0.5);
            sum_y += weight * ((y0 + y) as f32 + 0.5);
        }
    }
    if sum < LIGHT_FULL {
        return None; // not even one pixel's worth of reflection
    }
    // Take the light out of the eye picture; what stays is the iris under it.
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let at = ((y0 + y) * stride + x0 + x) * 3 + c;
                let left = crop.rgb[at] as f32 - layer[(y * w + x) * 3 + c];
                crop.rgb[at] = left.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Some(Highlight {
        x: crop.rect.x + x0 as u32,
        y: crop.rect.y + y0 as u32,
        w,
        h,
        layer,
        centre: [
            crop.rect.x as f32 + sum_x / sum,
            crop.rect.y as f32 + sum_y / sum,
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eye::Side;
    use ec_core::Rect;

    /// A dark iris 40 px wide on a light eye-white, with or without a 5 x 4 reflection.
    fn eye_picture(with_light: bool) -> (EyeCrop, Eye) {
        let (w, h) = (120usize, 80usize);
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let from_iris = (x as f32 + 0.5 - 60.0).hypot(y as f32 + 0.5 - 40.0);
                let mut v = if from_iris < 20.0 { 40 } else { 200 };
                if with_light && (63..68).contains(&x) && (33..37).contains(&y) {
                    v = 240;
                }
                rgb[(y * w + x) * 3..][..3].copy_from_slice(&[v, v, v]);
            }
        }
        let crop = EyeCrop {
            rect: Rect {
                x: 300,
                y: 100,
                w: w as u32,
                h: h as u32,
            },
            rgb,
        };
        let eye = Eye {
            side: Side::Left,
            left_corner: [320.0, 140.0],
            right_corner: [400.0, 140.0],
            iris: [360.0, 140.0],
            iris_diameter: 40.0,
        };
        (crop, eye)
    }

    #[test]
    fn a_reflection_is_lifted_off_and_the_iris_under_it_is_left() {
        let (mut crop, eye) = eye_picture(true);
        let light = lift(&mut crop, &eye).expect("there is a reflection");
        // its middle: columns 63..68 and rows 33..37 of the crop, in frame pixels
        assert!((light.centre[0] - 365.5).abs() < 0.01, "{:?}", light.centre);
        assert!((light.centre[1] - 135.0).abs() < 0.01, "{:?}", light.centre);
        // the layer holds exactly the extra light: 240 - 40
        assert_eq!(light.sample(365.5, 135.5), [200.0; 3]);
        assert_eq!(light.sample(350.5, 150.5), [0.0; 3]);
        assert_eq!(light.sample(10.0, 10.0), [0.0; 3]);
        // and the eye picture is now the same as one that never had a reflection
        let (plain, _) = eye_picture(false);
        assert_eq!(crop.rgb, plain.rgb);
    }

    #[test]
    fn an_eye_without_a_reflection_is_not_touched() {
        let (mut crop, eye) = eye_picture(false);
        let before = crop.rgb.clone();
        assert!(lift(&mut crop, &eye).is_none());
        assert_eq!(crop.rgb, before);
    }

    #[test]
    fn the_white_of_the_eye_is_not_a_reflection() {
        // an iris half covered by a light lid: large bright areas, no small spot
        let (mut crop, eye) = eye_picture(false);
        for y in 0..34 {
            for x in 0..120 {
                crop.rgb[(y * 120 + x) * 3..][..3].copy_from_slice(&[210, 170, 150]);
            }
        }
        let before = crop.rgb.clone();
        assert!(lift(&mut crop, &eye).is_none());
        assert_eq!(crop.rgb, before);
    }
}
