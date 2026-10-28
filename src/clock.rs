//! Time. [`Clock::create`] reads the monotonic clock and really sleeps. The
//! nulled clock keeps a virtual time that only sleeping moves, so tests run
//! hours of control loop in microseconds and assert on the pacing through
//! the sleeps they track.

#[cfg(test)]
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::nullable::OutputListener;
#[cfg(test)]
use crate::nullable::OutputTracker;

/// Clones share the time of a nulled clock and the sleep trackers.
#[derive(Clone)]
pub struct Clock {
    time: Time,
    sleeps: OutputListener<Duration>,
}

impl Clock {
    #[must_use]
    pub fn create() -> Self {
        Self::new(Time::Real)
    }

    fn new(time: Time) -> Self {
        Self {
            time,
            sleeps: OutputListener::default(),
        }
    }

    /// Monotonic time. It does not count time spent in suspend, because
    /// [`Instant`] is `CLOCK_MONOTONIC` on Linux.
    #[must_use]
    pub fn now(&self) -> Instant {
        self.time.now()
    }

    /// Sleeps until `deadline`, or returns at once if it has passed.
    pub fn sleep_until(&self, deadline: Instant) {
        self.sleeps
            .emit(|| deadline.saturating_duration_since(self.now()));
        self.time.sleep_until(deadline);
    }
}

#[cfg(test)]
impl Clock {
    #[must_use]
    pub fn create_null() -> Self {
        Self::new(Time::Stubbed(Arc::new(Mutex::new(Instant::now()))))
    }

    /// Tracks how long each sleep lasted.
    #[must_use]
    pub fn track_sleeps(&self) -> OutputTracker<Duration> {
        self.sleeps.track()
    }
}

// The seam. The real arm only forwards to std.
#[derive(Clone)]
enum Time {
    Real,
    #[cfg(test)]
    Stubbed(Arc<Mutex<Instant>>),
}

impl Time {
    fn now(&self) -> Instant {
        match self {
            Self::Real => Instant::now(),
            #[cfg(test)]
            Self::Stubbed(now) => *now.lock().unwrap(),
        }
    }

    fn sleep_until(&self, deadline: Instant) {
        match self {
            Self::Real => std::thread::sleep(deadline.saturating_duration_since(Instant::now())),
            #[cfg(test)]
            Self::Stubbed(now) => {
                let mut now = now.lock().unwrap();
                *now = (*now).max(deadline);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::Clock;

    #[test]
    fn real_clock_sleeps_until_the_deadline() {
        let clock = Clock::create();
        let deadline = clock.now() + Duration::from_millis(20);
        clock.sleep_until(deadline);
        assert!(Instant::now() >= deadline);
    }

    #[test]
    fn real_clock_returns_at_once_for_a_past_deadline() {
        let clock = Clock::create();
        let start = clock.now();
        clock.sleep_until(start.checked_sub(Duration::from_secs(1)).unwrap());
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn nulled_clock_returns_at_once_and_sleeping_advances_its_time() {
        let clock = Clock::create_null();
        let sleeps = clock.track_sleeps();
        let virtual_start = clock.now();
        let start = Instant::now();
        clock.sleep_until(virtual_start + Duration::from_secs(3600));
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(clock.now() - virtual_start, Duration::from_secs(3600));
        assert_eq!(sleeps.data(), [Duration::from_secs(3600)]);
    }

    #[test]
    fn nulled_clock_does_not_go_back_for_a_past_deadline() {
        let clock = Clock::create_null();
        let sleeps = clock.track_sleeps();
        let start = clock.now();
        clock.sleep_until(start + Duration::from_secs(5));
        clock.sleep_until(start + Duration::from_secs(2));
        assert_eq!(clock.now() - start, Duration::from_secs(5));
        assert_eq!(sleeps.data(), [Duration::from_secs(5), Duration::ZERO]);
    }

    #[test]
    fn nulled_clock_stands_still_until_time_passes() {
        let clock = Clock::create_null();
        let start = clock.now();
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(clock.now(), start);
    }

    #[test]
    fn clones_of_a_nulled_clock_share_its_time() {
        let clock = Clock::create_null();
        let other = clock.clone();
        let start = clock.now();
        other.sleep_until(start + Duration::from_millis(2));
        assert_eq!(clock.now() - start, Duration::from_millis(2));
    }
}
