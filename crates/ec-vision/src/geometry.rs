//! The square region a model looks at, and cutting it out of a frame.

use ec_core::{Frame, Rect};

/// A square region of the frame, possibly rotated. Pixel units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Roi {
    pub cx: f32,
    pub cy: f32,
    pub side: f32,
    /// Radians, clockwise on screen. The square's "up" follows the face's "up".
    pub rotation: f32,
}

impl Roi {
    /// A point inside the square (`u`, `v` from 0 to 1) to frame pixels.
    pub fn to_frame(&self, u: f32, v: f32) -> (f32, f32) {
        let (lx, ly) = ((u - 0.5) * self.side, (v - 0.5) * self.side);
        let (s, c) = self.rotation.sin_cos();
        (self.cx + lx * c - ly * s, self.cy + lx * s + ly * c)
    }

    /// The square that holds `points` with room to spare, turned so that the line
    /// from `points[level_from]` to `points[level_to]` lies level.
    /// This is how MediaPipe frames a face: 1.5 times the longer side of the box.
    pub fn around(points: &[[f32; 2]], level_from: usize, level_to: usize) -> Option<Roi> {
        let first = points.first()?;
        let (mut x0, mut y0, mut x1, mut y1) = (first[0], first[1], first[0], first[1]);
        for p in points {
            x0 = x0.min(p[0]);
            y0 = y0.min(p[1]);
            x1 = x1.max(p[0]);
            y1 = y1.max(p[1]);
        }
        let (a, b) = (points.get(level_from)?, points.get(level_to)?);
        Some(Roi {
            cx: (x0 + x1) / 2.0,
            cy: (y0 + y1) / 2.0,
            side: (x1 - x0).max(y1 - y0) * 1.5,
            rotation: (b[1] - a[1]).atan2(b[0] - a[0]),
        })
    }

    /// Usable: has a size, and its centre is inside a `width` x `height` frame.
    pub fn is_usable(&self, width: u32, height: u32) -> bool {
        self.side.is_finite()
            && self.side >= 8.0
            && (0.0..width as f32).contains(&self.cx)
            && (0.0..height as f32).contains(&self.cy)
    }
}

