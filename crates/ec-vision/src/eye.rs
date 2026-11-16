//! One eye: where it is in the frame, and the level patch the warp model looks at.
//!
//! The warp model expects a fixed framing:
//! a 96 x 64 patch centred between the two eye corners, level with them, and
//! 1 / 0.55 times as wide as the distance between them. [`EyeBox`] is that
//! rectangle placed on the camera frame; everything else here cuts pixels out of it.

use ec_core::{Frame, Rect};

use crate::points;

/// The size of the picture the warp model sees.
pub const PATCH_W: usize = 96;
pub const PATCH_H: usize = 64;
/// Eye-corner distance over patch width. The model was made for exactly this share.
const CORNER_SHARE: f32 = 0.55;
/// Corners closer than this (in frame pixels) make an eye too small to be worth correcting.
const MIN_CORNER_DISTANCE: f32 = 10.0;
/// Each model pixel is the average of this many samples across and down, so a patch
/// that is shrunk from the camera picture is averaged, not picked out pixel by pixel.
const SAMPLES: usize = 2;

/// Which eye, by its side of the picture (not of the person: an unmirrored camera
/// shows the person's right eye on the left).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub const BOTH: [Side; 2] = [Side::Left, Side::Right];

    /// 0 for the picture-left eye, 1 for the other: the order used in every `[_; 2]` here.
    pub fn index(self) -> usize {
        match self {
            Side::Left => 0,
            Side::Right => 1,
        }
    }

    /// The model knows one eye only: the one whose nose-side corner is on the right
    /// of the picture. The other eye is shown to it as a mirror image.
    pub fn mirrored(self) -> bool {
        self == Side::Right
    }
}

/// What the face points say about one eye. Frame pixels.
#[derive(Clone, Copy, Debug)]
pub struct Eye {
    pub side: Side,
    /// The eye corner nearer the left of the picture, then the other one.
    pub left_corner: [f32; 2],
    pub right_corner: [f32; 2],
    /// Middle of the iris.
    pub iris: [f32; 2],
    /// Width of the iris, in pixels. A human iris is very close to 11.7 mm across,
    /// so this doubles as a ruler.
    pub iris_diameter: f32,
}

impl Eye {
    /// Read one eye out of the 478 face points. `None` if the points are missing.
    pub fn from_points(p: &[[f32; 3]], side: Side) -> Option<Eye> {
        let (a, b, iris) = match side {
            Side::Left => (
                points::LEFT_EYE_OUTER,
                points::LEFT_EYE_INNER,
                points::LEFT_IRIS,
            ),
            Side::Right => (
                points::RIGHT_EYE_INNER,
                points::RIGHT_EYE_OUTER,
                points::RIGHT_IRIS,
            ),
        };
        let at = |i: usize| p.get(i).map(|q| [q[0], q[1]]);
        let dist =
            |a: [f32; 2], b: [f32; 2]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
        // the iris points are: centre, then four points round its edge, opposite ones two apart
        let c = iris.start;
        let across = dist(at(c + 1)?, at(c + 3)?);
        let down = dist(at(c + 2)?, at(c + 4)?);
        Some(Eye {
            side,
            left_corner: at(a)?,
            right_corner: at(b)?,
            iris: at(c)?,
            iris_diameter: (across + down) / 2.0,
        })
    }
}

/// The level rectangle around an eye that the warp model looks at, placed on the frame.
///
/// Positions inside it are written (u, v), each from 0 to 1: (0, 0) is its
/// top-left corner and (1, 1) its bottom-right.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EyeBox {
    /// Frame position of the top-left corner.
    pub origin: [f32; 2],
    /// The step from the left edge to the right edge, in frame pixels.
    pub across: [f32; 2],
    /// The step from the top edge to the bottom edge.
    pub down: [f32; 2],
}

