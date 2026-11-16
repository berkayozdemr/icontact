//! The webcam, through the `nokhwa` crate.
//!
//! Run against one camera so far (MacBook Pro, 2026-11-13): it delivers 1280x720
//! YUYV at 30 fps and offers no NV12. Windows, external webcams and MJPEG cameras
//! have not been tried (T-022).

use std::sync::mpsc;
use std::time::Duration;

use ec_core::{Frame, PixelFormat};
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{
    ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType,
    Resolution,
};
use nokhwa::Camera;

use crate::{CaptureError, FrameSource};

pub struct CameraDescription {
    pub index: u32,
    pub name: String,
}

/// Ask the operating system for camera access and wait for the answer.
/// On macOS this shows the permission dialog the first time; elsewhere it returns at once.
pub fn request_camera_permission(wait: Duration) -> Result<(), CaptureError> {
    let (tx, rx) = mpsc::channel();
    nokhwa::nokhwa_initialize(move |granted| {
        let _ = tx.send(granted);
    });
    match rx.recv_timeout(wait) {
        Ok(true) => Ok(()),
        _ => Err(CaptureError::PermissionDenied),
    }
}

pub fn list_cameras() -> Result<Vec<CameraDescription>, CaptureError> {
    let found = nokhwa::query(ApiBackend::Auto).map_err(|e| CaptureError::Camera(e.to_string()))?;
    Ok(found
        .iter()
        .filter_map(|c| match c.index() {
            CameraIndex::Index(i) => Some(CameraDescription {
                index: *i,
                name: c.human_name(),
            }),
            CameraIndex::String(_) => None,
        })
        .collect())
}

/// Is this a fake camera that programs feed, not a real one? Icontact must
/// never read from these by default: OBS's camera is where our own picture
/// goes out, so reading it would mean watching our own output (and when
/// nothing feeds it, it offers a layout we cannot read at all).
///
/// Judged by name, because that is all the camera list gives us. Seen on a
/// Mac the day after OBS was set up: OBS's camera had become number 0 and the
/// app failed to start (2026-11-16).
pub fn is_fake_camera(name: &str) -> bool {
    let name = name.to_lowercase();
    name.contains("virtual") || name.contains("icontact")
}

/// The camera to use when the user named none: the first real one in the
/// list. `None` if the list has only fake cameras or is empty.
pub fn pick_real_camera(cameras: &[CameraDescription]) -> Option<u32> {
    cameras
        .iter()
        .find(|c| !is_fake_camera(&c.name))
        .map(|c| c.index)
}

pub struct CameraSource {
    camera: Camera,
    seq: u64,
    name: String,
}

impl CameraSource {
    /// Open camera `index` as close as it offers to `width` x `height` at `fps`.
    /// Call [`request_camera_permission`] first.
    pub fn open(
        index: u32,
        width: u32,
        height: u32,
        fps: u32,
    ) -> Result<CameraSource, CaptureError> {
        // Ask for the size and rate in each layout we can use without decoding, then MJPEG,
        // then whatever the camera prefers. A single "closest to NV12" request is refused
        // outright by cameras that do not offer NV12 (seen on a MacBook camera, 2026-11-13).
        let mut last_error = String::from("no format tried");
        let mut opened = None;
        let layouts = [FrameFormat::NV12, FrameFormat::YUYV, FrameFormat::MJPEG];
        let requests = layouts
            .iter()
            .map(|f| {
                RequestedFormatType::Closest(CameraFormat::new(
                    Resolution::new(width, height),
                    *f,
                    fps,
                ))
            })
            .chain([
                RequestedFormatType::AbsoluteHighestFrameRate,
                RequestedFormatType::None,
            ]);
        for request in requests {
            match Camera::new(
                CameraIndex::Index(index),
                RequestedFormat::new::<RgbFormat>(request),
            ) {
                Ok(c) => {
                    opened = Some(c);
                    break;
                }
                Err(e) => last_error = e.to_string(),
            }
        }
        let mut camera = opened.ok_or(CaptureError::NoCamera(last_error))?;
        camera
            .open_stream()
            .map_err(|e| CaptureError::Camera(e.to_string()))?;
        let name = camera.info().human_name();
        Ok(CameraSource {
            camera,
            seq: 0,
            name,
        })
    }
}

