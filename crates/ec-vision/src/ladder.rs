//! The step-down ladder: what to give up, in what order, when the machine is too
//! slow.
//!
//! The rule that matters: the ladder never switches the correction itself on
//! and off, because that flickers. It only makes the work cheaper.
//!
//! The steps differ a little from the spec's first list, because the work
//! turned out differently: there is no "half patch resolution" to fall back to
//! (the model always sees 96 x 64, and applying its answer is cheap). The three
//! costly parts are the face mesh model, the warp model, and doing either at all.

/// How much work the pipeline does per frame. Later levels include the earlier ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Everything, every frame.
    Full,
    /// The face mesh model runs every second frame; between, the last points are reused.
    SparseTracking,
    /// The warp model also runs every second frame; between, its last answer is
    /// applied again at the eye's new position.
    SparseModel,
    /// Only every second frame is processed at all: 15 pictures a second.
    HalfRate,
}

impl Level {
    fn down(self) -> Option<Level> {
        match self {
            Level::Full => Some(Level::SparseTracking),
            Level::SparseTracking => Some(Level::SparseModel),
            Level::SparseModel => Some(Level::HalfRate),
            Level::HalfRate => None,
        }
    }

    fn up(self) -> Option<Level> {
        match self {
            Level::Full => None,
            Level::SparseTracking => Some(Level::Full),
            Level::SparseModel => Some(Level::SparseTracking),
            Level::HalfRate => Some(Level::SparseModel),
        }
    }

    /// A short name for read-outs.
    pub fn name(self) -> &'static str {
        match self {
            Level::Full => "full",
            Level::SparseTracking => "tracking every 2nd frame",
            Level::SparseModel => "eye model every 2nd frame",
            Level::HalfRate => "15 pictures a second",
        }
    }
}

/// Frames averaged before deciding anything: half a second.
const WINDOW: usize = 15;
/// "Room to spare" means the average is under this share of the limit.
const ROOM: f64 = 0.6;
/// Calm frames needed before trying a step up: 3 seconds at first...
const UP_WAIT: u32 = 90;
/// ...doubling each time a step up had to be undone, up to a minute.
const UP_WAIT_MAX: u32 = 1800;
/// A step down this soon after a step up means the step up was a mistake.
const REGRET_FRAMES: u32 = 60;

/// Watches how long frames take and picks the level.
pub struct Ladder {
    level: Level,
    /// A processed frame should take no longer than this, in milliseconds.
    pub limit_ms: f64,
    window: Vec<f64>,
    calm: u32,
    up_wait: u32,
    since_step_up: Option<u32>,
}

impl Ladder {
    pub fn new(limit_ms: f64) -> Ladder {
        Ladder {
            level: Level::Full,
            limit_ms,
            window: Vec::with_capacity(WINDOW),
            calm: 0,
            up_wait: UP_WAIT,
            since_step_up: None,
        }
    }

    pub fn level(&self) -> Level {
        self.level
    }

    /// Tell the ladder how long the frame just processed took. Returns the
    /// level to use for the next frame.
    pub fn report(&mut self, ms: f64) -> Level {
        if let Some(n) = self.since_step_up.as_mut() {
            *n += 1;
        }
        self.window.push(ms);
        if self.window.len() < WINDOW {
            return self.level;
        }
        let average = self.window.iter().sum::<f64>() / self.window.len() as f64;
        if average > self.limit_ms {
            // Too slow: step down, and start measuring afresh at the new level.
            if let Some(lower) = self.level.down() {
                if self.since_step_up.is_some_and(|n| n <= REGRET_FRAMES) {
                    self.up_wait = (self.up_wait * 2).min(UP_WAIT_MAX);
                }
                self.level = lower;
            }
            self.window.clear();
            self.calm = 0;
            self.since_step_up = None;
        } else {
            // Slide the window along by one frame.
            self.window.remove(0);
            if average < self.limit_ms * ROOM {
                self.calm += 1;
            } else {
                self.calm = 0;
            }
            if self.calm >= self.up_wait {
                if let Some(higher) = self.level.up() {
                    self.level = higher;
                    self.since_step_up = Some(0);
                    self.window.clear();
                }
                self.calm = 0;
            }
        }
        self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(l: &mut Ladder, ms: f64, frames: usize) -> Level {
        let mut level = l.level();
        for _ in 0..frames {
            level = l.report(ms);
        }
        level
    }

    #[test]
    fn a_fast_machine_stays_at_full() {
        let mut l = Ladder::new(23.0);
        assert_eq!(feed(&mut l, 9.0, 3000), Level::Full);
    }

    #[test]
    fn a_slow_machine_steps_down_one_level_per_half_second_and_stops_at_the_bottom() {
        let mut l = Ladder::new(23.0);
        assert_eq!(feed(&mut l, 40.0, 14), Level::Full);
        assert_eq!(feed(&mut l, 40.0, 1), Level::SparseTracking);
        assert_eq!(feed(&mut l, 40.0, 15), Level::SparseModel);
        assert_eq!(feed(&mut l, 40.0, 15), Level::HalfRate);
        assert_eq!(feed(&mut l, 40.0, 500), Level::HalfRate);
    }

    #[test]
    fn one_slow_frame_does_not_step_down() {
        let mut l = Ladder::new(23.0);
        feed(&mut l, 10.0, 100);
        l.report(200.0);
        assert_eq!(feed(&mut l, 10.0, 100), Level::Full);
    }

    #[test]
    fn it_steps_back_up_only_after_a_calm_spell() {
        let mut l = Ladder::new(23.0);
        feed(&mut l, 40.0, 15);
        assert_eq!(l.level(), Level::SparseTracking);
        // comfortable, but not for long enough yet
        assert_eq!(
            feed(&mut l, 8.0, WINDOW + UP_WAIT as usize - 2),
            Level::SparseTracking
        );
        assert_eq!(feed(&mut l, 8.0, 1), Level::Full);
    }

    #[test]
    fn busy_but_within_the_limit_holds_the_level() {
        let mut l = Ladder::new(23.0);
        feed(&mut l, 40.0, 15);
        // under the limit but without room to spare: neither up nor down
        assert_eq!(feed(&mut l, 20.0, 2000), Level::SparseTracking);
    }

    #[test]
    fn a_step_up_that_fails_makes_the_next_try_wait_longer() {
        let mut l = Ladder::new(23.0);
        // A machine that manages level 1 easily (8 ms) but not level 0 (40 ms).
        let run = |l: &mut Ladder| {
            let mut frames = 0;
            while l.level() != Level::Full {
                l.report(8.0);
                frames += 1;
            }
            while l.level() == Level::Full {
                l.report(40.0);
            }
            frames
        };
        feed(&mut l, 40.0, 15);
        let first = run(&mut l);
        let second = run(&mut l);
        let third = run(&mut l);
        assert!(second >= first + UP_WAIT as usize, "{first} {second}");
        assert!(third > second, "{second} {third}");
    }
}
