//! The whole per-frame job in one place: find the face, correct the eyes, and
//! keep an eye on the clock (spec §3.1).
//!
//! Both programs use this: `ec-app` on its pipeline thread, `ec-cli correct` in a loop.

use std::time::{Duration, Instant};

use ec_core::Frame;

use crate::corrector::{Correction, EyeCorrector};
use crate::ladder::{Ladder, Level};
use crate::tracker::{Face, FaceTracker};
use crate::VisionError;

/// A processed frame should take no longer than this. The spec's hard limit for
/// a whole frame is 28 ms; capture (4 ms) and the hand-off to the fake camera
/// (1 ms) happen outside the pipeline.
pub const FRAME_LIMIT_MS: f64 = 23.0;

/// One frame, after the pipeline.
pub struct Processed {
    /// The frame to show or send on. Corrected if correction is on and a face was found.
    pub frame: Frame,
    pub face: Option<Face>,
    /// `None` when nothing was corrected: no face, or correction off.
    pub correction: Option<Correction>,
    /// The ladder level this frame was processed at.
    pub level: Level,
    /// True when this is the previous picture again (the ladder's 15-a-second level).
    pub repeated: bool,
    /// Time spent finding the face. `None` when the last points were reused.
    pub track_ms: Option<f64>,
    /// Time spent on this frame in all, in milliseconds.
    pub total_ms: f64,
}

pub struct Pipeline {
    pub tracker: FaceTracker,
    pub corrector: EyeCorrector,
    pub ladder: Ladder,
    /// Correct the eyes. Off: frames are tracked but passed through unchanged.
    pub correcting: bool,
    /// For testing the ladder on a fast machine: behave as if the processor were
    /// this many times slower (by sleeping). 1.0 is off.
    pub slowdown: f32,
    last_face: Option<Face>,
    last_frame: Option<Frame>,
    seen: u64,
    processed: u64,
}

impl Pipeline {
    /// Loads all three models. `threads` is how many CPU threads each may use.
    pub fn new(threads: usize) -> Result<Pipeline, VisionError> {
        Ok(Pipeline {
            tracker: FaceTracker::new(threads)?,
            corrector: EyeCorrector::new(threads)?,
            ladder: Ladder::new(FRAME_LIMIT_MS),
            correcting: true,
            slowdown: 1.0,
            last_face: None,
            last_frame: None,
            seen: 0,
            processed: 0,
        })
    }

    /// Process one frame. `time` is the frame's time in seconds on any steady clock.
    ///
    /// Takes the frame by value: the pipeline owns it from here and hands the
    /// (possibly changed) frame back inside [`Processed`].
    pub fn process(&mut self, mut frame: Frame, time: f64) -> Result<Processed, VisionError> {
        let level = self.ladder.level();
        let number = self.seen;
        self.seen += 1;

        // Lowest level: every second frame is not processed; the last picture stands.
        if level == Level::HalfRate && number % 2 == 1 {
            if let Some(last) = &self.last_frame {
                return Ok(Processed {
                    frame: last.clone(),
                    face: self.last_face.clone(),
                    correction: None,
                    level,
                    repeated: true,
                    track_ms: None,
                    total_ms: 0.0,
                });
            }
        }

        let started = Instant::now();
        let second = self.processed % 2 == 1;
        self.processed += 1;

        // Stage 2: the face. At the lower levels, every second frame reuses the last points.
        let mut track_ms = None;
        let face = if level >= Level::SparseTracking && second && self.last_face.is_some() {
            self.last_face.clone()
        } else {
            let t = Instant::now();
            let found = self.tracker.track(&frame, time)?;
            track_ms = Some(t.elapsed().as_secs_f64() * 1000.0);
            found
        };

        // Stages 3 to 5: the eyes.
        let correction = match (&face, self.correcting) {
            (Some(face), true) => {
                let reuse = level >= Level::SparseModel && second;
                Some(self.corrector.correct(&mut frame, face, time, reuse)?)
            }
            _ => {
                self.corrector.pause();
                None
            }
        };

        if self.slowdown > 1.0 {
            std::thread::sleep(Duration::from_secs_f64(
                started.elapsed().as_secs_f64() * (self.slowdown as f64 - 1.0),
            ));
        }
        let total_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.ladder.report(total_ms);
        self.last_face = face.clone();
        // a clone shares the pixels, so keeping the last frame costs no copy
        self.last_frame = Some(frame.clone());
        Ok(Processed {
            frame,
            face,
            correction,
            level,
            repeated: false,
            track_ms,
            total_ms,
        })
    }

    /// Forget the face and the fades, as after switching tracking off.
    pub fn reset(&mut self) {
        self.tracker.reset();
        self.corrector.pause();
        self.last_face = None;
        self.last_frame = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::PixelFormat;

    fn blank(seq: u64) -> Frame {
        Frame::new(320, 240, PixelFormat::Rgb24, vec![60; 320 * 240 * 3], seq).unwrap()
    }

    #[test]
    fn a_frame_without_a_face_passes_through_untouched() {
        let mut p = Pipeline::new(1).unwrap();
        let out = p.process(blank(0), 0.0).unwrap();
        assert!(out.face.is_none() && out.correction.is_none() && !out.repeated);
        assert_eq!(out.frame.data(), blank(0).data());
        assert!(out.track_ms.is_some());
    }

    #[test]
    fn a_slow_machine_ends_at_half_rate_and_repeats_every_second_picture() {
        let mut p = Pipeline::new(1).unwrap();
        p.ladder.limit_ms = 0.0; // nothing is ever fast enough
        let mut levels = Vec::new();
        let mut repeats = Vec::new();
        for i in 0..80 {
            let out = p.process(blank(i), i as f64 / 30.0).unwrap();
            levels.push(out.level);
            repeats.push((out.repeated, out.frame.seq));
        }
        // down one level at a time, never back up
        assert!(levels.windows(2).all(|w| w[1] >= w[0]));
        assert_eq!(levels[79], Level::HalfRate);
        // at the bottom, a repeated picture is the one before it
        let (repeated, seq) = repeats[79];
        assert!(repeated && seq == 78, "{:?}", &repeats[70..]);
        assert!(!repeats[78].0);
    }

    #[test]
    fn the_pipeline_can_move_to_another_thread() {
        fn needs_send<T: Send>() {}
        needs_send::<Pipeline>();
    }
}
