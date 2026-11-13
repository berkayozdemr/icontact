//! Colour conversion between the camera's formats and packed RGB.
//!
//! Uses BT.601 "video range" numbers (Y 16..235), the usual webcam default.
//! OPEN (T-022): which matrix and range each real camera uses has not been
//! measured. A camera that sends BT.709 or full range will show a small colour
//! and contrast shift in the preview until that is read from the source.

use crate::frame::{Frame, PixelFormat, Rect};

#[inline(always)]
fn clamp8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// One YUV sample to RGB.
#[inline(always)]
pub fn yuv_to_rgb(y: u8, u: u8, v: u8) -> [u8; 3] {
    let c = 298 * (y as i32 - 16) + 128;
    let d = u as i32 - 128;
    let e = v as i32 - 128;
    [
        clamp8((c + 409 * e) >> 8),
        clamp8((c - 100 * d - 208 * e) >> 8),
        clamp8((c + 516 * d) >> 8),
    ]
}

/// One RGB pixel to YUV.
#[inline(always)]
pub fn rgb_to_yuv(r: u8, g: u8, b: u8) -> [u8; 3] {
    let (r, g, b) = (r as i32, g as i32, b as i32);
    [
        clamp8(((66 * r + 129 * g + 25 * b + 128) >> 8) + 16),
        clamp8(((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128),
        clamp8(((112 * r - 94 * g - 18 * b + 128) >> 8) + 128),
    ]
}

/// Fill `out` (`rect.w * rect.h * 3` bytes) with the RGB of `rect`.
/// The caller has checked that `rect` lies inside the frame.
pub(crate) fn region_to_rgb(frame: &Frame, rect: Rect, out: &mut [u8]) {
    let w = frame.width() as usize;
    let h = frame.height() as usize;
    let d = frame.data();
    let (rx, ry, rw, rh) = (
        rect.x as usize,
        rect.y as usize,
        rect.w as usize,
        rect.h as usize,
    );
    debug_assert_eq!(out.len(), rw * rh * 3);

    for (row, line) in out.chunks_exact_mut(rw * 3).enumerate() {
        let y = ry + row;
        match frame.format() {
            PixelFormat::Rgb24 => {
                let start = (y * w + rx) * 3;
                line.copy_from_slice(&d[start..start + rw * 3]);
            }
            PixelFormat::Bgr24 => {
                let start = (y * w + rx) * 3;
                for (px, src) in line
                    .as_chunks_mut::<3>()
                    .0
                    .iter_mut()
                    .zip(d[start..start + rw * 3].as_chunks::<3>().0)
                {
                    px[0] = src[2];
                    px[1] = src[1];
                    px[2] = src[0];
                }
            }
            PixelFormat::Nv12 => {
                let luma = &d[y * w..y * w + w];
                let uv = &d[w * h + (y / 2) * w..w * h + (y / 2) * w + w];
                for (i, px) in line.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                    let x = rx + i;
                    let c = x & !1;
                    px.copy_from_slice(&yuv_to_rgb(luma[x], uv[c], uv[c + 1]));
                }
            }
            PixelFormat::I420 => {
                let (cw, ch) = (w / 2, h / 2);
                let luma = &d[y * w..y * w + w];
                let u = &d[w * h + (y / 2) * cw..w * h + (y / 2) * cw + cw];
                let v = &d[w * h + cw * ch + (y / 2) * cw..w * h + cw * ch + (y / 2) * cw + cw];
                for (i, px) in line.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                    let x = rx + i;
                    px.copy_from_slice(&yuv_to_rgb(luma[x], u[x / 2], v[x / 2]));
                }
            }
            PixelFormat::Yuyv => {
                let src = &d[y * w * 2..y * w * 2 + w * 2];
                for (i, px) in line.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                    let x = rx + i;
                    let pair = (x & !1) * 2;
                    px.copy_from_slice(&yuv_to_rgb(src[x * 2], src[pair + 1], src[pair + 3]));
                }
            }
        }
    }
}

/// Packed RGB to I420. Each colour sample is the average of its 2x2 block.
/// `width` and `height` must be even.
pub fn rgb_to_i420(rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    assert!(
        w % 2 == 0 && h % 2 == 0 && rgb.len() == w * h * 3,
        "rgb_to_i420: bad size"
    );
    let (cw, ch) = (w / 2, h / 2);
    let mut out = vec![0u8; w * h + 2 * cw * ch];
    let (luma, chroma) = out.split_at_mut(w * h);
    let (u_plane, v_plane) = chroma.split_at_mut(cw * ch);
    for cy in 0..ch {
        for cx in 0..cw {
            let (mut su, mut sv) = (0u32, 0u32);
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let (x, y) = (cx * 2 + dx, cy * 2 + dy);
                let p = (y * w + x) * 3;
                let [yy, u, v] = rgb_to_yuv(rgb[p], rgb[p + 1], rgb[p + 2]);
                luma[y * w + x] = yy;
                su += u as u32;
                sv += v as u32;
            }
            u_plane[cy * cw + cx] = ((su + 2) / 4) as u8;
            v_plane[cy * cw + cx] = ((sv + 2) / 4) as u8;
        }
    }
    out
}

