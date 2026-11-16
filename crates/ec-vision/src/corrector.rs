//! Corrects both eyes of one tracked face in one frame.
//!
//! [`EyeCorrector::correct`] is the whole job in order: estimate the gaze, ask
//! the gate how strongly to act, run the warp model for each eye, and write the
//! turned eyes into the frame.

use std::time::Instant;

use ec_core::{Frame, Rect};

use crate::eye::{model_input, Eye, EyeBox, EyeCrop, Side};
use crate::gate::{self, Factors, Fade, GateInput};
use crate::gaze::{face_reading, Angles, Calibration, GazeEstimator};
use crate::highlight;
use crate::redirect;
use crate::tracker::Face;
use crate::warp::{Maps, WarpModel, MAX_TURN};
use crate::VisionError;

/// An eye whose strength is below this is not touched at all.
const MIN_STRENGTH: f32 = 0.005;
/// A turn smaller than this many degrees is not worth running the model for.
const MIN_TURN: f32 = 0.05;
/// A reading only teaches the "usual gaze" when the eyes are at least this open
/// and the head is turned away no more than this many degrees.
const TRUSTED_OPEN: f32 = 0.18;
const TRUSTED_HEAD: f32 = 20.0;

/// Steadying the warp model's answer over time. The model's answer wobbles a
/// little from frame to frame even on a still eye (camera noise in, wobble out),
/// and the wobble shows as shimmer. So each new answer is mixed into the last
/// one: by this share when the two are nearly alike...
const STEADY_SHARE: f32 = 0.25;
/// ...rising to "take the new one whole" when they differ by this much or more
/// (average fetch distance, 2.0 = the whole patch). A blink or a glance differs
/// by far more than noise does, so real movement is not held back.
const QUICK_CHANGE: f32 = 0.004;

/// Where the strength slider starts. Not 100%: at the full turn the current
/// eye model (v2) bends the lids and the iris enough to look odd on a live face.
/// History (Berkay, live, 2026-11-15): 100% looked weird, so this became 0.6;
/// 60% looked great but the eye contact was hardly felt, and what spoiled 100%
/// by then was the shrinking reflection. With the reflection kept whole
/// (`highlight.rs`) this went up to 0.8.
pub const DEFAULT_STRENGTH: f32 = 0.8;

/// What the user (or a test) can set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// The strength slider, 0 to 1.
    pub strength: f32,
    pub calibration: Calibration,
    /// For tests and comparisons: skip the gaze estimate and always ask for this
    /// turn (`[up, right]` in degrees). The rest of the gate still applies.
    pub fixed_turn: Option<Angles>,
    /// Keep the small white reflection in the eye its own size while the iris
    /// is turned (see `highlight.rs`). Off only for comparisons.
    pub keep_highlight: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            strength: DEFAULT_STRENGTH,
            calibration: Calibration::default(),
            fixed_turn: None,
            keep_highlight: true,
        }
    }
}

/// Where the time went for one frame, in milliseconds.
#[derive(Clone, Copy, Debug, Default)]
pub struct CorrectTimes {
    /// Gaze estimate and gate.
    pub gaze_ms: f64,
    /// Cutting the two eye regions out of the frame and shrinking them for the model.
    pub cut_ms: f64,
    /// The warp model, both eyes.
    pub model_ms: f64,
    /// Moving the pixels and writing them back into the frame.
    pub apply_ms: f64,
}

/// What was done to one frame. For the read-out, the score scripts and the tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct Correction {
    /// Where the eyes look relative to the camera, `[up, right]` in degrees.
    /// `None` if the eyes could not be read.
    pub gaze: Option<Angles>,
    /// The turn that would bring the gaze to the camera.
    pub needed: Angles,
    /// The gate's factors, picture-left eye first.
    pub factors: [Factors; 2],
    /// The strength after fading, 0 to 1, per eye.
    pub strength: [f32; 2],
    /// The turn actually applied, per eye.
    pub turn: [Angles; 2],
    /// How much the warp model's answer changed since the last frame, per eye
    /// (see `Maps::distance`). 0 when there was no last answer.
    pub map_change: [f32; 2],
    /// How many frame pixels were changed.
    pub changed_pixels: usize,
    pub times: CorrectTimes,
}

