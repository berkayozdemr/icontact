//! Where the camera's pictures come from.
//!
//! The camera promises meeting apps one fixed picture: [`WIDTH`] x [`HEIGHT`],
//! NV12, [`FPS`] a second. The promise is made before any frame has been seen,
//! and whether or not the Icontact app is running. `Feed` keeps that promise:
//! every call to [`Feed::next_picture`] returns one such picture, either the app's
//! newest frame fitted into it or a plain placeholder.

use std::path::PathBuf;
use std::time::Duration;

use ec_core::convert::frame_to_nv12_fit;
use ec_sink::{Fetch, Reader};

pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 720;
pub const FPS: u32 = 30;
/// Bytes in one NV12 picture: full-size brightness plus half-size colour.
pub const PICTURE_LEN: usize = (WIDTH * HEIGHT * 3 / 2) as usize;

/// How long to wait for the app's next frame before sending the last one
/// again. A little over one frame time (33 ms), so a frame that is a few
/// milliseconds late is still caught (see `Reader::fetch_within`).
const PATIENCE: Duration = Duration::from_millis(45);
/// One frame time, slept when there is nobody to wait for.
const FRAME_TIME: Duration = Duration::from_millis(1000 / FPS as u64);

pub struct Feed {
    path: PathBuf,
    /// Open while the app is sending. `None` until the block exists, and
    /// again after the app has gone, so a restarted app is picked up afresh.
    reader: Option<Reader>,
    picture: Vec<u8>,
    /// What `picture` holds right now.
    showing: Showing,
}

/// What kind of picture went out last. Used for the log and the tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Showing {
    /// A frame from the app.
    Live,
    /// The app is not sending: a dark picture with a grey band.
    Placeholder,
}

impl Feed {
    pub fn new(path: PathBuf) -> Feed {
        let mut feed = Feed {
            path,
            reader: None,
            picture: vec![0; PICTURE_LEN],
            showing: Showing::Placeholder,
        };
        feed.draw_placeholder();
        feed
    }

    /// The next picture to send. Takes about one frame time: it waits for the
    /// app's next frame, or sleeps when the app is not there. So the caller
    /// needs no clock of its own.
    pub fn next_picture(&mut self) -> (&[u8], Showing) {
        if self.reader.is_none() {
            self.reader = Reader::open(&self.path).ok();
        }
        let got = match &mut self.reader {
            Some(r) => r.fetch_within(PATIENCE),
            None => Ok(Fetch::WriterGone),
        };
        match got {
            Ok(Fetch::New(frame)) => {
                if frame_to_nv12_fit(&frame, WIDTH, HEIGHT, &mut self.picture) {
                    self.showing = Showing::Live;
                } else {
                    self.draw_placeholder();
                }
            }
            // The app is there but had no new frame in time: send the last
            // picture again. The wait already happened inside `fetch_within`.
            Ok(Fetch::NothingNew) => {}
            // The app closed, crashed, never ran, or remade the block.
            Ok(Fetch::WriterGone) | Err(_) => {
                self.reader = None;
                if self.showing != Showing::Placeholder {
                    self.draw_placeholder();
                }
                std::thread::sleep(FRAME_TIME);
            }
        }
        (&self.picture, self.showing)
    }

    /// Let go of the shared block, while nobody is watching the camera. The
    /// app can then remake the block; the next picture opens it again.
    pub fn close(&mut self) {
        self.reader = None;
    }

    /// A dark picture with a grey band across the middle: "the camera works,
    /// but Icontact is not sending". (A card with words on it is still to do.)
    fn draw_placeholder(&mut self) {
        let (w, h) = (WIDTH as usize, HEIGHT as usize);
        let (luma, colour) = self.picture.split_at_mut(w * h);
        luma.fill(28);
        for row in luma.chunks_mut(w).skip(h * 9 / 20).take(h / 10) {
            row.fill(90);
        }
        colour.fill(128); // no colour: grey
        self.showing = Showing::Placeholder;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::{Frame, PixelFormat};
    use ec_sink::Writer;

    /// A file name of our own in the temp folder, so tests do not collide.
    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("icontact-feed-{}-{name}.bin", std::process::id()))
    }

    #[test]
    fn with_no_app_the_placeholder_is_sent() {
        let mut feed = Feed::new(temp("nobody"));
        let (picture, showing) = feed.next_picture();
        assert_eq!(showing, Showing::Placeholder);
        assert_eq!(picture.len(), PICTURE_LEN);
        assert_eq!(picture[0], 28); // a dark corner
        let middle = (HEIGHT as usize / 2) * WIDTH as usize;
        assert_eq!(picture[middle], 90); // the grey band
    }

    #[test]
    fn the_apps_frame_comes_out_and_the_placeholder_returns_when_it_leaves() {
        let path = temp("live");
        // a small flat red frame in the layout a webcam might send
        let (w, h) = (64u32, 36u32);
        let rgb: Vec<u8> = std::iter::repeat_n([220u8, 30, 30], (w * h) as usize)
            .flatten()
            .collect();
        let frame = Frame::new(w, h, PixelFormat::Rgb24, rgb, 0).unwrap();
        let mut writer = Writer::create(&path, frame.data().len()).unwrap();
        writer.write(&frame).unwrap();

        let mut feed = Feed::new(path.clone());
        let (picture, showing) = feed.next_picture();
        assert_eq!(showing, Showing::Live);
        // 64x36 has the camera's 16:9 shape, so it fills the picture: every
        // brightness value is the red's.
        let y = ec_core::convert::rgb_to_yuv(220, 30, 30)[0];
        let luma = (WIDTH * HEIGHT) as usize;
        assert!(picture[..luma].iter().all(|&p| p == y));

        // nothing new: the same picture goes out again
        let (_, showing) = feed.next_picture();
        assert_eq!(showing, Showing::Live);

        // the app closes: back to the placeholder
        drop(writer);
        let (picture, showing) = feed.next_picture();
        assert_eq!(showing, Showing::Placeholder);
        assert_eq!(picture[0], 28);
        let _ = std::fs::remove_file(path);
    }
}
