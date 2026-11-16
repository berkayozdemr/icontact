//! The warp model: given an eye patch and a turn, says how to move its pixels.
//!
//! The model is ours, and
//! built into the executable like the two face models.

use ort::session::Session;
use ort::value::Tensor;

use crate::eye::{PATCH_H, PATCH_W};
use crate::{runtime, VisionError};

const MODEL: &[u8] = include_bytes!("../models/warp.onnx");
const PLANE: usize = PATCH_W * PATCH_H;
/// The model handles turns up to about this many degrees.
pub const MAX_TURN: f32 = 30.0;

/// What the model answers for one eye, as 64 rows x 96 columns per plane.
#[derive(Clone, Debug)]
pub struct Maps {
    /// Two planes. For each pixel: how far away to fetch its colour from, across
    /// (first plane) and down (second), where 2.0 is the whole width or height of the patch.
    pub flow: Vec<f32>,
    /// One plane, added to the fetched colour (1.0 is full white). Lets the model
    /// paint eye-white where the iris moved away.
    pub bright: Vec<f32>,
}

impl Maps {
    /// The three numbers (fetch across, fetch down, brightness) at a position in
    /// the patch, blended between the four nearest map pixels.
    #[inline]
    pub fn at(&self, u: f32, v: f32) -> [f32; 3] {
        // map pixel k has its middle at (k + 0.5) / size
        let fx = (u * PATCH_W as f32 - 0.5).clamp(0.0, (PATCH_W - 1) as f32);
        let fy = (v * PATCH_H as f32 - 0.5).clamp(0.0, (PATCH_H - 1) as f32);
        let (x0, y0) = (fx as usize, fy as usize);
        let (x1, y1) = ((x0 + 1).min(PATCH_W - 1), (y0 + 1).min(PATCH_H - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let blend = |plane: &[f32]| {
            let top = plane[y0 * PATCH_W + x0] * (1.0 - tx) + plane[y0 * PATCH_W + x1] * tx;
            let bottom = plane[y1 * PATCH_W + x0] * (1.0 - tx) + plane[y1 * PATCH_W + x1] * tx;
            top + (bottom - top) * ty
        };
        [
            blend(&self.flow[..PLANE]),
            blend(&self.flow[PLANE..]),
            blend(&self.bright),
        ]
    }
}

impl Maps {
    /// How different two answers are: the average change of the fetch distance,
    /// in the maps' own unit (2.0 is the whole patch).
    pub fn distance(&self, other: &Maps) -> f32 {
        let sum: f32 = self
            .flow
            .iter()
            .zip(&other.flow)
            .map(|(a, b)| (a - b).abs())
            .sum();
        sum / self.flow.len() as f32
    }

    /// Move this answer towards `newer`: `share` 0 keeps this one, 1 takes the newer.
    pub fn blend_towards(&mut self, newer: &Maps, share: f32) {
        for (old, new) in self.flow.iter_mut().zip(&newer.flow) {
            *old += (new - *old) * share;
        }
        for (old, new) in self.bright.iter_mut().zip(&newer.bright) {
            *old += (new - *old) * share;
        }
    }
}

pub(crate) struct WarpModel {
    session: Session,
}

impl WarpModel {
    pub fn new(threads: usize) -> Result<WarpModel, VisionError> {
        Ok(WarpModel {
            session: runtime::load(MODEL, threads)?,
        })
    }

    /// `input` is the patch from [`crate::eye::model_input`]; `turn` is in degrees:
    /// up, then towards the right of the (possibly mirrored) patch.
    pub fn run(&mut self, input: Vec<f32>, turn: [f32; 2]) -> Result<Maps, VisionError> {
        if input.len() != 3 * PLANE {
            return Err(VisionError::BadModelOutput("warp model input size"));
        }
        let img = Tensor::from_array(([1usize, 3, PATCH_H, PATCH_W], input))?;
        let turn = Tensor::from_array(([1usize, 2], turn.to_vec()))?;
        let outputs = self
            .session
            .run(ort::inputs!["img" => img, "turn" => turn])?;
        let (_, flow) = outputs["flow"].try_extract_tensor::<f32>()?;
        let (_, bright) = outputs["bright"].try_extract_tensor::<f32>()?;
        if flow.len() != 2 * PLANE || bright.len() != PLANE {
            return Err(VisionError::BadModelOutput("warp model sizes"));
        }
        Ok(Maps {
            flow: flow.to_vec(),
            bright: bright.to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_turn_asks_for_no_change_and_a_real_turn_asks_for_some() {
        let mut m = WarpModel::new(1).unwrap();
        // not an eye, but the model must still answer in the right shape
        let input: Vec<f32> = (0..3 * PLANE).map(|i| (i % 97) as f32 / 97.0).collect();
        let still = m.run(input.clone(), [0.0, 0.0]).unwrap();
        assert!(still.flow.iter().chain(&still.bright).all(|v| *v == 0.0));
        let moved = m.run(input, [10.0, -6.0]).unwrap();
        assert!(moved.flow.iter().any(|v| v.abs() > 1e-4));
        assert!(moved.flow.iter().all(|v| v.abs() <= 0.36));
    }

    #[test]
    fn answers_can_be_compared_and_blended() {
        let flat = |v: f32| Maps {
            flow: vec![v; 2 * PLANE],
            bright: vec![v; PLANE],
        };
        let mut a = flat(0.0);
        assert_eq!(a.distance(&flat(0.25)), 0.25);
        a.blend_towards(&flat(1.0), 0.25);
        assert_eq!((a.flow[7], a.bright[7]), (0.25, 0.25));
        a.blend_towards(&flat(1.0), 1.0);
        assert_eq!(a.flow[7], 1.0);
    }

    #[test]
    fn maps_are_read_at_pixel_middles() {
        let mut flow = vec![0.0f32; 2 * PLANE];
        flow[0] = 1.0; // top-left pixel, "across" plane
        flow[1] = 3.0;
        let maps = Maps {
            flow,
            bright: vec![0.5; PLANE],
        };
        let mid = |k: f32| (k + 0.5) / PATCH_W as f32;
        assert_eq!(maps.at(mid(0.0), 0.5 / PATCH_H as f32), [1.0, 0.0, 0.5]);
        assert_eq!(maps.at(mid(0.5), 0.5 / PATCH_H as f32)[0], 2.0);
        // outside the patch: the nearest edge value
        assert_eq!(maps.at(-1.0, -1.0)[0], 1.0);
    }
}