/// Everything about one eye in one frame, kept only when
/// [`EyeCorrector::keep_debug`] is on. Used to compare with the Python reference.
#[derive(Clone, Debug)]
pub struct EyeDebug {
    pub side: Side,
    pub eye_box: EyeBox,
    pub rect: Rect,
    /// `rect` of the frame as RGB, before and after.
    pub before: Vec<u8>,
    pub after: Vec<u8>,
    /// What the model was given (mirrored for the picture-right eye)...
    pub input: Vec<f32>,
    pub model_turn: Angles,
    /// ...and what it answered.
    pub maps: Maps,
}

pub struct EyeCorrector {
    warp: WarpModel,
    gaze: GazeEstimator,
    fades: [Fade; 2],
    /// The warp model's last answer per eye, for the ladder's "every second frame".
    maps: [Option<Maps>; 2],
    pub settings: Settings,
    pub keep_debug: bool,
    pub debug: Vec<EyeDebug>,
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

impl EyeCorrector {
    /// Loads the warp model. `threads` is how many CPU threads it may use.
    pub fn new(threads: usize) -> Result<EyeCorrector, VisionError> {
        Ok(EyeCorrector {
            warp: WarpModel::new(threads)?,
            gaze: GazeEstimator::new(Calibration::default()),
            fades: [Fade::default(); 2],
            maps: [None, None],
            settings: Settings::default(),
            keep_debug: false,
            debug: Vec::new(),
        })
    }