impl EyeBox {
    /// The box for an eye with these two corners (picture-left corner first).
    /// `None` if the corners are too close together to be a usable eye.
    pub fn around(left_corner: [f32; 2], right_corner: [f32; 2]) -> Option<EyeBox> {
        let d = [
            right_corner[0] - left_corner[0],
            right_corner[1] - left_corner[1],
        ];
        let corner_distance = (d[0] * d[0] + d[1] * d[1]).sqrt();
        if !corner_distance.is_finite() || corner_distance < MIN_CORNER_DISTANCE {
            return None;
        }
        let width = corner_distance / CORNER_SHARE;
        let height = width * PATCH_H as f32 / PATCH_W as f32;
        // unit vectors along the eye and square to it (pointing down the picture)
        let ex = [d[0] / corner_distance, d[1] / corner_distance];
        let ey = [-ex[1], ex[0]];
        let across = [ex[0] * width, ex[1] * width];
        let down = [ey[0] * height, ey[1] * height];
        let mid = [
            (left_corner[0] + right_corner[0]) / 2.0,
            (left_corner[1] + right_corner[1]) / 2.0,
        ];
        Some(EyeBox {
            origin: [
                mid[0] - (across[0] + down[0]) / 2.0,
                mid[1] - (across[1] + down[1]) / 2.0,
            ],
            across,
            down,
        })
    }

    pub fn from_eye(eye: &Eye) -> Option<EyeBox> {
        EyeBox::around(eye.left_corner, eye.right_corner)
    }

    /// Width of the box in frame pixels.
    pub fn width(&self) -> f32 {
        (self.across[0].powi(2) + self.across[1].powi(2)).sqrt()
    }

    /// A position in the box to frame pixels.
    #[inline]
    pub fn to_frame(&self, u: f32, v: f32) -> [f32; 2] {
        [
            self.origin[0] + u * self.across[0] + v * self.down[0],
            self.origin[1] + u * self.across[1] + v * self.down[1],
        ]
    }

    /// A frame position to its (u, v) in the box. Outside the box the numbers
    /// simply run below 0 or above 1.
    #[inline]
    pub fn to_patch(&self, x: f32, y: f32) -> (f32, f32) {
        let (dx, dy) = (x - self.origin[0], y - self.origin[1]);
        let w2 = self.across[0].powi(2) + self.across[1].powi(2);
        let h2 = self.down[0].powi(2) + self.down[1].powi(2);
        (
            (dx * self.across[0] + dy * self.across[1]) / w2,
            (dx * self.down[0] + dy * self.down[1]) / h2,
        )
    }

    /// The upright frame rectangle that holds the whole box, with two pixels to
    /// spare, cut off at the frame's edges. `None` if the box's middle is not in
    /// the frame: an eye that is half out of the picture is left alone.
    pub fn bounds(&self, frame_w: u32, frame_h: u32) -> Option<Rect> {
        let mid = self.to_frame(0.5, 0.5);
        let (fw, fh) = (frame_w as f32, frame_h as f32);
        if !(0.0..fw).contains(&mid[0]) || !(0.0..fh).contains(&mid[1]) {
            return None;
        }
        let corners = [
            self.to_frame(0.0, 0.0),
            self.to_frame(1.0, 0.0),
            self.to_frame(0.0, 1.0),
            self.to_frame(1.0, 1.0),
        ];
        let low = |i: usize| corners.iter().map(|c| c[i]).fold(f32::INFINITY, f32::min);
        let high = |i: usize| {
            corners
                .iter()
                .map(|c| c[i])
                .fold(f32::NEG_INFINITY, f32::max)
        };
        let x0 = (low(0).floor() - 2.0).clamp(0.0, fw) as u32;
        let y0 = (low(1).floor() - 2.0).clamp(0.0, fh) as u32;
        let x1 = (high(0).ceil() + 2.0).clamp(0.0, fw) as u32;
        let y1 = (high(1).ceil() + 2.0).clamp(0.0, fh) as u32;
        (x1 > x0 && y1 > y0).then_some(Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }
}

/// The part of the frame under an eye box, as RGB. Only this region is ever
/// converted from the camera's format.
#[derive(Clone, Debug)]
pub struct EyeCrop {
    pub rect: Rect,
    /// `rect.w * rect.h * 3` bytes.
    pub rgb: Vec<u8>,
}

impl EyeCrop {
    pub fn cut(frame: &Frame, eye_box: &EyeBox) -> Option<EyeCrop> {
        let rect = eye_box.bounds(frame.width(), frame.height())?;
        let rgb = frame.crop_rgb(rect).ok()?;
        Some(EyeCrop { rect, rgb })
    }

