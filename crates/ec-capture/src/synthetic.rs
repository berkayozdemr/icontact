//! A generated moving picture, for benchmarks and tests that need no camera or file.

use ec_core::convert::{i420_to_nv12, i420_to_yuyv, rgb_to_i420};
use ec_core::{Frame, PixelFormat};

use crate::{CaptureError, FrameSource};

/// Cycles through a few pre-built frames, so producing one costs only a copy.
pub struct SyntheticSource {
    width: u32,
    height: u32,
    format: PixelFormat,
    frames: Vec<Vec<u8>>,
    seq: u64,
    limit: Option<u64>,
}

impl SyntheticSource {
    /// `limit`: stop after this many frames; `None` runs forever.
    pub fn new(
        width: u32,
        height: u32,
        format: PixelFormat,
        limit: Option<u64>,
    ) -> SyntheticSource {
        assert!(
            width.is_multiple_of(2) && height.is_multiple_of(2) && width > 0 && height > 0,
            "synthetic source needs an even, non-zero size"
        );
        let (w, h) = (width as usize, height as usize);
        let frames = (0..8usize)
            .map(|k| {
                let mut rgb = Vec::with_capacity(w * h * 3);
                for y in 0..h {
                    for x in 0..w {
                        let s = x + k * 16;
                        rgb.extend_from_slice(&[
                            (s * 255 / (w + 128)) as u8,
                            (y * 255 / h) as u8,
                            (((s / 32) + (y / 32)) % 2 * 160 + 40) as u8,
                        ]);
                    }
                }
                match format {
                    PixelFormat::Rgb24 => rgb,
                    PixelFormat::Bgr24 => rgb
                        .as_chunks::<3>()
                        .0
                        .iter()
                        .flat_map(|p| [p[2], p[1], p[0]])
                        .collect(),
                    PixelFormat::I420 => rgb_to_i420(&rgb, width, height),
                    PixelFormat::Nv12 => {
                        i420_to_nv12(&rgb_to_i420(&rgb, width, height), width, height)
                    }
                    PixelFormat::Yuyv => {
                        i420_to_yuyv(&rgb_to_i420(&rgb, width, height), width, height)
                    }
                }
            })
            .collect();
        SyntheticSource {
            width,
            height,
            format,
            frames,
            seq: 0,
            limit,
        }
    }
}

impl FrameSource for SyntheticSource {
    fn next_frame(&mut self) -> Result<Option<Frame>, CaptureError> {
        if self.limit.is_some_and(|l| self.seq >= l) {
            return Ok(None);
        }
        let data = self.frames[(self.seq % self.frames.len() as u64) as usize].clone();
        let frame = Frame::new(self.width, self.height, self.format, data, self.seq)
            .map_err(|e| CaptureError::BadFrame(e.to_string()))?;
        self.seq += 1;
        Ok(Some(frame))
    }

    fn describe(&self) -> String {
        format!(
            "test pattern {}x{} {:?}",
            self.width, self.height, self.format
        )
    }

    fn nominal_fps(&self) -> Option<f64> {
        Some(30.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stops_at_the_limit_and_counts_up() {
        let mut s = SyntheticSource::new(32, 16, PixelFormat::Nv12, Some(3));
        let seqs: Vec<u64> = std::iter::from_fn(|| s.next_frame().unwrap())
            .map(|f| f.seq)
            .collect();
        assert_eq!(seqs, vec![0, 1, 2]);
    }

    #[test]
    fn every_format_builds_valid_frames_that_move() {
        for fmt in [
            PixelFormat::Nv12,
            PixelFormat::I420,
            PixelFormat::Yuyv,
            PixelFormat::Rgb24,
            PixelFormat::Bgr24,
        ] {
            let mut s = SyntheticSource::new(64, 32, fmt, None);
            let a = s.next_frame().unwrap().unwrap();
            let b = s.next_frame().unwrap().unwrap();
            assert_eq!(a.data().len(), fmt.byte_len(64, 32));
            assert_ne!(a.data(), b.data(), "{fmt:?} does not change between frames");
        }
    }
}
