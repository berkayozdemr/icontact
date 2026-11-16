//! A small log file next to the shared block:
//! `%ProgramData%\Icontact\mfcam.log` on Windows.
//!
//! The camera library runs inside a Windows service. It has no window and no
//! console, so this file is the only way to see what it did. Every failure to
//! write is ignored: logging must never break the camera.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

/// Start over when the file grows past this, so it can never fill a disk.
const MAX_BYTES: u64 = 512 * 1024;

pub fn path() -> PathBuf {
    ec_sink::default_path().with_file_name("mfcam.log")
}

/// Add one line, with the time (milliseconds since 1970) and the process id
/// in front, so lines from the service and from other programs can be told apart.
pub fn line(text: &str) {
    let path = path();
    let too_big = std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES);
    let file = OpenOptions::new()
        .create(true)
        .append(!too_big)
        .write(true)
        .truncate(too_big)
        .open(&path);
    if let Ok(mut f) = file {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let _ = writeln!(f, "{ms} pid {} {text}", std::process::id());
    }
}
