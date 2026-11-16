//! Turning the eye in the frame: applies the warp model's answer at the camera's
//! own sharpness and writes the result back.
//!
//! The model answers on a small 96 x 64 patch, but nothing is ever pasted back
//! from that patch. Instead, each camera pixel near the eye asks the model's map
//! "where should my colour come from?" and fetches it from the camera picture
//! itself. Two things follow:
//! - the eye keeps the camera's full detail, however big it is in the frame;
//! - where the map says "from right here", the pixel is not touched at all.
//!
//! The soft edge (T-054) is done on the *distance fetched*, not by mixing two
//! pictures: towards the edge of the box the fetch distance shrinks to nothing,
//! so the moved middle joins the untouched surroundings without a seam and
//! without a ghost of the old iris showing through.

use ec_core::{Frame, FrameError};

use crate::eye::{EyeBox, EyeCrop};
use crate::highlight::{Highlight, FOLLOW};
use crate::warp::Maps;

/// Inside this share of the way from the box's middle to its edge the model's
/// answer is used in full; from there it fades, reaching nothing at the edge.
/// The eye corners sit at 0.55 of the way across, so the eye itself is always in full.
const MASK_FULL: f32 = 0.7;
/// A pixel that would move less than this (in frame pixels) and brighten less
/// than half a step is left exactly as the camera sent it.
const MIN_SHIFT: f32 = 0.01;

/// How much of the model's answer to use at a position in the box: 1 in the
/// middle, fading smoothly to 0 at the box's edge.
pub fn mask(u: f32, v: f32) -> f32 {
    let fade = |t: f32| {
        // 0 at the middle of the box, 1 at its edge
        let d = (t - 0.5).abs() * 2.0;
        let x = ((1.0 - d) / (1.0 - MASK_FULL)).clamp(0.0, 1.0);
        x * x * (3.0 - 2.0 * x) // "smoothstep": no kink at either end
    };
    fade(u) * fade(v)
}

