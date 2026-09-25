//! Waiting. [`Clock::create`] really sleeps; [`Clock::create_null`] returns
//! at once, so tests run hours of control loop in microseconds and assert on
//! the pacing through [`Clock::track_sleeps`].

use std::time::Duration;

use crate::nullable::{OutputListener, OutputTracker};

pub struct Clock {
    sleep: fn(Duration),
    sleeps: OutputListener<Duration>,
}

impl Clock {
    #[must_use]
    pub fn create() -> Self {
        Self::new(std::thread::sleep)
    }

    #[must_use]
    pub fn create_null() -> Self {
        Self::new(|_| {})
    }

    fn new(sleep: fn(Duration)) -> Self {
        Self {
            sleep,
            sleeps: OutputListener::new(),
        }
    }

    pub fn sleep(&self, duration: Duration) {
        self.sleeps.emit(&duration);
        (self.sleep)(duration);
    }

    #[must_use]
    pub fn track_sleeps(&self) -> OutputTracker<Duration> {
        self.sleeps.track()
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
