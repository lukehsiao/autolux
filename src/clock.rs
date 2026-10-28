//! Waiting. [`Clock::create`] really sleeps. The nulled clock returns at
//! once, so tests run hours of control loop in microseconds and assert on
//! the pacing through the sleeps they track.

use std::time::Duration;

use crate::nullable::OutputListener;
#[cfg(test)]
use crate::nullable::OutputTracker;

pub struct Clock {
    sleeper: Sleeper,
    sleeps: OutputListener<Duration>,
}

impl Clock {
    #[must_use]
    pub fn create() -> Self {
        Self::new(Sleeper::Real)
    }

    fn new(sleeper: Sleeper) -> Self {
        Self {
            sleeper,
            sleeps: OutputListener::default(),
        }
    }

    pub fn sleep(&self, duration: Duration) {
        self.sleeps.emit(|| duration);
        self.sleeper.sleep(duration);
    }
}

#[cfg(test)]
impl Clock {
    #[must_use]
    pub fn create_null() -> Self {
        Self::new(Sleeper::Stubbed)
    }

    #[must_use]
    pub fn track_sleeps(&self) -> OutputTracker<Duration> {
        self.sleeps.track()
    }
}

// The seam. The real arm only forwards to std::thread::sleep.
enum Sleeper {
    Real,
    #[cfg(test)]
    Stubbed,
}

impl Sleeper {
    fn sleep(&self, duration: Duration) {
        match self {
            Self::Real => std::thread::sleep(duration),
            #[cfg(test)]
            Self::Stubbed => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::Clock;

    #[test]
    fn real_clock_sleeps_at_least_as_long_as_asked() {
        let start = Instant::now();
        Clock::create().sleep(Duration::from_millis(20));
        assert!(start.elapsed() >= Duration::from_millis(20));
    }

    #[test]
    fn nulled_clock_returns_at_once_and_records_the_request() {
        let clock = Clock::create_null();
        let sleeps = clock.track_sleeps();
        let start = Instant::now();
        clock.sleep(Duration::from_secs(3600));
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(sleeps.data(), [Duration::from_secs(3600)]);
    }
}