    /// Correct the eyes of `face` in `frame`. `time` is the frame's time in seconds.
    ///
    /// With `reuse_maps`, the warp model is not run for an eye that already has
    /// an answer from the previous frame; that answer is applied again at the
    /// eye's new position (the ladder's third level).
    pub fn correct(
        &mut self,
        frame: &mut Frame,
        face: &Face,
        time: f64,
        reuse_maps: bool,
    ) -> Result<Correction, VisionError> {
        let mut report = Correction::default();
        self.debug.clear();

        // Stage 3a: where are the eyes looking?
        let t = Instant::now();
        let head_away = face.head.yaw.hypot(face.head.pitch);
        let eyes_open = face.eye_open[0].min(face.eye_open[1]);
        let trusted = eyes_open >= TRUSTED_OPEN && head_away <= TRUSTED_HEAD;
        self.gaze.calibration = self.settings.calibration;
        report.gaze = face_reading(face).map(|r| self.gaze.update(r, time, trusted));
        let (needed, needed_size, needed_sideways) = match (self.settings.fixed_turn, report.gaze) {
            // a fixed turn is never "too far"
            (Some(turn), _) => (Some(turn), 0.0, 0.0),
            // to look into the camera, undo the distance from it
            (None, Some(g)) => (Some([-g[0], -g[1]]), g[0].hypot(g[1]), g[1]),
            (None, None) => (None, 0.0, 0.0),
        };
        report.needed = needed.unwrap_or_default();

        // Stage 3b: how strongly, per eye (one eye can be shut while the other is open).
        for side in Side::BOTH {
            let i = side.index();
            report.factors[i] = gate::factors(&GateInput {
                needed_turn: needed_size,
                needed_sideways,
                head_away,
                eye_open: face.eye_open[i],
                confidence: face.confidence,
                user: self.settings.strength,
            });
            let target = match needed {
                Some(_) => report.factors[i].product(),
                None => 0.0,
            };
            report.strength[i] = self.fades[i].step(target, time);
            // Less strength means a smaller turn, never a see-through mix of two eyes.
            report.turn[i] = report
                .needed
                .map(|a| (a * report.strength[i]).clamp(-MAX_TURN, MAX_TURN));
        }
        report.times.gaze_ms = ms_since(t);

        // Stages 4 and 5, one eye at a time.
        for side in Side::BOTH {
            let i = side.index();
            let turn = report.turn[i];
            if report.strength[i] < MIN_STRENGTH || turn[0].hypot(turn[1]) < MIN_TURN {
                self.maps[i] = None;
                continue;
            }
            let t = Instant::now();
            let found = Eye::from_points(&face.points, side)
                .and_then(|eye| EyeBox::from_eye(&eye).map(|b| (eye, b)))
                .and_then(|(eye, b)| EyeCrop::cut(frame, &b).map(|crop| (eye, b, crop)));
            let Some((eye, eye_box, crop)) = found else {
                self.maps[i] = None;
                continue;
            };
            let mirrored = side.mirrored();
            // In the mirror image, "towards the right" is the other way.
            let model_turn = if mirrored { [turn[0], -turn[1]] } else { turn };
            let mut input_kept = Vec::new();
            if !(reuse_maps && self.maps[i].is_some()) {
                let input = model_input(&crop, &eye_box, mirrored);
                report.times.cut_ms += ms_since(t);
                if self.keep_debug {
                    input_kept = input.clone();
                }
                let t = Instant::now();
                let fresh = self.warp.run(input, model_turn)?;
                // Steady the answer: mix it into the last one, unless it moved a lot.
                match &mut self.maps[i] {
                    Some(last) => {
                        report.map_change[i] = last.distance(&fresh);
                        let share = STEADY_SHARE
                            + (1.0 - STEADY_SHARE) * (report.map_change[i] / QUICK_CHANGE).min(1.0);
                        last.blend_towards(&fresh, share);
                    }
                    None => self.maps[i] = Some(fresh),
                }
                report.times.model_ms += ms_since(t);
            } else {
                report.times.cut_ms += ms_since(t);
            }
            let Some(maps) = &self.maps[i] else {
                continue;
            };
            let t = Instant::now();
            // Lift the reflection off a copy of the eye picture, turn the eye
            // without it, and lay it back whole (the model above saw the eye
            // as the camera did, reflection and all).
            let mut bare = None;
            let mut light = None;
            if self.settings.keep_highlight {
                let mut copy = crop.clone();
                light = highlight::lift(&mut copy, &eye);
                if light.is_some() {
                    bare = Some(copy);
                }
            }
            report.changed_pixels += redirect::apply(
                frame,
                bare.as_ref().unwrap_or(&crop),
                &eye_box,
                maps,
                mirrored,
                light.as_ref(),
            )?;
            report.times.apply_ms += ms_since(t);

            if self.keep_debug {
                self.debug.push(EyeDebug {
                    side,
                    eye_box,
                    rect: crop.rect,
                    after: frame.crop_rgb(crop.rect)?,
                    before: crop.rgb,
                    input: input_kept,
                    model_turn,
                    maps: maps.clone(),
                });
            }
        }
        Ok(report)
    }

    /// Call when there is no face (or correction is switched off): the next
    /// correction then fades in from nothing instead of popping in.
    pub fn pause(&mut self) {
        self.fades.iter_mut().for_each(Fade::reset);
        self.maps = [None, None];
    }

