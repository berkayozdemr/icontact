//! One Euro filter: steady when the face is still, quick when it moves.
//!
//! A plain smoothing filter has one dial: more smoothing means more lag. This
//! one turns the dial by itself. It watches how fast the value is changing and
//! smooths hard when slow (where jitter shows) and lightly when fast (where lag shows).
//! Casiez, Roussel, Vogel 2012, "1 € Filter".

use std::f32::consts::TAU;

/// The filter for one number.
#[derive(Clone, Copy, Debug)]
pub struct OneEuro {
    /// Smoothing when still, in Hz. Lower is steadier and laggier.
    pub min_cutoff: f32,
    /// How quickly smoothing lets go as speed rises. Higher is snappier.
    pub beta: f32,
    /// Smoothing of the speed estimate itself, in Hz.
    pub speed_cutoff: f32,
    last: Option<(f32, f32)>, // smoothed value, smoothed speed
}

/// The share of the new value to take, for a cutoff in Hz and a step in seconds.
fn share(cutoff: f32, dt: f32) -> f32 {
    let r = TAU * cutoff * dt;
    r / (r + 1.0)
}

impl OneEuro {
    pub fn new(min_cutoff: f32, beta: f32) -> OneEuro {
        OneEuro {
            min_cutoff,
            beta,
            speed_cutoff: 1.0,
            last: None,
        }
    }

    /// Feed one value, `dt` seconds after the previous one. Returns the smoothed value.
    pub fn filter(&mut self, value: f32, dt: f32) -> f32 {
        let Some((prev, prev_speed)) = self.last else {
            self.last = Some((value, 0.0));
            return value;
        };
        if dt <= 0.0 || dt.is_nan() {
            return prev;
        }
        let raw_speed = (value - prev) / dt;
        let speed = prev_speed + share(self.speed_cutoff, dt) * (raw_speed - prev_speed);
        let cutoff = self.min_cutoff + self.beta * speed.abs();
        let out = prev + share(cutoff, dt) * (value - prev);
        self.last = Some((out, speed));
        out
    }

    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// One filter per coordinate of a set of points.
///
/// Values are measured in face widths, so the same settings behave the same
/// whether the face fills the picture or sits far back.
pub struct PointFilter {
    filters: Vec<[OneEuro; 3]>,
    last_time: Option<f64>,
}

impl PointFilter {
    pub fn new(points: usize, min_cutoff: f32, beta: f32) -> PointFilter {
        PointFilter {
            filters: vec![[OneEuro::new(min_cutoff, beta); 3]; points],
            last_time: None,
        }
    }

    /// Smooth `points` in place. `time` is in seconds, `face_size` in the points' unit.
    pub fn apply(&mut self, points: &mut [[f32; 3]], time: f64, face_size: f32) {
        let dt = self.last_time.map_or(0.0, |t| (time - t) as f32);
        // After a long gap the old state says nothing about now.
        if self.last_time.is_some() && !(0.0..1.0).contains(&dt) {
            self.reset();
        }
        self.last_time = Some(time);
        let size = face_size.max(1.0);
        for (p, f) in points.iter_mut().zip(self.filters.iter_mut()) {
            for (v, f) in p.iter_mut().zip(f.iter_mut()) {
                *v = f.filter(*v / size, dt) * size;
            }
        }
    }

    pub fn reset(&mut self) {
        self.last_time = None;
        for f in self.filters.iter_mut().flatten() {
            f.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repeatable stand-in for noise, roughly -1..1.
    fn noise(i: u32) -> f32 {
        ((i.wrapping_mul(2_654_435_761) >> 8) % 2001) as f32 / 1000.0 - 1.0
    }

    fn spread(v: &[f32]) -> f32 {
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        (v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / v.len() as f32).sqrt()
    }

    #[test]
    fn first_value_passes_through() {
        let mut f = OneEuro::new(1.0, 0.0);
        assert_eq!(f.filter(5.0, 0.0), 5.0);
    }

    #[test]
    fn a_still_value_with_jitter_gets_steadier() {
        let mut f = OneEuro::new(1.0, 0.0);
        let raw: Vec<f32> = (0..300).map(|i| 10.0 + 0.01 * noise(i)).collect();
        let out: Vec<f32> = raw.iter().map(|v| f.filter(*v, 1.0 / 30.0)).collect();
        assert!(spread(&out[30..]) < spread(&raw[30..]) / 2.0);
    }

    #[test]
    fn a_fast_move_lags_less_with_beta() {
        let run = |beta: f32| {
            let mut f = OneEuro::new(1.0, beta);
            let mut out = 0.0;
            for i in 0..10 {
                out = f.filter(i as f32, 1.0 / 30.0); // moving 30 units a second
            }
            9.0 - out // how far behind after ten steps
        };
        assert!(run(1.0) < run(0.0) / 3.0, "{} vs {}", run(1.0), run(0.0));
    }

    #[test]
    fn point_filter_resets_after_a_gap() {
        let mut f = PointFilter::new(1, 0.5, 0.0);
        let mut p = [[0.0, 0.0, 0.0]];
        f.apply(&mut p, 0.0, 1.0);
        p = [[100.0, 0.0, 0.0]];
        f.apply(&mut p, 0.033, 1.0);
        assert!(p[0][0] < 50.0, "should lag: {}", p[0][0]);
        p = [[200.0, 0.0, 0.0]];
        f.apply(&mut p, 5.0, 1.0);
        assert_eq!(p[0][0], 200.0);
    }
}
