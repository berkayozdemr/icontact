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

/// Mix `rgb` into `rect` of a frame's bytes, in the frame's own format.
///
/// `alpha` holds one byte per pixel of `rect`: 0 keeps the frame's pixel, 255
/// replaces it, values between mix the two. A pixel with alpha 0 is never
/// written, so everything outside the mask stays bit-for-bit as the camera sent it.
///
/// The YUV formats store colour once per block of pixels (2x2, or 2x1 for YUYV).
/// A block's colour moves by the average pull of its pixels, and a block with
/// no pulled pixel is left alone.
///
/// The caller has checked that `rect` lies inside the frame and that `rgb` and
/// `alpha` have the right lengths.
pub(crate) fn blend_region(
    data: &mut [u8],
    width: u32,
    height: u32,
    format: PixelFormat,
    rect: Rect,
    rgb: &[u8],
    alpha: &[u8],
) {
    let (w, h) = (width as usize, height as usize);
    let (rx, ry, rw, rh) = (
        rect.x as usize,
        rect.y as usize,
        rect.w as usize,
        rect.h as usize,
    );
    debug_assert_eq!(rgb.len(), rw * rh * 3);
    debug_assert_eq!(alpha.len(), rw * rh);
    // old + (new - old) * a / 255, rounded to nearest
    let mix = |old: u8, new: u8, a: u8| -> u8 {
        let step = (new as i32 - old as i32) * a as i32;
        (old as i32 + (step + step.signum() * 127) / 255) as u8
    };

    if matches!(format, PixelFormat::Rgb24 | PixelFormat::Bgr24) {
        let swap = format == PixelFormat::Bgr24;
        for row in 0..rh {
            for col in 0..rw {
                let a = alpha[row * rw + col];
                if a == 0 {
                    continue;
                }
                let src = &rgb[(row * rw + col) * 3..][..3];
                let dst = &mut data[((ry + row) * w + rx + col) * 3..][..3];
                for (c, new) in src.iter().enumerate() {
                    let d = if swap { 2 - c } else { c };
                    dst[d] = mix(dst[d], *new, a);
                }
            }
        }
        return;
    }

    // How many pixels share one colour sample, across and down.
    let (block_w, block_h) = match format {
        PixelFormat::Yuyv => (2, 1),
        _ => (2, 2),
    };
    // Where brightness and the two colour bytes of a block live in each layout.
    let luma_at = |x: usize, y: usize| match format {
        PixelFormat::Yuyv => (y * w + x) * 2,
        _ => y * w + x,
    };
    let chroma_at = |cx: usize, cy: usize| match format {
        PixelFormat::Nv12 => {
            let i = w * h + cy * w + cx * 2;
            (i, i + 1)
        }
        PixelFormat::I420 => {
            let cw = w / 2;
            let i = w * h + cy * cw + cx;
            (i, i + cw * (h / 2))
        }
        _ => {
            let i = (cy * w + cx * 2) * 2;
            (i + 1, i + 3)
        }
    };

    // First pass: brightness per pixel, and for each colour block the summed pull
    // towards the new colour (alpha times the difference), for U and for V.
    let (bx0, by0) = (rx / block_w, ry / block_h);
    let blocks_w = (rx + rw - 1) / block_w - bx0 + 1;
    let blocks_h = (ry + rh - 1) / block_h - by0 + 1;
    let mut pull = vec![(0i32, 0i32, false); blocks_w * blocks_h];
    for row in 0..rh {
        for col in 0..rw {
            let a = alpha[row * rw + col];
            if a == 0 {
                continue;
            }
            let (x, y) = (rx + col, ry + row);
            let p = (row * rw + col) * 3;
            let [new_y, new_u, new_v] = rgb_to_yuv(rgb[p], rgb[p + 1], rgb[p + 2]);
            let li = luma_at(x, y);
            data[li] = mix(data[li], new_y, a);
            let (cx, cy) = (x / block_w, y / block_h);
            let (ui, vi) = chroma_at(cx, cy);
            let slot = &mut pull[(cy - by0) * blocks_w + (cx - bx0)];
            slot.0 += (new_u as i32 - data[ui] as i32) * a as i32;
            slot.1 += (new_v as i32 - data[vi] as i32) * a as i32;
            slot.2 = true;
        }
    }
    // Second pass: move each touched block's colour by its average pull.
    let per_block = (block_w * block_h * 255) as i32;
    let moved = |old: u8, sum: i32| -> u8 {
        clamp8(old as i32 + (sum + sum.signum() * per_block / 2) / per_block)
    };
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let (su, sv, touched) = pull[by * blocks_w + bx];
            if !touched {
                continue;
            }
            let (ui, vi) = chroma_at(bx0 + bx, by0 + by);
            data[ui] = moved(data[ui], su);
            data[vi] = moved(data[vi], sv);
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

/// Any frame to UYVY (packed U, Y0, V, Y1: two pixels per four bytes), written
/// into `out`, which must hold `width * height * 2` bytes. Returns `false` and
/// writes nothing if the frame's width is odd (UYVY stores colour per pixel pair)
/// or `out` has the wrong size.
///
/// UYVY is what the macOS fake camera takes (T-070). This is a whole-frame
/// conversion, the one place the corrected frame leaves the camera's format.
pub fn frame_to_uyvy(frame: &Frame, out: &mut [u8]) -> bool {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    if w % 2 != 0 || out.len() != w * h * 2 {
        return false;
    }
    let data = frame.data();
    let cw = w / 2;
    match frame.format() {
        // Same samples as UYVY, in another order: Y0 U Y1 V -> U Y0 V Y1.
        PixelFormat::Yuyv => {
            // `as_chunks` cuts a slice into fixed-size arrays (here 4 bytes each)
            let (groups_out, groups_in) = (out.as_chunks_mut::<4>().0, data.as_chunks::<4>().0);
            for (o, i) in groups_out.iter_mut().zip(groups_in) {
                *o = [i[1], i[0], i[3], i[2]];
            }
        }
        // Half-height colour: each colour row serves the two picture rows it covers.
        PixelFormat::Nv12 | PixelFormat::I420 => {
            let nv12 = frame.format() == PixelFormat::Nv12;
            let colour = &data[w * h..];
            let v_plane = cw * (h / 2); // where I420's V plane starts within `colour`
            for y in 0..h {
                let luma = &data[y * w..][..w];
                let row = &mut out[y * w * 2..][..w * 2];
                let c = (y / 2) * cw;
                for cx in 0..cw {
                    let (u, v) = if nv12 {
                        (colour[(c + cx) * 2], colour[(c + cx) * 2 + 1])
                    } else {
                        (colour[c + cx], colour[v_plane + c + cx])
                    };
                    row[cx * 4..][..4].copy_from_slice(&[u, luma[cx * 2], v, luma[cx * 2 + 1]]);
                }
            }
        }
        // Brightness per pixel; colour is the average of each pixel pair.
        PixelFormat::Rgb24 | PixelFormat::Bgr24 => {
            let (r, b) = if frame.format() == PixelFormat::Rgb24 {
                (0, 2)
            } else {
                (2, 0)
            };
            // two pixels in (6 bytes), two pixels out (4 bytes)
            let (groups_out, groups_in) = (out.as_chunks_mut::<4>().0, data.as_chunks::<6>().0);
            for (o, i) in groups_out.iter_mut().zip(groups_in) {
                let [y0, u0, v0] = rgb_to_yuv(i[r], i[1], i[b]);
                let [y1, u1, v1] = rgb_to_yuv(i[3 + r], i[4], i[3 + b]);
                // the average of the pair, halves rounded up
                *o = [
                    (u0 as u16 + u1 as u16).div_ceil(2) as u8,
                    y0,
                    (v0 as u16 + v1 as u16).div_ceil(2) as u8,
                    y1,
                ];
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UYVY back to YUYV, to compare with conversions that already have tests.
    fn uyvy_as_yuyv(uyvy: &[u8]) -> Vec<u8> {
        let groups = uyvy.as_chunks::<4>().0;
        groups
            .iter()
            .flat_map(|c| [c[1], c[0], c[3], c[2]])
            .collect()
    }

    #[test]
    fn every_format_gives_the_same_uyvy_picture() {
        let (w, h) = (32u32, 16u32);
        let rgb = smooth_rgb(w as usize, h as usize);
        let i420 = rgb_to_i420(&rgb, w, h);
        let yuyv = i420_to_yuyv(&i420, w, h);
        let uyvy_of = |format, data: Vec<u8>| {
            let f = Frame::new(w, h, format, data, 0).unwrap();
            let mut out = vec![0u8; (w * h * 2) as usize];
            assert!(frame_to_uyvy(&f, &mut out));
            out
        };
        // The three camera formats hold the same samples: exactly equal.
        let from_yuyv = uyvy_of(PixelFormat::Yuyv, yuyv.clone());
        assert_eq!(uyvy_as_yuyv(&from_yuyv), yuyv);
        assert_eq!(uyvy_of(PixelFormat::I420, i420.clone()), from_yuyv);
        assert_eq!(
            uyvy_of(PixelFormat::Nv12, i420_to_nv12(&i420, w, h)),
            from_yuyv
        );
        // From RGB the brightness is exact; the colour is averaged over two
        // pixels here and over four in the others, so it may differ a little
        // on this smooth picture.
        let from_rgb = uyvy_of(PixelFormat::Rgb24, rgb.clone());
        let pixels = rgb.as_chunks::<3>().0;
        let bgr: Vec<u8> = pixels.iter().flat_map(|p| [p[2], p[1], p[0]]).collect();
        assert_eq!(uyvy_of(PixelFormat::Bgr24, bgr), from_rgb);
        for (i, (a, b)) in from_rgb.iter().zip(&from_yuyv).enumerate() {
            let limit = if i % 2 == 1 { 0 } else { 3 }; // odd bytes are brightness
            assert!(
                (*a as i32 - *b as i32).abs() <= limit,
                "byte {i}: {a} vs {b}"
            );
        }
    }

    #[test]
    fn uyvy_refuses_an_odd_width_or_a_wrong_sized_buffer() {
        let f = Frame::new(5, 4, PixelFormat::Rgb24, vec![0; 60], 0).unwrap();
        assert!(!frame_to_uyvy(&f, &mut [0u8; 40]));
        let f = Frame::new(4, 4, PixelFormat::Rgb24, vec![0; 48], 0).unwrap();
        assert!(!frame_to_uyvy(&f, &mut [0u8; 31]));
        assert!(frame_to_uyvy(&f, &mut [0u8; 32]));
    }

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

    /// The same picture in every layout the pipeline can write into.
    fn all_layouts(w: u32, h: u32) -> Vec<Frame> {
        let rgb = smooth_rgb(w as usize, h as usize);
        let bgr: Vec<u8> = rgb
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[2], p[1], p[0]])
            .collect();
        let i420 = rgb_to_i420(&rgb, w, h);
        vec![
            Frame::new(w, h, PixelFormat::I420, i420.clone(), 0).unwrap(),
            Frame::new(w, h, PixelFormat::Nv12, i420_to_nv12(&i420, w, h), 0).unwrap(),
            Frame::new(w, h, PixelFormat::Yuyv, i420_to_yuyv(&i420, w, h), 0).unwrap(),
            Frame::new(w, h, PixelFormat::Rgb24, rgb, 0).unwrap(),
            Frame::new(w, h, PixelFormat::Bgr24, bgr, 0).unwrap(),
        ]
    }

    #[test]
    fn blending_with_an_empty_mask_changes_no_byte() {
        let r = Rect {
            x: 7,
            y: 5,
            w: 21,
            h: 13,
        };
        for mut f in all_layouts(64, 48) {
            let before = f.data().to_vec();
            f.blend_rgb(r, &vec![255; 21 * 13 * 3], &vec![0; 21 * 13])
                .unwrap();
            assert_eq!(f.data(), &before[..], "{:?}", f.format());
        }
    }

    #[test]
    fn blending_paints_inside_the_mask_and_nothing_outside() {
        // odd offsets and sizes again, so colour blocks are cut by the rectangle's edge
        let r = Rect {
            x: 7,
            y: 5,
            w: 21,
            h: 13,
        };
        let colour = [200u8, 90, 40];
        let patch: Vec<u8> = colour.iter().copied().cycle().take(21 * 13 * 3).collect();
        for mut f in all_layouts(64, 48) {
            let before = f.to_rgb();
            f.blend_rgb(r, &patch, &vec![255; 21 * 13]).unwrap();
            let after = f.to_rgb();
            for y in 0..48usize {
                for x in 0..64usize {
                    let i = (y * 64 + x) * 3;
                    let inside = (7..28).contains(&x) && (5..18).contains(&y);
                    // well inside: every pixel of every colour block was painted
                    let deep = (8..27).contains(&x) && (6..17).contains(&y);
                    // a colour block reaches at most one pixel past the rectangle
                    let far = !(6..29).contains(&x) || !(4..19).contains(&y);
                    if deep {
                        assert!(
                            max_diff(&after[i..i + 3], &colour) <= 4,
                            "{:?} at {x},{y}: {:?}",
                            f.format(),
                            &after[i..i + 3]
                        );
                    }
                    if far {
                        assert_eq!(&after[i..i + 3], &before[i..i + 3], "{:?}", f.format());
                    }
                    if !inside && f.format().needs_even_size() {
                        // brightness is per pixel, so outside the rectangle it must not move at all
                        let luma = |rgb: &[u8]| rgb_to_yuv(rgb[i], rgb[i + 1], rgb[i + 2])[0];
                        assert!(luma(&after).abs_diff(luma(&before)) <= 3);
                    }
                }
            }
        }
    }

    #[test]
    fn half_alpha_lands_half_way() {
        let mut f = Frame::new(2, 1, PixelFormat::Rgb24, vec![0, 100, 200, 9, 9, 9], 0).unwrap();
        let r = Rect {
            x: 0,
            y: 0,
            w: 2,
            h: 1,
        };
        f.blend_rgb(r, &[100, 100, 0, 50, 50, 50], &[128, 0])
            .unwrap();
        assert_eq!(f.data(), &[50, 100, 100, 9, 9, 9]);
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
