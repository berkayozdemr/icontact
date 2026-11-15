//! Where the eyes are looking, compared with the camera (spec §3.2 stage 3, T-052).
//!
//! All angles here are a pair of degrees, `[up, right]`: up is positive, and
//! "right" means towards the right of the picture. The warp model's turn uses
//! the same order and signs.
//!
//! The estimate has two steps.
//!
//! 1. A **reading**: how far the iris sits from the middle of the eye, turned
//!    into degrees of eyeball rotation, plus the head's own angle. It is steady
//!    from frame to frame but has an unknown offset that differs per person
//!    (on the one face measured so far the iris sits about 8 degrees "up" from
//!    the line through the eye corners while looking at the screen).
//! 2. A **zero point** that removes the offset: [`Calibration`]. Either measured
//!    ("look at the camera", the reading at that moment is zero), or assumed:
//!    the place the person looks at most is taken to be the screen, a fixed
//!    angle below the camera.
//!
//! NOT MEASURED: T-011 (accuracy against known targets) has never been run, so
//! nobody knows yet how many degrees this is off by.

use crate::eye::{Eye, Side};
use crate::smooth::OneEuro;
use crate::tracker::Face;

/// Degrees: `[up, right]`.
pub type Angles = [f32; 2];

/// Eyeball radius divided by iris width, for an average adult (12 mm / 11.7 mm).
/// The iris width in pixels is measured, so this gives the eyeball's size in pixels.
const EYEBALL_OVER_IRIS: f32 = 12.0 / 11.7;
/// Without calibration: how far above the screen's middle the camera is taken to
/// be, in degrees, as seen from the person. A laptop camera at arm's length.
pub const DEFAULT_CAMERA_ABOVE: f32 = 10.0;
/// Without calibration, "where the person usually looks" is an average over
/// about this many seconds.
const USUAL_SECONDS: f32 = 60.0;
/// A calibration needs at least this many readings (a third of a second).
const MIN_CALIBRATION_READINGS: usize = 10;
/// One Euro settings for the gaze, in degrees (see `smooth.rs`). Steady when the
/// eyes rest, quick on a glance: at 50 degrees a second the cutoff is about 11 Hz.
const SMOOTH_MIN_CUTOFF: f32 = 1.0;
const SMOOTH_BETA: f32 = 0.2;

/// One eye's reading: the iris's offset from the middle of the eye, as the
/// eyeball rotation that would put it there. `None` if the iris has no size.
pub fn eye_reading(eye: &Eye) -> Option<Angles> {
    let radius = eye.iris_diameter * EYEBALL_OVER_IRIS;
    if !radius.is_finite() || radius < 1.0 {
        return None;
    }
    let (a, b) = (eye.left_corner, eye.right_corner);
    let d = [b[0] - a[0], b[1] - a[1]];
    let len = (d[0] * d[0] + d[1] * d[1]).sqrt();
    if len < 1e-3 {
        return None;
    }
    // "along" runs from the picture-left corner to the other; "down" is square to it
    let along = [d[0] / len, d[1] / len];
    let down = [-along[1], along[0]];
    let from_mid = [
        eye.iris[0] - (a[0] + b[0]) / 2.0,
        eye.iris[1] - (a[1] + b[1]) / 2.0,
    ];
    let right = from_mid[0] * along[0] + from_mid[1] * along[1];
    let below = from_mid[0] * down[0] + from_mid[1] * down[1];
    // An eyeball of radius r turned by an angle moves the iris by r * sin(angle).
    let angle = |shift: f32| (shift / radius).clamp(-1.0, 1.0).asin().to_degrees();
    Some([angle(-below), angle(right)])
}

/// The whole face's reading: the two eyes' average, plus the head's own angle.
///
/// The head is added because the iris offset is measured inside the eye: turn
/// the head right while still looking at the camera and the irises slide left
/// by the same angle. The sum stays put.
pub fn face_reading(face: &Face) -> Option<Angles> {
    let mut sum = [0.0f32; 2];
    let mut n = 0.0;
    for side in Side::BOTH {
        if let Some(r) = Eye::from_points(&face.points, side).and_then(|e| eye_reading(&e)) {
            sum[0] += r[0];
            sum[1] += r[1];
            n += 1.0;
        }
    }
    (n > 0.0).then(|| [sum[0] / n + face.head.pitch, sum[1] / n + face.head.yaw])
}

