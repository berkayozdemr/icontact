//! The Windows 11 camera: `icontact_mfcam.dll`.
//!
//! When a meeting app opens "Icontact Camera", Windows' camera service loads
//! this library and asks it for pictures, 30 times a second. The library does
//! no eye correction itself. It only reads the newest finished frame that the
//! Icontact app left in the shared block (`ec-sink`) and hands it on.
//!
//! - `feed.rs`: get the newest frame and fit it into the fixed picture the
//!   camera promises (1280x720, NV12). Plain Rust; tested on any system.
//! - `com.rs`: the Windows side: the objects Windows talks to. Windows only.
//! - `log.rs`: a small log file, because this code runs inside a Windows
//!   service where nothing can be printed.
//!
//! WRITTEN ON A MAC AND ONLY COMPILE-CHECKED FOR WINDOWS. Until someone has
//! seen it work on a Windows 11 machine, treat every line of `com.rs` as a
//! first draft.

pub mod feed;
pub mod log;

#[cfg(windows)]
mod com;
