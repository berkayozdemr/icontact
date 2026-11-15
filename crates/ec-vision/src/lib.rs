//! Everything that looks at the picture (spec §3.2, stages 2 to 5).
//!
//! [`Pipeline`] is the way in: give it a frame, get the frame back with the eyes
//! corrected, plus what was found on the way. Inside it:
//!
//! - [`FaceTracker`] finds the 478 face points, the head angle, how open each
//!   eye is and a confidence (stage 2).
//! - [`EyeCorrector`] estimates the gaze, asks the gate how strongly to act,
//!   runs the warp model and writes the turned eyes into the frame (stages 3 to 5).
//! - [`Ladder`] makes the work cheaper when the machine cannot keep up.
//!
//! The face detector and face mesh are MediaPipe's (Apache-2.0); the warp model
//! is our own, trained in `training/`. All three are ONNX files built into the
//! executable; see `models/NOTICE.md`.

mod corrector;
mod detector;
pub mod eye;
pub mod gate;
pub mod gaze;
mod geometry;
pub mod highlight;
pub mod ladder;
mod landmarks;
mod measure;
mod pipeline;
pub mod redirect;
mod runtime;
mod smooth;
mod tracker;
mod warp;

pub use corrector::{CorrectTimes, Correction, EyeCorrector, EyeDebug, Settings, DEFAULT_STRENGTH};
pub use gaze::{Angles, Calibration};
pub use geometry::Roi;
pub use ladder::{Ladder, Level};
pub use measure::{eye_openness, head_angles, HeadAngles};
pub use pipeline::{Pipeline, Processed, FRAME_LIMIT_MS};
pub use smooth::{OneEuro, PointFilter};
pub use tracker::{Face, FaceTracker, StageTimes, POINT_COUNT};
pub use warp::Maps;

/// Named points of the face mesh. "Left" and "right" are sides of the picture,
/// not of the person: an unmirrored camera shows the person's right eye on the left.
pub mod points {
    pub const LEFT_EYE_OUTER: usize = 33;
    pub const LEFT_EYE_INNER: usize = 133;
    pub const LEFT_EYE_TOP: usize = 159;
    pub const LEFT_EYE_BOTTOM: usize = 145;
    pub const RIGHT_EYE_INNER: usize = 362;
    pub const RIGHT_EYE_OUTER: usize = 263;
    pub const RIGHT_EYE_TOP: usize = 386;
    pub const RIGHT_EYE_BOTTOM: usize = 374;
    /// Iris centre, then four points on its edge.
    pub const LEFT_IRIS: std::ops::Range<usize> = 468..473;
    pub const RIGHT_IRIS: std::ops::Range<usize> = 473..478;
    pub const FOREHEAD: usize = 10;
    pub const CHIN: usize = 152;
    pub const LEFT_CHEEK_EDGE: usize = 234;
    pub const RIGHT_CHEEK_EDGE: usize = 454;
}

#[derive(Debug)]
pub enum VisionError {
    /// ONNX Runtime could not start, load a model or run it.
    Runtime(String),
    /// A model returned something other than what we built against.
    BadModelOutput(&'static str),
    /// Reading from or writing into a frame failed.
    Frame(ec_core::FrameError),
}

impl std::fmt::Display for VisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VisionError::Runtime(e) => write!(f, "model runner: {e}"),
            VisionError::BadModelOutput(what) => write!(f, "unexpected model output: {what}"),
            VisionError::Frame(e) => write!(f, "frame: {e}"),
        }
    }
}

impl std::error::Error for VisionError {}

impl<R> From<ort::Error<R>> for VisionError {
    fn from(e: ort::Error<R>) -> Self {
        VisionError::Runtime(e.to_string())
    }
}

impl From<ec_core::FrameError> for VisionError {
    fn from(e: ec_core::FrameError) -> Self {
        VisionError::Frame(e)
    }
}
