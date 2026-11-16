//! Follows one face from frame to frame.
//!
//! Detect once, then track: while a face is being followed, the square for the
//! next frame comes from this frame's points, and only the mesh model runs.
//! The detector runs again only after the face is lost.

use std::time::Instant;

use ec_core::Frame;

use crate::detector::Detector;
use crate::geometry::Roi;
use crate::landmarks::LandmarkModel;
use crate::measure::{eye_openness, head_angles, HeadAngles};
use crate::smooth::PointFilter;
use crate::{points, VisionError};

pub use crate::landmarks::POINT_COUNT;

/// Below this the mesh model is saying "this square is not a face".
const MIN_FACE_SCORE: f32 = 0.5;
/// One Euro settings, in face widths (see [`PointFilter`]). Light on purpose: the raw
/// points already shimmer by only 0.1 to 0.2 px on a 355 px face, and points that trail
/// the picture would put the eye correction in the wrong place. Measured on two
/// recordings, these roughly halve the shimmer for about 1 px of lag at worst
///. T-064 tunes them again with the gate.
const SMOOTH_MIN_CUTOFF: f32 = 3.0;
const SMOOTH_BETA: f32 = 300.0;

/// One tracked face.
#[derive(Clone, Debug)]
pub struct Face {
    /// 478 points: x, y in frame pixels, z in the same unit (smaller is nearer).
    /// Indexes worth knowing are in [`crate::points`].
    pub points: Vec<[f32; 3]>,
    /// 0 to 1: how sure the mesh model is that this is a face.
    pub confidence: f32,
    pub head: HeadAngles,
    /// Lid gap over eye width, picture-left eye first. See [`eye_openness`].
    pub eye_open: [f32; 2],
    /// The square the mesh model looked at for this frame.
    pub roi: Roi,
    /// True on the frame where the detector had to find the face again.
    pub redetected: bool,
}

/// Where the time went for the last frame, in milliseconds.
#[derive(Clone, Copy, Debug, Default)]
pub struct StageTimes {
    /// `None` when the detector did not run.
    pub detect_ms: Option<f64>,
    /// Cutting the square out of the frame and running the mesh model.
    pub mesh_ms: f64,
}

pub struct FaceTracker {
    detector: Detector,
    mesh: LandmarkModel,
    /// Where to look next frame. `None`: no face is being followed.
    roi: Option<Roi>,
    filter: PointFilter,
    /// Smooth the points over time. Off gives each frame's raw answer.
    pub smoothing: bool,
    /// Timings of the most recent [`FaceTracker::track`] call.
    pub last_times: StageTimes,
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

impl FaceTracker {
    /// Loads both models. `threads` is how many CPU threads a model may use.
    pub fn new(threads: usize) -> Result<FaceTracker, VisionError> {
        Ok(FaceTracker {
            detector: Detector::new(threads)?,
            mesh: LandmarkModel::new(threads)?,
            roi: None,
            filter: PointFilter::new(POINT_COUNT, SMOOTH_MIN_CUTOFF, SMOOTH_BETA),
            smoothing: true,
            last_times: StageTimes::default(),
        })
    }

    /// Find the face in `frame`. `time` is the frame's time in seconds on any
    /// steady clock; the smoothing needs to know how far apart frames are.
    pub fn track(&mut self, frame: &Frame, time: f64) -> Result<Option<Face>, VisionError> {
        self.last_times = StageTimes::default();
        // At most two tries: the followed square, then a fresh one from the detector.
        for _ in 0..2 {
            let (roi, redetected) = match self.roi {
                Some(roi) => (roi, false),
                None => {
                    let t = Instant::now();
                    let found = self.detector.detect(frame)?;
                    self.last_times.detect_ms = Some(ms_since(t));
                    match found {
                        Some(d) => (d.roi(), true),
                        None => break,
                    }
                }
            };
            let t = Instant::now();
            let mesh = self.mesh.run(frame, &roi)?;
            self.last_times.mesh_ms += ms_since(t);

            // The next frame's square comes from the raw points: smoothed ones would trail a moving head.
            let flat: Vec<[f32; 2]> = mesh.points.iter().map(|p| [p[0], p[1]]).collect();
            let next = Roi::around(&flat, points::LEFT_EYE_OUTER, points::RIGHT_EYE_OUTER)
                .filter(|r| r.is_usable(frame.width(), frame.height()));
            if let (true, Some(next)) = (mesh.score >= MIN_FACE_SCORE, next) {
                self.roi = Some(next);
                let mut points = mesh.points;
                if self.smoothing {
                    self.filter.apply(&mut points, time, next.side / 1.5);
                }
                return Ok(Some(Face {
                    confidence: mesh.score,
                    head: head_angles(&points).unwrap_or_default(),
                    eye_open: eye_openness(&points).unwrap_or_default(),
                    points,
                    roi,
                    redetected,
                }));
            }
            // Lost it. If that was the followed square, loop once more and ask the detector.
            self.roi = None;
            self.filter.reset();
            if redetected {
                break;
            }
        }
        self.roi = None;
        self.filter.reset();
        Ok(None)
    }

    /// Forget the face being followed, so the next frame starts with the detector.
    pub fn reset(&mut self) {
        self.roi = None;
        self.filter.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::PixelFormat;

    #[test]
    fn a_blank_frame_has_no_face_and_the_detector_ran() {
        let mut t = FaceTracker::new(1).unwrap();
        let f = Frame::new(320, 240, PixelFormat::Rgb24, vec![60; 320 * 240 * 3], 0).unwrap();
        assert!(t.track(&f, 0.0).unwrap().is_none());
        assert!(t.last_times.detect_ms.is_some());
        assert_eq!(t.last_times.mesh_ms, 0.0);
    }

    #[test]
    fn the_tracker_can_move_to_another_thread() {
        fn needs_send<T: Send>() {}
        needs_send::<FaceTracker>();
    }
}
