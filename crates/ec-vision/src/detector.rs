//! Finds a face anywhere in the frame (MediaPipe "BlazeFace short range").
//!
//! Slow next to tracking and only roughly placed, so it runs only when no face
//! is being followed. Its one job is to give the landmark model a starting square.

use ec_core::Frame;
use ort::session::Session;
use ort::value::Tensor;

use crate::geometry::{cut_square, Roi};
use crate::{runtime, VisionError};

const MODEL: &[u8] = include_bytes!("../models/face_detector.onnx");
/// The model looks at a 128 x 128 picture.
const INPUT: usize = 128;
/// It makes 896 guesses, each tied to a fixed spot ("anchor") in that picture.
const ANCHORS: usize = 896;
/// Per guess: box centre x, y, width, height, then six face points (x, y).
const VALUES: usize = 16;
const MIN_SCORE: f32 = 0.5;
/// Guesses that overlap the best one by more than this are the same face.
const SAME_FACE_OVERLAP: f32 = 0.3;

/// A face the detector found, in frame pixels.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Detection {
    pub score: f32,
    /// Box as centre x, centre y, width, height.
    pub bbox: [f32; 4],
    /// Picture-left eye, then picture-right eye.
    pub eyes: [[f32; 2]; 2],
}

impl Detection {
    /// The square to hand the landmark model.
    pub fn roi(&self) -> Roi {
        let [cx, cy, w, h] = self.bbox;
        let [a, b] = self.eyes;
        Roi {
            cx,
            cy,
            side: w.max(h) * 1.5,
            rotation: (b[1] - a[1]).atan2(b[0] - a[0]),
        }
    }
}

pub(crate) struct Detector {
    session: Session,
    anchors: Vec<[f32; 2]>,
}

impl Detector {
    pub fn new(threads: usize) -> Result<Detector, VisionError> {
        Ok(Detector {
            session: runtime::load(MODEL, threads)?,
            anchors: anchors(),
        })
    }

    /// The most certain face in the frame, if any.
    pub fn detect(&mut self, frame: &Frame) -> Result<Option<Detection>, VisionError> {
        // The whole frame, shrunk into the square with black bars to keep its shape.
        let (fw, fh) = (frame.width() as f32, frame.height() as f32);
        let view = Roi {
            cx: fw / 2.0,
            cy: fh / 2.0,
            side: fw.max(fh),
            rotation: 0.0,
        };
        // This model wants values from -1 to 1.
        let input = cut_square(frame, &view, INPUT, 1.0 / 127.5, -1.0);
        let tensor = Tensor::from_array(([1usize, INPUT, INPUT, 3], input))?;
        let outputs = self.session.run(ort::inputs!["input" => tensor])?;
        let (_, boxes) = outputs["regressors"].try_extract_tensor::<f32>()?;
        let (_, scores) = outputs["classificators"].try_extract_tensor::<f32>()?;
        if boxes.len() != ANCHORS * VALUES || scores.len() != ANCHORS {
            return Err(VisionError::BadModelOutput("face detector sizes"));
        }
        Ok(best_face(boxes, scores, &self.anchors).map(|d| {
            // from the 0..1 square back to frame pixels
            let to_px = |p: [f32; 2]| {
                let (x, y) = view.to_frame(p[0], p[1]);
                [x, y]
            };
            let [cx, cy] = to_px([d.bbox[0], d.bbox[1]]);
            Detection {
                score: d.score,
                bbox: [cx, cy, d.bbox[2] * view.side, d.bbox[3] * view.side],
                eyes: [to_px(d.eyes[0]), to_px(d.eyes[1])],
            }
        }))
    }
}

/// The fixed spots the model's guesses are measured from, as centres in 0..1.
/// A 16 x 16 grid with 2 guesses per cell, then an 8 x 8 grid with 6 per cell.
fn anchors() -> Vec<[f32; 2]> {
    let mut out = Vec::with_capacity(ANCHORS);
    for (grid, per_cell) in [(16usize, 2usize), (8, 6)] {
        for y in 0..grid {
            for x in 0..grid {
                let centre = [
                    (x as f32 + 0.5) / grid as f32,
                    (y as f32 + 0.5) / grid as f32,
                ];
                out.extend(std::iter::repeat_n(centre, per_cell));
            }
        }
    }
    out
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x.clamp(-100.0, 100.0)).exp())
}

/// Overlap of two centre/size boxes: shared area over combined area.
fn overlap(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let w = (a[0] + a[2] / 2.0).min(b[0] + b[2] / 2.0) - (a[0] - a[2] / 2.0).max(b[0] - b[2] / 2.0);
    let h = (a[1] + a[3] / 2.0).min(b[1] + b[3] / 2.0) - (a[1] - a[3] / 2.0).max(b[1] - b[3] / 2.0);
    if w <= 0.0 || h <= 0.0 {
        return 0.0;
    }
    let shared = w * h;
    shared / (a[2] * a[3] + b[2] * b[3] - shared)
}

