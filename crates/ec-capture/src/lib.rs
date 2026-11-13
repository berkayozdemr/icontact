//! Where frames come from: a camera, a clip file, or a generated test pattern.

mod camera;
mod synthetic;
pub mod y4m;

pub use camera::{list_cameras, request_camera_permission, CameraDescription, CameraSource};
pub use synthetic::SyntheticSource;
pub use y4m::{Y4mSource, Y4mWriter};

use ec_core::Frame;

#[derive(Debug)]
pub enum CaptureError {
    /// The operating system refused camera access.
    PermissionDenied,
    /// No camera, or not the one asked for.
    NoCamera(String),
    /// The camera or its driver reported a failure.
    Camera(String),
    /// A clip file could not be read.
    Io(std::io::Error),
    /// A clip file is not in a form we read.
    BadClip(String),
    /// The source delivered a frame that does not fit its stated format.
    BadFrame(String),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::PermissionDenied => write!(
                f,
                "camera access was refused (System Settings > Privacy & Security > Camera)"
            ),
            CaptureError::NoCamera(s) => write!(f, "no usable camera: {s}"),
            CaptureError::Camera(s) => write!(f, "camera error: {s}"),
            CaptureError::Io(e) => write!(f, "clip file error: {e}"),
            CaptureError::BadClip(s) => write!(f, "unsupported clip: {s}"),
            CaptureError::BadFrame(s) => write!(f, "bad frame from source: {s}"),
        }
    }
}

impl std::error::Error for CaptureError {}

impl From<std::io::Error> for CaptureError {
    fn from(e: std::io::Error) -> Self {
        CaptureError::Io(e)
    }
}

/// Anything that hands out frames one at a time.
pub trait FrameSource {
    /// The next frame. `Ok(None)` means the source has ended (a clip ran out).
    /// A camera blocks here until its next frame; a clip returns at once.
    fn next_frame(&mut self) -> Result<Option<Frame>, CaptureError>;

    /// One line for logs: what this source is and what it delivers.
    fn describe(&self) -> String;

    /// Frames per second the source is meant to be played at, if it knows.
    fn nominal_fps(&self) -> Option<f64>;
}
