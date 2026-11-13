//! Everything that looks at the picture (spec §3.2, stage 2 so far).
//!
//! [`FaceTracker`] is the way in: give it a frame, get back the 478 face points,
//! the head angle, how open each eye is and a confidence.
//!
//! The two models are MediaPipe's face detector and face mesh (Apache-2.0,
//! see `models/NOTICE.md`), converted to ONNX and built into the executable.

mod detector;
mod geometry;
mod landmarks;
mod measure;
mod runtime;
mod smooth;
mod tracker;

pub use geometry::Roi;
pub use measure::{eye_openness, head_angles, HeadAngles};
pub use smooth::{OneEuro, PointFilter};
pub use tracker::{Face, FaceTracker, StageTimes, POINT_COUNT};

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
}

impl std::fmt::Display for VisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VisionError::Runtime(e) => write!(f, "model runner: {e}"),
            VisionError::BadModelOutput(what) => write!(f, "unexpected model output: {what}"),
        }
    }
}

impl std::error::Error for VisionError {}

impl<R> From<ort::Error<R>> for VisionError {
    fn from(e: ort::Error<R>) -> Self {
        VisionError::Runtime(e.to_string())
    }
}