    /// Forget the person too: the learned "usual gaze" starts over.
    pub fn reset(&mut self) {
        self.pause();
        self.gaze.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::HeadAngles;
    use crate::{points, Roi};
    use ec_core::PixelFormat;

    /// A made-up face on a 640 x 360 frame: eyes 60 px wide, irises centred.
    fn face(eye_open: [f32; 2]) -> Face {
        let mut p = vec![[0.0f32; 3]; 478];
        let mut eye = |a: usize, b: usize, iris: usize, x: f32| {
            p[a] = [x, 150.0, 0.0];
            p[b] = [x + 60.0, 150.0, 0.0];
            p[iris] = [x + 30.0, 150.0, 0.0];
            p[iris + 1] = [x + 42.0, 150.0, 0.0];
            p[iris + 3] = [x + 18.0, 150.0, 0.0];
            p[iris + 2] = [x + 30.0, 138.0, 0.0];
            p[iris + 4] = [x + 30.0, 162.0, 0.0];
        };
        eye(
            points::LEFT_EYE_OUTER,
            points::LEFT_EYE_INNER,
            points::LEFT_IRIS.start,
            220.0,
        );
        eye(
            points::RIGHT_EYE_INNER,
            points::RIGHT_EYE_OUTER,
            points::RIGHT_IRIS.start,
            360.0,
        );
        Face {
            points: p,
            confidence: 0.99,
            head: HeadAngles::default(),
            eye_open,
            roi: Roi {
                cx: 320.0,
                cy: 180.0,
                side: 300.0,
                rotation: 0.0,
            },
            redetected: false,
        }
    }

    fn textured_frame() -> Frame {
        let (w, h) = (640u32, 360u32);
        let mut rgb = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = if (x / 4 + y / 4) % 2 == 0 { 60 } else { 170 };
                rgb.extend_from_slice(&[v as u8, (v + x / 8) as u8, (v + y / 8) as u8]);
            }
        }
        let i420 = ec_core::convert::rgb_to_i420(&rgb, w, h);
        let nv12 = ec_core::convert::i420_to_nv12(&i420, w, h);
        Frame::new(w, h, PixelFormat::Nv12, nv12, 0).unwrap()
    }

    #[test]
    fn open_eyes_are_turned_and_the_strength_fades_in() {
        let mut c = EyeCorrector::new(1).unwrap();
        c.settings.strength = 1.0;
        let face = face([0.33, 0.33]);
        let mut strengths = Vec::new();
        let mut changed = 0;
        for i in 0..8 {
            let mut f = textured_frame();
            let r = c.correct(&mut f, &face, i as f64 / 30.0, false).unwrap();
            strengths.push(r.strength[0]);
            changed = r.changed_pixels;
            // uncalibrated: the steady gaze is taken to be the screen, so "turn up by 10"
            assert_eq!(r.needed, [crate::gaze::DEFAULT_CAMERA_ABOVE, 0.0]);
        }
        assert!(strengths[0] > 0.0 && strengths[0] < 0.3, "{strengths:?}");
        assert!(strengths.windows(2).all(|w| w[1] >= w[0]));
        assert_eq!(strengths[7], 1.0);
        assert!(changed > 500, "{changed}");
    }

    #[test]
    fn a_shut_eye_is_left_alone_while_the_open_one_is_turned() {
        let mut c = EyeCorrector::new(1).unwrap();
        c.settings.strength = 1.0;
        let face = face([0.05, 0.33]);
        let mut last = Correction::default();
        for i in 0..8 {
            let mut f = textured_frame();
            last = c.correct(&mut f, &face, i as f64 / 30.0, false).unwrap();
        }
        assert_eq!(last.strength, [0.0, 1.0]);
        assert_eq!(last.turn[0], [0.0, 0.0]);
    }

    #[test]
    fn zero_strength_changes_no_byte() {
        let mut c = EyeCorrector::new(1).unwrap();
        c.settings.strength = 0.0;
        let mut f = textured_frame();
        let before = f.data().to_vec();
        let r = c.correct(&mut f, &face([0.33, 0.33]), 0.0, false).unwrap();
        assert_eq!(r.changed_pixels, 0);
        assert_eq!(f.data(), &before[..]);
    }

    #[test]
    fn a_reused_answer_skips_the_model() {
        let mut c = EyeCorrector::new(1).unwrap();
        c.settings.fixed_turn = Some([10.0, 0.0]);
        let face = face([0.33, 0.33]);
        for i in 0..6 {
            let mut f = textured_frame();
            c.correct(&mut f, &face, i as f64 / 30.0, false).unwrap();
        }
        let mut f = textured_frame();
        let r = c.correct(&mut f, &face, 0.2, true).unwrap();
        assert_eq!(r.times.model_ms, 0.0);
        assert!(r.changed_pixels > 500);
    }
}
