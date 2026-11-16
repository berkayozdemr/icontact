//! A video frame that stays in the format the camera delivered.
//!
//! Converting a whole 1280x720 frame to RGB and back costs more than the eye
//! correction itself, so the pipeline only converts the small regions it needs
//! ([`Frame::crop_rgb`]). The whole-frame conversion ([`Frame::to_rgb`]) exists
//! for the preview window.

use std::sync::Arc;
use std::time::Instant;

use crate::convert;

/// Pixel layouts the pipeline understands without decoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// Full-size Y plane, then one half-size plane of interleaved U,V.
    Nv12,
    /// Full-size Y plane, then half-size U plane, then half-size V plane.
    I420,
    /// Packed Y0 U Y1 V, two pixels per four bytes.
    Yuyv,
    /// Packed R,G,B.
    Rgb24,
    /// Packed B,G,R.
    Bgr24,
}

impl PixelFormat {
    /// Bytes a frame of this format and size must hold.
    pub fn byte_len(self, width: u32, height: u32) -> usize {
        let (w, h) = (width as usize, height as usize);
        match self {
            PixelFormat::Nv12 | PixelFormat::I420 => w * h + 2 * (w / 2) * (h / 2),
            PixelFormat::Yuyv => w * h * 2,
            PixelFormat::Rgb24 | PixelFormat::Bgr24 => w * h * 3,
        }
    }

    /// The colour planes are half-size, so width and height must be even.
    pub fn needs_even_size(self) -> bool {
        matches!(
            self,
            PixelFormat::Nv12 | PixelFormat::I420 | PixelFormat::Yuyv
        )
    }
}

/// A rectangle in pixels, origin top-left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Width or height is zero, or odd for a format with half-size colour planes.
    BadSize { width: u32, height: u32 },
    /// The buffer does not hold exactly the bytes the format and size need.
    BadLength { expected: usize, got: usize },
    /// A crop rectangle reaches outside the frame or is empty.
    BadRect(Rect),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::BadSize { width, height } => {
                write!(f, "unusable frame size {width}x{height}")
            }
            FrameError::BadLength { expected, got } => {
                write!(f, "frame buffer holds {got} bytes, format needs {expected}")
            }
            FrameError::BadRect(r) => write!(f, "crop {r:?} is empty or outside the frame"),
        }
    }
}

impl std::error::Error for FrameError {}

/// One video frame. Cheap to clone: the pixels are shared.
#[derive(Clone, Debug)]
pub struct Frame {
    width: u32,
    height: u32,
    format: PixelFormat,
    data: Arc<Vec<u8>>,
    /// When the source handed the frame over. Used to measure added delay.
    pub captured_at: Instant,
    /// Counts up by one per frame from the same source.
    pub seq: u64,
}