impl CameraSource {
    /// Open the first real camera (see [`pick_real_camera`]).
    pub fn open_default(width: u32, height: u32, fps: u32) -> Result<CameraSource, CaptureError> {
        let index = pick_real_camera(&list_cameras()?).ok_or_else(|| {
            CaptureError::NoCamera("no camera found (fake cameras are not counted)".into())
        })?;
        CameraSource::open(index, width, height, fps)
    }

    /// Every mode the camera offers, as text, for diagnosis.
    pub fn offered_formats(&mut self) -> Vec<String> {
        let mut v: Vec<String> = self
            .camera
            .compatible_camera_formats()
            .map(|f| f.iter().map(|c| c.to_string()).collect())
            .unwrap_or_default();
        v.sort();
        v.dedup();
        v
    }
}

impl FrameSource for CameraSource {
    fn next_frame(&mut self) -> Result<Option<Frame>, CaptureError> {
        let buffer = self
            .camera
            .frame()
            .map_err(|e| CaptureError::Camera(e.to_string()))?;
        let res = buffer.resolution();
        let (w, h) = (res.width(), res.height());
        let native = match buffer.source_frame_format() {
            FrameFormat::NV12 => Some(PixelFormat::Nv12),
            FrameFormat::YUYV => Some(PixelFormat::Yuyv),
            FrameFormat::RAWRGB => Some(PixelFormat::Rgb24),
            FrameFormat::RAWBGR => Some(PixelFormat::Bgr24),
            FrameFormat::MJPEG | FrameFormat::GRAY => None,
        };
        // Keep the camera's own bytes when they fit the stated layout (D-6). Otherwise
        // (MJPEG, grey, or a driver whose buffer is not the size its label implies) decode to RGB.
        let frame = match native {
            Some(fmt)
                if buffer.buffer().len() == fmt.byte_len(w, h)
                    && (!fmt.needs_even_size() || (w % 2 == 0 && h % 2 == 0)) =>
            {
                Frame::new(w, h, fmt, buffer.buffer().to_vec(), self.seq)
            }
            _ => {
                let rgb = buffer
                    .decode_image::<RgbFormat>()
                    .map_err(|e| CaptureError::BadFrame(e.to_string()))?;
                Frame::new(
                    rgb.width(),
                    rgb.height(),
                    PixelFormat::Rgb24,
                    rgb.into_raw(),
                    self.seq,
                )
            }
        }
        .map_err(|e| CaptureError::BadFrame(e.to_string()))?;
        self.seq += 1;
        Ok(Some(frame))
    }

    fn describe(&self) -> String {
        format!("camera \"{}\" {}", self.name, self.camera.camera_format())
    }

    fn nominal_fps(&self) -> Option<f64> {
        Some(self.camera.frame_rate() as f64)
    }
}

impl Drop for CameraSource {
    fn drop(&mut self) {
        let _ = self.camera.stop_stream();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam(index: u32, name: &str) -> CameraDescription {
        CameraDescription {
            index,
            name: name.into(),
        }
    }

    #[test]
    fn the_default_camera_is_the_first_real_one() {
        // the order seen on the MacBook once OBS's camera existed
        let list = [
            cam(0, "OBS Virtual Camera"),
            cam(1, "MacBook Pro Camera"),
            cam(2, "iPhone Desk View Camera"),
        ];
        assert_eq!(pick_real_camera(&list), Some(1));
    }

    #[test]
    fn only_fake_cameras_means_no_default() {
        assert_eq!(pick_real_camera(&[cam(0, "OBS Virtual Camera")]), None);
        assert_eq!(pick_real_camera(&[]), None);
    }
}
