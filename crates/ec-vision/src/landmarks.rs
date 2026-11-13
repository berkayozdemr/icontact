//! The face mesh model: 478 points on a face that is already framed.

use ec_core::Frame;
use ort::session::Session;
use ort::value::Tensor;

use crate::geometry::{cut_square, Roi};
use crate::{runtime, VisionError};

const MODEL: &[u8] = include_bytes!("../models/face_landmarks_detector.onnx");
/// The model looks at a 256 x 256 picture.
const INPUT: usize = 256;
pub const POINT_COUNT: usize = 478;

pub(crate) struct Mesh {
    /// x, y in frame pixels; z in the same unit, smaller is nearer the camera.
    pub points: Vec<[f32; 3]>,
    /// 0 to 1: how sure the model is that the square holds a face.
    pub score: f32,
}

pub(crate) struct LandmarkModel {
    session: Session,
}

impl LandmarkModel {
    pub fn new(threads: usize) -> Result<LandmarkModel, VisionError> {
        Ok(LandmarkModel {
            session: runtime::load(MODEL, threads)?,
        })
    }

    pub fn run(&mut self, frame: &Frame, roi: &Roi) -> Result<Mesh, VisionError> {
        // This model wants values from 0 to 1.
        let input = cut_square(frame, roi, INPUT, 1.0 / 255.0, 0.0);
        let tensor = Tensor::from_array(([1usize, INPUT, INPUT, 3], input))?;
        let outputs = self.session.run(ort::inputs!["input_12" => tensor])?;
        let (_, raw) = outputs["Identity"].try_extract_tensor::<f32>()?;
        let (_, flag) = outputs["Identity_1"].try_extract_tensor::<f32>()?;
        if raw.len() != POINT_COUNT * 3 || flag.len() != 1 {
            return Err(VisionError::BadModelOutput("face mesh sizes"));
        }
        // Points come back in the square's own 0..256 pixels.
        let unit = roi.side / INPUT as f32;
        let points = raw
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| {
                let (x, y) = roi.to_frame(p[0] / INPUT as f32, p[1] / INPUT as f32);
                [x, y, p[2] * unit]
            })
            .collect();
        Ok(Mesh {
            points,
            score: 1.0 / (1.0 + (-flag[0]).exp()),
        })
    }
}