impl Frame {
    pub fn new(
        width: u32,
        height: u32,
        format: PixelFormat,
        data: Vec<u8>,
        seq: u64,
    ) -> Result<Frame, FrameError> {
        if width == 0
            || height == 0
            || (format.needs_even_size() && (!width.is_multiple_of(2) || !height.is_multiple_of(2)))
        {
            return Err(FrameError::BadSize { width, height });
        }
        let expected = format.byte_len(width, height);
        if data.len() != expected {
            return Err(FrameError::BadLength {
                expected,
                got: data.len(),
            });
        }
        Ok(Frame {
            width,
            height,
            format,
            data: Arc::new(data),
            captured_at: Instant::now(),
            seq,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn format(&self) -> PixelFormat {
        self.format
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// The whole frame as packed RGB. For the preview only.
    pub fn to_rgb(&self) -> Vec<u8> {
        let full = Rect {
            x: 0,
            y: 0,
            w: self.width,
            h: self.height,
        };
        let mut out = vec![0u8; (self.width * self.height * 3) as usize];
        convert::region_to_rgb(self, full, &mut out);
        out
    }

    /// One region as packed RGB, `rect.w * rect.h * 3` bytes.
    pub fn crop_rgb(&self, rect: Rect) -> Result<Vec<u8>, FrameError> {
        if !self.contains(rect) {
            return Err(FrameError::BadRect(rect));
        }
        let mut out = vec![0u8; (rect.w * rect.h * 3) as usize];
        convert::region_to_rgb(self, rect, &mut out);
        Ok(out)
    }

    /// Mix an RGB patch into `rect`, writing the frame's own format.
    ///
    /// `rgb` is `rect.w * rect.h * 3` bytes and `alpha` one byte per pixel:
    /// 0 keeps the frame's pixel untouched, 255 replaces it.
    ///
    /// The pixels are shared between clones of a frame (`Arc`). `Arc::make_mut`
    /// gives us our own copy first if anyone else still holds them, so a clone
    /// kept elsewhere (the "original" for the preview) is never changed.
    pub fn blend_rgb(&mut self, rect: Rect, rgb: &[u8], alpha: &[u8]) -> Result<(), FrameError> {
        if !self.contains(rect) {
            return Err(FrameError::BadRect(rect));
        }
        let pixels = (rect.w * rect.h) as usize;
        if rgb.len() != pixels * 3 || alpha.len() != pixels {
            return Err(FrameError::BadLength {
                expected: pixels * 3,
                got: rgb.len(),
            });
        }
        let (w, h, format) = (self.width, self.height, self.format);
        let data: &mut Vec<u8> = Arc::make_mut(&mut self.data);
        convert::blend_region(data, w, h, format, rect, rgb, alpha);
        Ok(())
    }

    /// True if `rect` is not empty and lies inside the frame.
    fn contains(&self, rect: Rect) -> bool {
        rect.w > 0
            && rect.h > 0
            && rect.x.checked_add(rect.w).is_some_and(|r| r <= self.width)
            && rect.y.checked_add(rect.h).is_some_and(|b| b <= self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_lengths() {
        assert_eq!(PixelFormat::Nv12.byte_len(1280, 720), 1_382_400);
        assert_eq!(PixelFormat::I420.byte_len(1280, 720), 1_382_400);
        assert_eq!(PixelFormat::Yuyv.byte_len(1280, 720), 1_843_200);
        assert_eq!(PixelFormat::Rgb24.byte_len(1280, 720), 2_764_800);
    }

    #[test]
    fn rejects_wrong_length_and_odd_sizes() {
        assert_eq!(
            Frame::new(4, 2, PixelFormat::Nv12, vec![0; 11], 0).unwrap_err(),
            FrameError::BadLength {
                expected: 12,
                got: 11
            }
        );
        assert_eq!(
            Frame::new(3, 2, PixelFormat::Nv12, vec![0; 9], 0).unwrap_err(),
            FrameError::BadSize {
                width: 3,
                height: 2
            }
        );
        assert_eq!(
            Frame::new(0, 2, PixelFormat::Rgb24, vec![], 0).unwrap_err(),
            FrameError::BadSize {
                width: 0,
                height: 2
            }
        );
        // packed RGB has no half-size planes, so odd sizes are fine
        assert!(Frame::new(3, 1, PixelFormat::Rgb24, vec![0; 9], 0).is_ok());
    }

    #[test]
    fn crop_must_be_inside() {
        let f = Frame::new(4, 4, PixelFormat::Rgb24, vec![0; 48], 0).unwrap();
        assert!(f
            .crop_rgb(Rect {
                x: 0,
                y: 0,
                w: 4,
                h: 4
            })
            .is_ok());
        assert!(f
            .crop_rgb(Rect {
                x: 2,
                y: 2,
                w: 3,
                h: 1
            })
            .is_err());
        assert!(f
            .crop_rgb(Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 1
            })
            .is_err());
        assert!(f
            .crop_rgb(Rect {
                x: u32::MAX,
                y: 0,
                w: 2,
                h: 1
            })
            .is_err());
    }
}