    /// The colour at a frame position (0 to 255 per channel), blended from the four
    /// pixels around it. A pixel's middle is at its index plus a half, so pixel 3
    /// covers 3.0 to 4.0. Positions outside the crop get its nearest edge pixel.
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> [f32; 3] {
        let (w, h) = (self.rect.w as usize, self.rect.h as usize);
        let fx = (x - 0.5 - self.rect.x as f32).clamp(0.0, (w - 1) as f32);
        let fy = (y - 0.5 - self.rect.y as f32).clamp(0.0, (h - 1) as f32);
        let (x0, y0) = (fx as usize, fy as usize);
        let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let px = |x: usize, y: usize| &self.rgb[(y * w + x) * 3..][..3];
        let (a, b, c, d) = (px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1));
        let mut out = [0.0f32; 3];
        for (i, o) in out.iter_mut().enumerate() {
            let top = a[i] as f32 + (b[i] as f32 - a[i] as f32) * tx;
            let bottom = c[i] as f32 + (d[i] as f32 - c[i] as f32) * tx;
            *o = top + (bottom - top) * ty;
        }
        out
    }
}

/// The eye as the warp model wants it: three planes (red, green, blue) of
/// 64 rows x 96 columns, values 0 to 1. With `mirrored`, flipped left to right.
pub fn model_input(crop: &EyeCrop, eye_box: &EyeBox, mirrored: bool) -> Vec<f32> {
    let plane = PATCH_W * PATCH_H;
    let mut out = vec![0.0f32; 3 * plane];
    let scale = 1.0 / (SAMPLES * SAMPLES) as f32 / 255.0;
    for row in 0..PATCH_H {
        for col in 0..PATCH_W {
            let mut sum = [0.0f32; 3];
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let u = (col as f32 + (sx as f32 + 0.5) / SAMPLES as f32) / PATCH_W as f32;
                    let v = (row as f32 + (sy as f32 + 0.5) / SAMPLES as f32) / PATCH_H as f32;
                    let p = eye_box.to_frame(u, v);
                    let c = crop.sample(p[0], p[1]);
                    for i in 0..3 {
                        sum[i] += c[i];
                    }
                }
            }
            let to_col = if mirrored { PATCH_W - 1 - col } else { col };
            for i in 0..3 {
                out[i * plane + row * PATCH_W + to_col] = sum[i] * scale;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::PixelFormat;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn the_box_is_centred_level_and_as_wide_as_the_model_expects() {
        let b = EyeBox::around([100.0, 50.0], [155.0, 50.0]).unwrap();
        // corners 55 apart -> 100 wide, and 3:2
        assert!(close(b.width(), 100.0));
        assert_eq!(b.to_frame(0.5, 0.5), [127.5, 50.0]);
        assert!(close(b.to_frame(0.0, 0.0)[0], 77.5));
        assert!(close(
            b.to_frame(0.0, 0.0)[1],
            50.0 - 100.0 * 64.0 / 96.0 / 2.0
        ));
        // the corners sit at 22.5% and 77.5% of the width, on the middle line
        let (u, v) = b.to_patch(100.0, 50.0);
        assert!(close(u, 0.225) && close(v, 0.5), "{u} {v}");
    }

    #[test]
    fn a_tilted_eye_gives_a_tilted_box_and_the_two_mappings_undo_each_other() {
        let b = EyeBox::around([100.0, 50.0], [140.0, 80.0]).unwrap();
        for (u, v) in [(0.0, 0.0), (1.0, 0.25), (0.3, 0.9)] {
            let p = b.to_frame(u, v);
            let (u2, v2) = b.to_patch(p[0], p[1]);
            assert!(close(u, u2) && close(v, v2));
        }
        // "down" in the box is square to the line through the corners
        assert!(close(
            b.across[0] * b.down[0] + b.across[1] * b.down[1],
            0.0
        ));
        assert!(b.down[1] > 0.0);
    }

    #[test]
    fn tiny_or_off_frame_eyes_are_refused() {
        assert!(EyeBox::around([10.0, 10.0], [15.0, 10.0]).is_none());
        let b = EyeBox::around([-80.0, 20.0], [-30.0, 20.0]).unwrap();
        assert!(b.bounds(640, 480).is_none());
        let b = EyeBox::around([100.0, 50.0], [155.0, 50.0]).unwrap();
        let r = b.bounds(640, 480).unwrap();
        assert!(r.x <= 76 && r.x + r.w >= 179 && r.y <= 15 && r.y + r.h >= 85);
    }

    /// A picture whose red is its column and green its row.
    fn ramp(w: u32, h: u32) -> Frame {
        let mut d = Vec::new();
        for y in 0..h {
            for x in 0..w {
                d.extend_from_slice(&[x as u8, y as u8, 7]);
            }
        }
        Frame::new(w, h, PixelFormat::Rgb24, d, 0).unwrap()
    }

    #[test]
    fn sampling_reads_pixel_middles_and_blends_between_them() {
        let f = ramp(200, 100);
        let b = EyeBox::around([100.0, 50.0], [155.0, 50.0]).unwrap();
        let crop = EyeCrop::cut(&f, &b).unwrap();
        assert_eq!(crop.sample(120.5, 40.5), [120.0, 40.0, 7.0]);
        let s = crop.sample(121.0, 40.5);
        assert!(close(s[0], 120.5) && close(s[1], 40.0));
    }

    #[test]
    fn model_input_is_the_averaged_patch_and_mirrors() {
        let f = ramp(200, 100);
        // corners 52.8 apart -> the box is 96 x 64 frame pixels: one model pixel per frame pixel
        let b = EyeBox::around([73.6, 50.0], [126.4, 50.0]).unwrap();
        let crop = EyeCrop::cut(&f, &b).unwrap();
        let plain = model_input(&crop, &b, false);
        let flipped = model_input(&crop, &b, true);
        let plane = PATCH_W * PATCH_H;
        // model pixel (col 10, row 5) covers frame x 62..63, y 23..24
        let i = 5 * PATCH_W + 10;
        assert!(close(plain[i] * 255.0, 62.0), "{}", plain[i] * 255.0);
        assert!(close(plain[plane + i] * 255.0, 23.0));
        assert!(close(plain[2 * plane + i] * 255.0, 7.0));
        assert!(close(flipped[5 * PATCH_W + (PATCH_W - 1 - 10)], plain[i]));
    }

    #[test]
    fn an_eye_is_read_from_the_named_points() {
        let mut p = vec![[0.0f32; 3]; 478];
        p[points::RIGHT_EYE_INNER] = [200.0, 50.0, 0.0];
        p[points::RIGHT_EYE_OUTER] = [260.0, 52.0, 0.0];
        let c = points::RIGHT_IRIS.start;
        p[c] = [231.0, 49.0, 0.0];
        p[c + 1] = [241.0, 49.0, 0.0];
        p[c + 3] = [221.0, 49.0, 0.0];
        p[c + 2] = [231.0, 38.0, 0.0];
        p[c + 4] = [231.0, 60.0, 0.0];
        let e = Eye::from_points(&p, Side::Right).unwrap();
        assert_eq!(e.left_corner, [200.0, 50.0]);
        assert_eq!(e.iris, [231.0, 49.0]);
        assert!(close(e.iris_diameter, 21.0));
        assert!(Eye::from_points(&[], Side::Left).is_none());
    }
}