/// Apply `maps` to the eye under `eye_box` and write the result into `frame`.
/// `crop` is the untouched picture the colours are fetched from. With `mirrored`
/// the maps were made from the mirror image of the patch (see [`crate::eye::Side`]).
///
/// With a `highlight` (see [`crate::highlight`]), `crop` is the eye with its
/// reflection already taken out; the reflection is laid back on top here, whole,
/// moved by part of the iris's move.
///
/// Returns how many pixels were changed.
pub fn apply(
    frame: &mut Frame,
    crop: &EyeCrop,
    eye_box: &EyeBox,
    maps: &Maps,
    mirrored: bool,
    highlight: Option<&Highlight>,
) -> Result<usize, FrameError> {
    let rect = crop.rect;
    let (w, h) = (rect.w as usize, rect.h as usize);
    let mut rgb = vec![0u8; w * h * 3];
    let mut alpha = vec![0u8; w * h];
    let mut changed = 0;
    // What the maps say at a frame position: how far away to fetch the colour
    // from (in frame pixels, across and down the picture) and how much to brighten.
    // `None` outside the box.
    let fetch_at = |x: f32, y: f32| -> Option<(f32, f32, f32)> {
        let (u, v) = eye_box.to_patch(x, y);
        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return None;
        }
        let m = mask(u, v);
        if m <= 0.0 {
            return None;
        }
        // For a mirrored eye, read the map at the mirrored spot, and a fetch
        // "from the right" in the mirror image is "from the left" here.
        let [mut fetch_across, fetch_down, bright] = maps.at(if mirrored { 1.0 - u } else { u }, v);
        if mirrored {
            fetch_across = -fetch_across;
        }
        // The map counts 2.0 as the whole patch; turn that into frame pixels.
        let (a, d) = (m * fetch_across / 2.0, m * fetch_down / 2.0);
        Some((
            a * eye_box.across[0] + d * eye_box.down[0],
            a * eye_box.across[1] + d * eye_box.down[1],
            m * bright * 255.0,
        ))
    };
    // The reflection moves as one piece: by FOLLOW of the move the iris makes
    // where the reflection sits. (Fetching "from over there" moves the picture
    // the other way, so the layer is fetched from the same side as the iris is.)
    let light_shift = highlight.and_then(|h| {
        fetch_at(h.centre[0], h.centre[1]).map(|(dx, dy, _)| (h, dx * FOLLOW, dy * FOLLOW))
    });
    for row in 0..h {
        for col in 0..w {
            // the middle of this frame pixel
            let x = rect.x as f32 + col as f32 + 0.5;
            let y = rect.y as f32 + row as f32 + 0.5;
            let Some((dx, dy, lift)) = fetch_at(x, y) else {
                continue;
            };
            // the reflection that belongs here now, and the one that was here
            let (light, light_before) = match light_shift {
                Some((h, sx, sy)) => (h.sample(x + sx, y + sy), h.sample(x, y)),
                None => ([0.0; 3], [0.0; 3]),
            };
            let light_moved = (0..3).any(|c| (light[c] - light_before[c]).abs() >= 0.5);
            if dx * dx + dy * dy < MIN_SHIFT * MIN_SHIFT && lift.abs() < 0.5 && !light_moved {
                continue;
            }
            let colour = crop.sample(x + dx, y + dy);
            let i = row * w + col;
            for c in 0..3 {
                rgb[i * 3 + c] = (colour[c] + lift + light[c]).round().clamp(0.0, 255.0) as u8;
            }
            alpha[i] = 255;
            changed += 1;
        }
    }
    if changed > 0 {
        frame.blend_rgb(rect, &rgb, &alpha)?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eye::{PATCH_H, PATCH_W};
    use ec_core::convert::{i420_to_nv12, i420_to_yuyv, rgb_to_i420};
    use ec_core::PixelFormat;

    const PLANE: usize = PATCH_W * PATCH_H;

    /// A picture with detail everywhere, in every layout a camera may send.
    fn frames() -> Vec<Frame> {
        let (w, h) = (320u32, 200u32);
        let mut rgb = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let stripe = if (x / 3 + y / 5) % 2 == 0 { 40 } else { 0 };
                rgb.extend_from_slice(&[
                    (60 + x / 2 + stripe) as u8,
                    (50 + y / 2 + stripe) as u8,
                    (90 + (x + y) / 4) as u8,
                ]);
            }
        }
        let i420 = rgb_to_i420(&rgb, w, h);
        vec![
            Frame::new(w, h, PixelFormat::Nv12, i420_to_nv12(&i420, w, h), 0).unwrap(),
            Frame::new(w, h, PixelFormat::Yuyv, i420_to_yuyv(&i420, w, h), 0).unwrap(),
            Frame::new(w, h, PixelFormat::I420, i420, 0).unwrap(),
            Frame::new(w, h, PixelFormat::Rgb24, rgb, 0).unwrap(),
        ]
    }

    fn tilted_box() -> EyeBox {
        EyeBox::around([120.0, 90.0], [190.0, 99.0]).unwrap()
    }

    fn constant_maps(across: f32, down: f32, bright: f32) -> Maps {
        let mut flow = vec![across; 2 * PLANE];
        flow[PLANE..].fill(down);
        Maps {
            flow,
            bright: vec![bright; PLANE],
        }
    }

    #[test]
    fn the_mask_is_full_in_the_middle_and_gone_at_the_edge() {
        assert_eq!(mask(0.5, 0.5), 1.0);
        assert_eq!(mask(0.225, 0.5), 1.0); // an eye corner
        assert_eq!(mask(0.0, 0.5), 0.0);
        assert_eq!(mask(0.5, 1.0), 0.0);
        let part = mask(0.07, 0.5);
        assert!(part > 0.0 && part < 1.0);
    }

    /// T-050: a turn of zero must not change one byte, in any camera format.
    #[test]
    fn a_zero_answer_changes_no_byte_in_any_format() {
        for mut f in frames() {
            let before = f.data().to_vec();
            let b = tilted_box();
            let crop = EyeCrop::cut(&f, &b).unwrap();
            let n = apply(
                &mut f,
                &crop,
                &b,
                &constant_maps(0.0, 0.0, 0.0),
                false,
                None,
            )
            .unwrap();
            assert_eq!(n, 0);
            assert_eq!(f.data(), &before[..], "{:?}", f.format());
        }
    }

    /// T-050 and T-054: a real shift changes the eye, and nothing outside the box.
    #[test]
    fn a_shift_moves_the_middle_and_leaves_everything_outside_the_box_alone() {
        for mut f in frames() {
            let before = f.to_rgb();
            let b = tilted_box();
            let crop = EyeCrop::cut(&f, &b).unwrap();
            let n = apply(
                &mut f,
                &crop,
                &b,
                &constant_maps(0.1, 0.05, 0.0),
                false,
                None,
            )
            .unwrap();
            assert!(n > 1000, "{n}");
            let after = f.to_rgb();
            let mut changed_inside = 0;
            for y in 0..f.height() as usize {
                for x in 0..f.width() as usize {
                    let i = (y * f.width() as usize + x) * 3;
                    let same = after[i..i + 3] == before[i..i + 3];
                    let (u, v) = b.to_patch(x as f32 + 0.5, y as f32 + 0.5);
                    // colour is stored per 2x2 block, so allow a block's reach past the box
                    let margin_u = 2.5 / b.width();
                    let margin_v = margin_u * 1.5;
                    let outside =
                        u < -margin_u || u > 1.0 + margin_u || v < -margin_v || v > 1.0 + margin_v;
                    if outside {
                        assert!(same, "{:?} changed at {x},{y}", f.format());
                    } else if !same {
                        changed_inside += 1;
                    }
                }
            }
            assert!(changed_inside > 1000);
        }
    }

    #[test]
    fn a_shift_fetches_from_where_the_map_says() {
        // An upright box 96 x 64 frame pixels, on a picture whose red is its column.
        let (w, h) = (200u32, 100u32);
        let mut d = Vec::new();
        for _y in 0..h {
            for x in 0..w {
                d.extend_from_slice(&[x as u8, 50, 7]);
            }
        }
        let mut f = Frame::new(w, h, PixelFormat::Rgb24, d, 0).unwrap();
        let b = EyeBox::around([73.6, 50.0], [126.4, 50.0]).unwrap();
        let crop = EyeCrop::cut(&f, &b).unwrap();
        // 0.25 of the half-width = 12 frame pixels to the right
        apply(
            &mut f,
            &crop,
            &b,
            &constant_maps(0.25, 0.0, 0.0),
            false,
            None,
        )
        .unwrap();
        let red_at = |f: &Frame, x: usize, y: usize| f.data()[(y * 200 + x) * 3];
        assert_eq!(red_at(&f, 100, 50), 112);
        // mirrored: the same map means "from the left"
        let mut f2 = Frame::new(w, h, PixelFormat::Rgb24, crop_source(w, h), 0).unwrap();
        apply(
            &mut f2,
            &crop,
            &b,
            &constant_maps(0.25, 0.0, 0.0),
            true,
            None,
        )
        .unwrap();
        assert_eq!(red_at(&f2, 100, 50), 88);
        // brightness is added on top
        let mut f3 = Frame::new(w, h, PixelFormat::Rgb24, crop_source(w, h), 0).unwrap();
        apply(
            &mut f3,
            &crop,
            &b,
            &constant_maps(0.0, 0.0, 0.1),
            false,
            None,
        )
        .unwrap();
        assert_eq!(red_at(&f3, 100, 50), 100 + 26);
    }

    fn crop_source(w: u32, h: u32) -> Vec<u8> {
        let mut d = Vec::new();
        for _y in 0..h {
            for x in 0..w {
                d.extend_from_slice(&[x as u8, 50, 7]);
            }
        }
        d
    }
}
