//! The gate: how strongly to correct, from 0 (leave the eye alone) to 1.
//!
//! Each reason to hold back gives a factor from 0 to 1; the strength is their
//! product. The product then moves through a [`Fade`], so a change is spread
//! over about 150 ms and never snaps.
//!
//! The strength is used to shorten the turn (half strength = half the angle),
//! not to mix the raw and the corrected eye, which would show two irises.
//!
//! The limits below are the spec's first guesses. T-064 tunes them on real clips.
//! NOT BUILT: the "eye covered by a hand, hair or glare" factor. Nothing here
//! can tell yet that an eye is covered.

/// Turn needed: full strength up to here, in degrees...
const TURN_FULL: f32 = 15.0;
/// ...and none from here on. Beyond this the person is plainly looking elsewhere.
const TURN_ZERO: f32 = 30.0;
/// The sideways part of the turn has tighter limits of its own. The current eye
/// model (v2) smears the iris on sideways turns of about 10 degrees and more
/// (seen on real recordings), so a sideways glance is let
/// through as a glance. Raise these when a model handles sideways turns better.
const SIDE_FULL: f32 = 5.0;
const SIDE_ZERO: f32 = 12.0;
/// Head turned away from the camera, in degrees.
const HEAD_FULL: f32 = 20.0;
const HEAD_ZERO: f32 = 35.0;
/// Eye openness (lid gap over eye width). Measured on one face: about 0.36 open, 0.09 shut.
const OPEN_ZERO: f32 = 0.10;
const OPEN_FULL: f32 = 0.18;
/// How sure the face model is that it sees a face.
const SURE_ZERO: f32 = 0.5;
const SURE_FULL: f32 = 0.8;
/// A full fade from 0 to 1 (or back) takes this long.
pub const FADE_SECONDS: f32 = 0.15;

/// What the gate looks at, for one eye.
#[derive(Clone, Copy, Debug)]
pub struct GateInput {
    /// How far the gaze would have to be turned to reach the camera, in degrees.
    pub needed_turn: f32,
    /// The sideways part of that turn, in degrees.
    pub needed_sideways: f32,
    /// How far the head is turned away from the camera, in degrees.
    pub head_away: f32,
    /// This eye's openness (see `measure::eye_openness`).
    pub eye_open: f32,
    /// The tracker's confidence, 0 to 1.
    pub confidence: f32,
    /// The user's strength slider, 0 to 1.
    pub user: f32,
}

/// One factor per reason, each 0 to 1. Kept apart so the app can show which
/// one is holding the correction back.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Factors {
    pub turn: f32,
    pub side: f32,
    pub head: f32,
    pub open: f32,
    pub sure: f32,
    pub user: f32,
}

impl Factors {
    pub fn product(&self) -> f32 {
        self.turn * self.side * self.head * self.open * self.sure * self.user
    }
}

/// 1 at or below `full`, 0 at or above `zero`, a straight line between.
fn falling(x: f32, full: f32, zero: f32) -> f32 {
    ((zero - x) / (zero - full)).clamp(0.0, 1.0)
}

/// 0 at or below `zero`, 1 at or above `full`.
fn rising(x: f32, zero: f32, full: f32) -> f32 {
    ((x - zero) / (full - zero)).clamp(0.0, 1.0)
}

pub fn factors(i: &GateInput) -> Factors {
    // A number that is not a number (NaN) fails every comparison, so make it count as "no".
    let clean = |f: f32| if f.is_nan() { 0.0 } else { f };
    Factors {
        turn: clean(falling(i.needed_turn.abs(), TURN_FULL, TURN_ZERO)),
        side: clean(falling(i.needed_sideways.abs(), SIDE_FULL, SIDE_ZERO)),
        head: clean(falling(i.head_away.abs(), HEAD_FULL, HEAD_ZERO)),
        open: clean(rising(i.eye_open, OPEN_ZERO, OPEN_FULL)),
        sure: clean(rising(i.confidence, SURE_ZERO, SURE_FULL)),
        user: clean(i.user.clamp(0.0, 1.0)),
    }
}

/// Follows a target at a limited speed, so the strength fades instead of jumping.
#[derive(Clone, Copy, Debug, Default)]
pub struct Fade {
    value: f32,
    last_time: Option<f64>,
}

impl Fade {
    pub fn value(&self) -> f32 {
        self.value
    }