/// Cut `roi` out of `frame` as `size` x `size` RGB floats, each `value * scale + offset`.
/// Parts of the square that fall outside the frame are black.
///
/// Only the part of the frame under the square is converted to RGB.
pub(crate) fn cut_square(
    frame: &Frame,
    roi: &Roi,
    size: usize,
    scale: f32,
    offset: f32,
) -> Vec<f32> {
    let mut out = vec![offset; size * size * 3];

    // The upright box around the (possibly rotated) square, kept inside the frame.
    let corners = [
        roi.to_frame(0.0, 0.0),
        roi.to_frame(1.0, 0.0),
        roi.to_frame(0.0, 1.0),
        roi.to_frame(1.0, 1.0),
    ];
    let min_x = corners.iter().map(|c| c.0).fold(f32::INFINITY, f32::min);
    let max_x = corners
        .iter()
        .map(|c| c.0)
        .fold(f32::NEG_INFINITY, f32::max);
    let min_y = corners.iter().map(|c| c.1).fold(f32::INFINITY, f32::min);
    let max_y = corners
        .iter()
        .map(|c| c.1)
        .fold(f32::NEG_INFINITY, f32::max);
    let (fw, fh) = (frame.width() as f32, frame.height() as f32);
    let bx0 = (min_x.floor() - 1.0).clamp(0.0, fw) as u32;
    let by0 = (min_y.floor() - 1.0).clamp(0.0, fh) as u32;
    let bx1 = (max_x.ceil() + 2.0).clamp(0.0, fw) as u32;
    let by1 = (max_y.ceil() + 2.0).clamp(0.0, fh) as u32;
    if bx1 <= bx0 || by1 <= by0 {
        return out;
    }
    let rect = Rect {
        x: bx0,
        y: by0,
        w: bx1 - bx0,
        h: by1 - by0,
    };
    let Ok(rgb) = frame.crop_rgb(rect) else {
        return out;
    };
    let (bw, bh) = (rect.w as usize, rect.h as usize);

    // One output pixel is this many source pixels along each axis of the square.
    let step = roi.side / size as f32;
    let (s, c) = roi.rotation.sin_cos();
    // Source position of output pixel (0, 0)'s centre, measured inside the crop.
    let (ox, oy) = roi.to_frame(0.5 / size as f32, 0.5 / size as f32);
    let (ox, oy) = (ox - 0.5 - bx0 as f32, oy - 0.5 - by0 as f32);

    for (row, line) in out.chunks_exact_mut(size * 3).enumerate() {
        let (rx, ry) = (ox - row as f32 * step * s, oy + row as f32 * step * c);
        for (col, px) in line.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let x = rx + col as f32 * step * c;
            let y = ry + col as f32 * step * s;
            if x <= -1.0 || y <= -1.0 || x >= bw as f32 || y >= bh as f32 {
                continue;
            }
            // Blend the four source pixels around (x, y); outside the crop counts as black.
            let (x0, y0) = (x.floor(), y.floor());
            let (tx, ty) = (x - x0, y - y0);
            let (x0, y0) = (x0 as isize, y0 as isize);
            let mut sum = [0.0f32; 3];
            for (dx, dy, weight) in [
                (0, 0, (1.0 - tx) * (1.0 - ty)),
                (1, 0, tx * (1.0 - ty)),
                (0, 1, (1.0 - tx) * ty),
                (1, 1, tx * ty),
            ] {
                let (sx, sy) = (x0 + dx, y0 + dy);
                if sx < 0 || sy < 0 || sx >= bw as isize || sy >= bh as isize {
                    continue;
                }
                let i = (sy as usize * bw + sx as usize) * 3;
                sum[0] += rgb[i] as f32 * weight;
                sum[1] += rgb[i + 1] as f32 * weight;
                sum[2] += rgb[i + 2] as f32 * weight;
            }
            px[0] = sum[0] * scale + offset;
            px[1] = sum[1] * scale + offset;
            px[2] = sum[2] * scale + offset;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::PixelFormat;

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
    fn an_upright_square_at_native_scale_copies_pixels() {
        let f = ramp(64, 48);
        let roi = Roi {
            cx: 20.0,
            cy: 16.0,
            side: 8.0,
            rotation: 0.0,
        };
        let out = cut_square(&f, &roi, 8, 1.0, 0.0);
        // the square covers columns 16..24 and rows 12..20
        assert_eq!(&out[..3], &[16.0, 12.0, 7.0]);
        assert_eq!(&out[(7 * 8 + 7) * 3..], &[23.0, 19.0, 7.0]);
    }

    #[test]
    fn a_quarter_turn_swaps_the_axes() {
        let f = ramp(64, 48);
        let roi = Roi {
            cx: 20.0,
            cy: 16.0,
            side: 8.0,
            rotation: std::f32::consts::FRAC_PI_2,
        };
        let out = cut_square(&f, &roi, 8, 1.0, 0.0);
        // moving right in the square now moves down in the frame
        let (a, b) = (&out[..3], &out[3..6]);
        assert!((b[1] - a[1] - 1.0).abs() < 1e-3, "{a:?} {b:?}");
        assert!((b[0] - a[0]).abs() < 1e-3, "{a:?} {b:?}");
    }

    #[test]
    fn outside_the_frame_is_black_and_scaled() {
        let f = ramp(16, 16);
        let roi = Roi {
            cx: 0.0,
            cy: 0.0,
            side: 8.0,
            rotation: 0.0,
        };
        let out = cut_square(&f, &roi, 8, 1.0 / 127.5, -1.0);
        assert_eq!(out[0], -1.0); // top-left of the square is off the frame
        let last = &out[(7 * 8 + 7) * 3..];
        assert!((last[0] - (3.0 / 127.5 - 1.0)).abs() < 1e-5);
    }

    #[test]
    fn roi_round_trip_and_framing() {
        let roi = Roi {
            cx: 100.0,
            cy: 50.0,
            side: 40.0,
            rotation: 0.3,
        };
        assert_eq!(roi.to_frame(0.5, 0.5), (100.0, 50.0));
        let pts = [[10.0, 20.0], [30.0, 20.0], [20.0, 60.0]];
        let r = Roi::around(&pts, 0, 1).unwrap();
        assert_eq!((r.cx, r.cy, r.side, r.rotation), (20.0, 40.0, 60.0, 0.0));
        assert!(Roi::around(&[], 0, 1).is_none());
    }
}
