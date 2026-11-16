//! The hand-over point between the app and the fake camera.
//!
//! The fake camera does not live in our app. On Windows it is a small piece of
//! code that Windows (or Zoom, Teams, a browser) loads into *its own* process.
//! So finished frames must cross from one process to another. They do it
//! through one shared block of memory:
//! - the app is the only [`Writer`]; it puts every finished frame in;
//! - any number of [`Reader`]s (one per program showing the camera) take the
//!   newest frame out whenever they want one.
//!
//! "Newest frame wins": nobody ever waits. The writer never waits for a
//! reader, a reader never waits for the writer, and a frame nobody fetched in
//! time is simply replaced. A meeting app must never freeze because of us.
//!
//! That is the Windows route. On a Mac there is no camera of our own: frames
//! are pushed straight into OBS's installed camera instead (`obs_mac`).
//!
//! The exact byte layout is in [`layout`]. It is written down to the byte
//! because the Windows 10 camera that reads it is C++, not Rust.

pub mod layout;
#[cfg(windows)]
pub mod mf_win;
#[cfg(target_os = "macos")]
pub mod obs_mac;
mod reader;
mod writer;

use std::path::PathBuf;

pub use reader::{Fetch, Reader};
pub use writer::Writer;

#[derive(Debug)]
pub enum SinkError {
    /// The file could not be opened, sized or mapped.
    Io(std::io::Error),
    /// The file is not a frame block of a version this code understands.
    NotOurs(&'static str),
    /// The frame is bigger than the block was made for.
    TooBig { needed: usize, room: usize },
    /// Another running app is already writing into this block.
    Busy { pid: u32 },
    /// The fake camera could not be found or would not take a frame. The
    /// text says what is wrong, in words for the user.
    Camera(String),
}

impl std::fmt::Display for SinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SinkError::Io(e) => write!(f, "shared frame file: {e}"),
            SinkError::NotOurs(why) => write!(f, "not a usable shared frame file: {why}"),
            SinkError::TooBig { needed, room } => write!(
                f,
                "frame of {needed} bytes does not fit the shared block ({room} bytes per frame)"
            ),
            SinkError::Camera(why) => write!(f, "fake camera: {why}"),
            SinkError::Busy { pid } => {
                write!(
                    f,
                    "another Icontact (process {pid}) is already sending frames"
                )
            }
        }
    }
}

impl std::error::Error for SinkError {}

impl From<std::io::Error> for SinkError {
    fn from(e: std::io::Error) -> Self {
        SinkError::Io(e)
    }
}

/// The class id of the Windows 11 camera, as text with braces. Three places
/// must agree on it: the camera library (`ec-mfcam`, as a number), the install
/// script (which writes it into the registry) and the app (which hands it to
/// Windows to start the camera).
pub const WIN_CAMERA_CLSID: &str = "{3233B581-9067-4B31-A3F2-C1C07D73BA7E}";
/// The name meeting apps show for the Windows camera.
pub const WIN_CAMERA_NAME: &str = "Icontact Camera";

/// Where the shared block lives when nobody says otherwise.
///
/// It is an ordinary file that both sides map into memory. A file, not a named
/// memory block, because on Windows 11 the camera runs inside a Windows
/// service, in a different login session from the app, and a plain file is the
/// one thing both can open without special rights.
///
/// NOT TESTED ON WINDOWS. To settle there (T-061 / T-064b): that the camera
/// service may read this folder, and that Windows does not keep writing the
/// file's contents to disk.
pub fn default_path() -> PathBuf {
    if cfg!(windows) {
        let base = std::env::var_os("ProgramData").unwrap_or_else(|| "C:\\ProgramData".into());
        PathBuf::from(base).join("Icontact").join("frames.bin")
    } else {
        std::env::temp_dir().join("icontact-frames.bin")
    }
}

/// Milliseconds since 1970, the clock both sides share.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests;