/// I420 to NV12: same samples, the two colour planes interleaved.
pub fn i420_to_nv12(i420: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let n = (w / 2) * (h / 2);
    assert_eq!(i420.len(), w * h + 2 * n, "i420_to_nv12: bad size");
    let mut out = Vec::with_capacity(i420.len());
    out.extend_from_slice(&i420[..w * h]);
    for i in 0..n {
        out.push(i420[w * h + i]);
        out.push(i420[w * h + n + i]);
    }
    out
}

/// I420 to YUYV. Each colour sample is used for the two rows it covers.
pub fn i420_to_yuyv(i420: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let (cw, n) = (w / 2, (w / 2) * (h / 2));
    assert_eq!(i420.len(), w * h + 2 * n, "i420_to_yuyv: bad size");
    let mut out = Vec::with_capacity(w * h * 2);
    for y in 0..h {
        for cx in 0..cw {
            let c = (y / 2) * cw + cx;
            out.extend_from_slice(&[
                i420[y * w + cx * 2],
                i420[w * h + c],
                i420[y * w + cx * 2 + 1],
                i420[w * h + n + c],
            ]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A smooth picture: colour changes slowly, so half-size colour planes lose little.
    fn smooth_rgb(w: usize, h: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                v.extend_from_slice(&[
                    (40 + x * 150 / w) as u8,
                    (60 + y * 120 / h) as u8,
                    (200 - (x + y) * 100 / (w + h)) as u8,
                ]);
            }
        }
        v
    }

    fn max_diff(a: &[u8], b: &[u8]) -> u8 {
        assert_eq!(a.len(), b.len());
        a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap()
    }

    #[test]
    fn primaries_round_trip_within_rounding() {
        for rgb in [
            [255, 255, 255],
            [0, 0, 0],
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [128, 128, 128],
            [200, 150, 90],
        ] {
            let [y, u, v] = rgb_to_yuv(rgb[0], rgb[1], rgb[2]);
            let back = yuv_to_rgb(y, u, v);
            assert!(max_diff(&rgb, &back) <= 3, "{rgb:?} -> {back:?}");
        }
    }

    #[test]
    fn video_range_black_and_white() {
        assert_eq!(rgb_to_yuv(0, 0, 0), [16, 128, 128]);
        assert_eq!(rgb_to_yuv(255, 255, 255), [235, 128, 128]);
        assert_eq!(yuv_to_rgb(16, 128, 128), [0, 0, 0]);
        assert_eq!(yuv_to_rgb(235, 128, 128), [255, 255, 255]);
    }

    #[test]
    fn every_yuv_layout_gives_the_same_picture() {
        let (w, h) = (64u32, 48u32);
        let rgb = smooth_rgb(w as usize, h as usize);
        let i420 = rgb_to_i420(&rgb, w, h);
        let a = Frame::new(w, h, PixelFormat::I420, i420.clone(), 0)
            .unwrap()
            .to_rgb();
        let b = Frame::new(w, h, PixelFormat::Nv12, i420_to_nv12(&i420, w, h), 0)
            .unwrap()
            .to_rgb();
        let c = Frame::new(w, h, PixelFormat::Yuyv, i420_to_yuyv(&i420, w, h), 0)
            .unwrap()
            .to_rgb();
        assert_eq!(a, b, "NV12 differs from I420");
        assert_eq!(a, c, "YUYV differs from I420");
        assert!(
            max_diff(&a, &rgb) <= 6,
            "round trip drifted by {}",
            max_diff(&a, &rgb)
        );
    }

    #[test]
    fn bgr_is_rgb_swapped() {
        let f = Frame::new(2, 1, PixelFormat::Bgr24, vec![1, 2, 3, 4, 5, 6], 0).unwrap();
        assert_eq!(f.to_rgb(), vec![3, 2, 1, 6, 5, 4]);
    }

    #[test]
    fn crop_equals_the_same_region_of_the_whole() {
        let (w, h) = (64u32, 48u32);
        let rgb = smooth_rgb(w as usize, h as usize);
        let i420 = rgb_to_i420(&rgb, w, h);
        let frames = [
            Frame::new(w, h, PixelFormat::I420, i420.clone(), 0).unwrap(),
            Frame::new(w, h, PixelFormat::Nv12, i420_to_nv12(&i420, w, h), 0).unwrap(),
            Frame::new(w, h, PixelFormat::Yuyv, i420_to_yuyv(&i420, w, h), 0).unwrap(),
            Frame::new(w, h, PixelFormat::Rgb24, rgb.clone(), 0).unwrap(),
        ];
        // odd offsets and sizes on purpose: the half-size colour planes are where an off-by-one hides
        let r = Rect {
            x: 7,
            y: 5,
            w: 21,
            h: 13,
        };
        for f in &frames {
            let whole = f.to_rgb();
            let crop = f.crop_rgb(r).unwrap();
            for row in 0..r.h as usize {
                let start = ((r.y as usize + row) * w as usize + r.x as usize) * 3;
                assert_eq!(
                    &crop[row * r.w as usize * 3..(row + 1) * r.w as usize * 3],
                    &whole[start..start + r.w as usize * 3],
                    "{:?} row {row}",
                    f.format()
                );
            }
        }
    }
}