/// What a reading of "looking into the camera" is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Calibration {
    /// Not calibrated. The place the person looks at most is taken to be the
    /// middle of the screen, and the camera to sit `camera_above` degrees above it.
    Assumed { camera_above: f32 },
    /// Calibrated: the reading recorded while the person looked into the camera.
    Measured { at_camera: Angles },
}

impl Default for Calibration {
    fn default() -> Self {
        Calibration::Assumed {
            camera_above: DEFAULT_CAMERA_ABOVE,
        }
    }
}

impl Calibration {
    /// Make a calibration from readings taken while the person looked into the
    /// camera. Uses the middle value of each angle, so a blink or a glance away
    /// during the recording does not spoil it. `None` if there are too few.
    pub fn from_readings(readings: &[Angles]) -> Option<Calibration> {
        if readings.len() < MIN_CALIBRATION_READINGS {
            return None;
        }
        let middle = |i: usize| {
            let mut v: Vec<f32> = readings.iter().map(|r| r[i]).collect();
            v.sort_by(|a, b| a.total_cmp(b));
            v[v.len() / 2]
        };
        Some(Calibration::Measured {
            at_camera: [middle(0), middle(1)],
        })
    }
}

/// Turns readings into "how far from the camera the eyes look", smoothed.
pub struct GazeEstimator {
    pub calibration: Calibration,
    /// Running average of the readings: where this person usually looks.
    usual: Option<Angles>,
    usual_count: u32,
    filters: [OneEuro; 2],
    last_time: Option<f64>,
}

impl GazeEstimator {
    pub fn new(calibration: Calibration) -> GazeEstimator {
        GazeEstimator {
            calibration,
            usual: None,
            usual_count: 0,
            filters: [OneEuro::new(SMOOTH_MIN_CUTOFF, SMOOTH_BETA); 2],
            last_time: None,
        }
    }

    /// Feed one reading taken at `time` (seconds). Returns the gaze relative to
    /// the camera: `[0, 0]` is looking into it, `[-10, 0]` is 10 degrees below it.
    ///
    /// `trusted` says whether this reading should also teach the "usual" place
    /// (not during a blink or with the head turned away).
    pub fn update(&mut self, reading: Angles, time: f64, trusted: bool) -> Angles {
        let dt = self.last_time.map_or(0.0, |t| (time - t) as f32);
        // After a long gap the smoothing state says nothing about now.
        if self.last_time.is_some() && !(0.0..1.0).contains(&dt) {
            self.filters.iter_mut().for_each(OneEuro::reset);
        }
        self.last_time = Some(time);

        if trusted {
            self.usual_count = self.usual_count.saturating_add(1);
            // A plain average at first, then a slow follow, so the first seconds count fully.
            let share = (dt / USUAL_SECONDS).max(1.0 / self.usual_count as f32);
            let usual = self.usual.get_or_insert(reading);
            usual[0] += (reading[0] - usual[0]) * share;
            usual[1] += (reading[1] - usual[1]) * share;
        }
        let gaze = match self.calibration {
            Calibration::Measured { at_camera } => {
                [reading[0] - at_camera[0], reading[1] - at_camera[1]]
            }
            Calibration::Assumed { camera_above } => {
                let usual = self.usual.unwrap_or(reading);
                [reading[0] - usual[0] - camera_above, reading[1] - usual[1]]
            }
        };
        [
            self.filters[0].filter(gaze[0], dt),
            self.filters[1].filter(gaze[1], dt),
        ]
    }