    /// Move towards `target` (0 to 1). `time` is in seconds. Returns the new value.
    pub fn step(&mut self, target: f32, time: f64) -> f32 {
        // The first call has no "time since last": start one frame's worth in.
        let dt = self
            .last_time
            .map_or(1.0 / 30.0, |t| (time - t) as f32)
            .max(0.0);
        self.last_time = Some(time);
        let reach = dt / FADE_SECONDS;
        let target = target.clamp(0.0, 1.0);
        self.value += (target - self.value).clamp(-reach, reach);
        self.value
    }

    /// Back to zero: after the face was lost, the correction fades in again.
    pub fn reset(&mut self) {
        *self = Fade::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_clear() -> GateInput {
        GateInput {
            needed_turn: 8.0,
            needed_sideways: 2.0,
            head_away: 5.0,
            eye_open: 0.33,
            confidence: 0.98,
            user: 1.0,
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn nothing_in_the_way_gives_full_strength() {
        assert_eq!(factors(&all_clear()).product(), 1.0);
    }

    #[test]
    fn needed_turn_fades_from_15_to_30_degrees() {
        let f = |turn| {
            factors(&GateInput {
                needed_turn: turn,
                ..all_clear()
            })
            .turn
        };
        assert_eq!(f(15.0), 1.0);
        assert!(close(f(22.5), 0.5));
        assert_eq!(f(30.0), 0.0);
        assert_eq!(f(60.0), 0.0);
        assert!(close(f(-22.5), 0.5)); // the direction does not matter
    }

    #[test]
    fn a_sideways_turn_fades_from_5_to_12_degrees() {
        let f = |side| {
            factors(&GateInput {
                needed_sideways: side,
                ..all_clear()
            })
            .side
        };
        assert_eq!(f(5.0), 1.0);
        assert!(close(f(-8.5), 0.5));
        assert_eq!(f(12.0), 0.0);
    }

    #[test]
    fn head_turn_fades_from_20_to_35_degrees() {
        let f = |head| {
            factors(&GateInput {
                head_away: head,
                ..all_clear()
            })
            .head
        };
        assert_eq!(f(20.0), 1.0);
        assert!(close(f(27.5), 0.5));
        assert_eq!(f(35.0), 0.0);
    }

    #[test]
    fn a_closing_eye_fades_out() {
        let f = |open| {
            factors(&GateInput {
                eye_open: open,
                ..all_clear()
            })
            .open
        };
        assert_eq!(f(0.36), 1.0);
        assert!(close(f(0.14), 0.5));
        assert_eq!(f(0.09), 0.0);
    }

    #[test]
    fn low_confidence_fades_out() {
        let f = |sure| {
            factors(&GateInput {
                confidence: sure,
                ..all_clear()
            })
            .sure
        };
        assert_eq!(f(0.95), 1.0);
        assert!(close(f(0.65), 0.5));
        assert_eq!(f(0.4), 0.0);
    }

    #[test]
    fn the_slider_scales_and_is_kept_in_range() {
        let f = |user| {
            factors(&GateInput {
                user,
                ..all_clear()
            })
            .product()
        };
        assert_eq!(f(0.4), 0.4);
        assert_eq!(f(0.0), 0.0);
        assert_eq!(f(7.0), 1.0);
    }

    #[test]
    fn the_factors_multiply() {
        let f = factors(&GateInput {
            needed_turn: 22.5,
            eye_open: 0.14,
            user: 0.5,
            ..all_clear()
        });
        assert!(close(f.product(), 0.125));
    }

    #[test]
    fn a_broken_number_closes_the_gate() {
        let f = factors(&GateInput {
            needed_turn: f32::NAN,
            ..all_clear()
        });
        assert_eq!(f.product(), 0.0);
    }

    #[test]
    fn the_fade_takes_150_ms_and_never_jumps() {
        let mut fade = Fade::default();
        let mut last = 0.0;
        let mut frames = 0;
        // 30 frames a second: full strength must take about 5 frames to arrive
        while fade.value() < 1.0 {
            let v = fade.step(1.0, frames as f64 / 30.0);
            assert!(v - last <= 1.0 / 30.0 / FADE_SECONDS + 1e-4);
            last = v;
            frames += 1;
        }
        assert_eq!(frames, 5);
        // and the same on the way down
        let v = fade.step(0.0, 5.0 / 30.0);
        assert!(v > 0.7 && v < 0.8, "{v}");
    }

    #[test]
    fn a_long_gap_lets_the_fade_finish() {
        let mut fade = Fade::default();
        fade.step(1.0, 0.0);
        assert_eq!(fade.step(1.0, 2.0), 1.0);
    }
}