/// Turn the raw numbers into one face, in 0..1 of the model's picture.
/// The best guess wins; guesses that overlap it are averaged in by their score,
/// which steadies the box.
fn best_face(boxes: &[f32], scores: &[f32], anchors: &[[f32; 2]]) -> Option<Detection> {
    let s = INPUT as f32;
    let decode = |i: usize| -> Detection {
        let v = &boxes[i * VALUES..(i + 1) * VALUES];
        let [ax, ay] = anchors[i];
        Detection {
            score: sigmoid(scores[i]),
            bbox: [v[0] / s + ax, v[1] / s + ay, v[2] / s, v[3] / s],
            eyes: [
                [v[4] / s + ax, v[5] / s + ay],
                [v[6] / s + ax, v[7] / s + ay],
            ],
        }
    };
    let found: Vec<Detection> = (0..anchors.len())
        .filter(|&i| sigmoid(scores[i]) >= MIN_SCORE)
        .map(decode)
        .collect();
    let best = *found.iter().max_by(|a, b| a.score.total_cmp(&b.score))?;

    let mut total = 0.0;
    let mut sum = [0.0f32; 8];
    for d in found
        .iter()
        .filter(|d| overlap(&d.bbox, &best.bbox) > SAME_FACE_OVERLAP)
    {
        let values = [
            d.bbox[0],
            d.bbox[1],
            d.bbox[2],
            d.bbox[3],
            d.eyes[0][0],
            d.eyes[0][1],
            d.eyes[1][0],
            d.eyes[1][1],
        ];
        for (acc, v) in sum.iter_mut().zip(values) {
            *acc += v * d.score;
        }
        total += d.score;
    }
    let m = sum.map(|v| v / total);
    Some(Detection {
        score: best.score,
        bbox: [m[0], m[1], m[2], m[3]],
        eyes: [[m[4], m[5]], [m[6], m[7]]],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_count_and_first_cells() {
        let a = anchors();
        assert_eq!(a.len(), ANCHORS);
        assert_eq!(a[0], [1.0 / 32.0, 1.0 / 32.0]);
        assert_eq!(a[2], [3.0 / 32.0, 1.0 / 32.0]);
        assert_eq!(a[512], [1.0 / 16.0, 1.0 / 16.0]);
    }

    #[test]
    fn overlap_of_boxes() {
        let a = [0.5, 0.5, 0.2, 0.2];
        assert!((overlap(&a, &a) - 1.0).abs() < 1e-6);
        assert_eq!(overlap(&a, &[0.9, 0.9, 0.1, 0.1]), 0.0);
        // half-shifted: shared 0.02 of 0.06 combined
        assert!((overlap(&a, &[0.6, 0.5, 0.2, 0.2]) - 1.0 / 3.0).abs() < 1e-5);
    }

    #[test]
    fn nothing_scores_high_means_no_face() {
        let a = anchors();
        let boxes = vec![0.0; ANCHORS * VALUES];
        let scores = vec![-10.0; ANCHORS];
        assert!(best_face(&boxes, &scores, &a).is_none());
    }

    #[test]
    fn overlapping_guesses_merge_and_far_ones_are_ignored() {
        let a = anchors();
        let mut boxes = vec![0.0; ANCHORS * VALUES];
        let mut scores = vec![-10.0; ANCHORS];
        // two guesses from the same cell: same centre, widths 32 and 48 px of 128
        for (i, w) in [(0usize, 32.0f32), (1, 48.0)] {
            boxes[i * VALUES + 2] = w;
            boxes[i * VALUES + 3] = w;
            scores[i] = 5.0;
        }
        // a weaker face far away
        boxes[895 * VALUES + 2] = 16.0;
        boxes[895 * VALUES + 3] = 16.0;
        scores[895] = 1.0;
        let d = best_face(&boxes, &scores, &a).unwrap();
        assert!((d.bbox[0] - 1.0 / 32.0).abs() < 1e-6);
        assert!((d.bbox[2] - 40.0 / 128.0).abs() < 1e-6, "{:?}", d.bbox);
    }

    #[test]
    fn the_model_loads_and_finds_no_face_in_a_blank_frame() {
        let mut d = Detector::new(1).unwrap();
        let f = Frame::new(
            64,
            48,
            ec_core::PixelFormat::Rgb24,
            vec![90; 64 * 48 * 3],
            0,
        )
        .unwrap();
        assert!(d.detect(&f).unwrap().is_none());
    }
}