    /// Forget everything except the calibration.
    pub fn reset(&mut self) {
        self.usual = None;
        self.usual_count = 0;
        self.last_time = None;
        self.filters.iter_mut().for_each(OneEuro::reset);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    /// An eye 60 px wide with a 23.4 px iris, so the eyeball radius is 24 px.
    fn eye(iris: [f32; 2]) -> Eye {
        Eye {
            side: Side::Left,
            left_corner: [100.0, 50.0],
            right_corner: [160.0, 50.0],
            iris,
            iris_diameter: 23.4,
        }
    }

    #[test]
    fn a_centred_iris_reads_zero() {
        assert_eq!(eye_reading(&eye([130.0, 50.0])), Some([0.0, 0.0]));
    }

    #[test]
    fn the_reading_is_the_eyeball_angle_with_up_and_right_positive() {
        // 12 px is half the radius: asin(0.5) = 30 degrees
        let r = eye_reading(&eye([142.0, 50.0])).unwrap();
        assert!(close(r[0], 0.0) && close(r[1], 30.0), "{r:?}");
        let r = eye_reading(&eye([130.0, 38.0])).unwrap();
        assert!(close(r[0], 30.0) && close(r[1], 0.0), "{r:?}");
        let r = eye_reading(&eye([118.0, 62.0])).unwrap();
        assert!(close(r[0], -30.0) && close(r[1], -30.0), "{r:?}");
    }

    #[test]
    fn a_tilted_head_does_not_change_the_reading() {
        // the same eye turned a quarter turn clockwise about (100, 50)
        let e = Eye {
            side: Side::Left,
            left_corner: [100.0, 50.0],
            right_corner: [100.0, 110.0],
            iris: [100.0 + 12.0, 80.0], // was 12 px "up" before the turn
            iris_diameter: 23.4,
        };
        let r = eye_reading(&e).unwrap();
        assert!(close(r[0], 30.0) && close(r[1], 0.0), "{r:?}");
    }

    #[test]
    fn an_eye_without_an_iris_gives_nothing() {
        let mut e = eye([130.0, 50.0]);
        e.iris_diameter = 0.0;
        assert_eq!(eye_reading(&e), None);
    }

    #[test]
    fn a_measured_calibration_is_subtracted() {
        let mut g = GazeEstimator::new(Calibration::Measured {
            at_camera: [8.0, 3.0],
        });
        assert_eq!(g.update([8.0, 3.0], 0.0, true), [0.0, 0.0]);
        let mut g = GazeEstimator::new(Calibration::Measured {
            at_camera: [8.0, 3.0],
        });
        assert_eq!(g.update([-2.0, 5.0], 0.0, true), [-10.0, 2.0]);
    }

    #[test]
    fn uncalibrated_the_usual_place_is_the_screen_below_the_camera() {
        let mut g = GazeEstimator::new(Calibration::default());
        // the person looks steadily at one place: taken to be the screen
        let mut gaze = [0.0; 2];
        for i in 0..90 {
            gaze = g.update([7.0, 4.0], i as f64 / 30.0, true);
        }
        assert!(close(gaze[0], -DEFAULT_CAMERA_ABOVE) && close(gaze[1], 0.0));
        // a glance 6 degrees to the right of it shows up as such, once the smoothing settles
        for i in 90..150 {
            gaze = g.update([7.0, 10.0], i as f64 / 30.0, false);
        }
        assert!((gaze[1] - 6.0).abs() < 0.3, "{gaze:?}");
        assert!(close(gaze[0], -DEFAULT_CAMERA_ABOVE));
    }

    #[test]
    fn untrusted_readings_do_not_move_the_usual_place() {
        let mut g = GazeEstimator::new(Calibration::default());
        g.update([0.0, 0.0], 0.0, true);
        for i in 1..300 {
            g.update([20.0, 20.0], i as f64 / 30.0, false);
        }
        assert_eq!(g.usual, Some([0.0, 0.0]));
    }

    #[test]
    fn calibration_takes_the_middle_reading_and_needs_enough_of_them() {
        assert_eq!(Calibration::from_readings(&[[1.0, 1.0]; 5]), None);
        let mut readings = vec![[8.0, 3.0]; 20];
        readings.extend([[-30.0, 40.0]; 5]); // a blink and a glance away
        assert_eq!(
            Calibration::from_readings(&readings),
            Some(Calibration::Measured {
                at_camera: [8.0, 3.0]
            })
        );
    }
}
