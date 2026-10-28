// Adapted from openring-rs src/nullable.rs (github.com/lukehsiao/openring-rs @ 2352ab0), Copyright Luke Hsiao. BlueOak-1.0.0.
//! Shared parts of the nullable infrastructure wrappers. A wrapper emits
//! each write it makes through an [`OutputListener`], so a test can track
//! what went out. Its embedded stub answers from `ConfigurableResponses`.
//!
//! Only the crate's own unit tests can track output or build a stub. In
//! other builds an [`OutputListener`] holds nothing and emitting is free.

#[cfg(test)]
use std::{
    collections::VecDeque,
    sync::{Arc, PoisonError, Weak},
};
use std::{fmt, marker::PhantomData, sync::Mutex};

/// The writes one tracker has seen. The tracker owns it, and the listener
/// holds only a weak reference, so a dropped tracker stops collecting.
#[cfg(test)]
type Seen<T> = Mutex<Vec<T>>;

/// The write channel of a wrapper. The wrapper calls [`emit`] at each write,
/// in the code path that real and nulled instances share. Clones share
/// their trackers, so a tracker sees writes from every clone of a wrapper,
/// the way a real server sees requests from every clone of a client.
///
/// [`emit`]: OutputListener::emit
#[derive(Clone)]
pub struct OutputListener<T> {
    #[cfg(test)]
    trackers: Arc<Mutex<Vec<Weak<Seen<T>>>>>,
    // A Mutex marker gives every build the auto traits of the test-only
    // field, so a type that is Send in production is also Send in tests.
    _data: PhantomData<Mutex<T>>,
}

impl<T> Default for OutputListener<T> {
    fn default() -> Self {
        Self {
            #[cfg(test)]
            trackers: Arc::default(),
            _data: PhantomData,
        }
    }
}

impl<T> fmt::Debug for OutputListener<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutputListener").finish_non_exhaustive()
    }
}

impl<T: Clone> OutputListener<T> {
    /// Record one write. `data` runs only when a live tracker listens, so a
    /// write nobody tracks builds nothing.
    pub fn emit(&self, data: impl FnOnce() -> T) {
        #[cfg(test)]
        {
            let mut trackers = self.trackers.lock().unwrap_or_else(PoisonError::into_inner);
            let live: Vec<Arc<Seen<T>>> = trackers.iter().filter_map(Weak::upgrade).collect();
            // A dropped tracker can never read again, so forget it.
            trackers.retain(|tracker| tracker.strong_count() > 0);
            drop(trackers);
            if live.is_empty() {
                return;
            }
            let data = data();
            for tracker in live {
                tracker
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(data.clone());
            }
        }
        #[cfg(not(test))]
        let _ = (self, data);
    }

    /// Start recording writes. The tracker sees only writes made after this
    /// call, and stops recording when it is dropped.
    #[cfg(test)]
    #[must_use]
    pub fn track(&self) -> OutputTracker<T> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        self.trackers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::downgrade(&seen));
        OutputTracker { seen }
    }
}

/// The writes a wrapper made since a test started tracking it, in order.
#[cfg(test)]
pub struct OutputTracker<T> {
    seen: Arc<Seen<T>>,
}

#[cfg(test)]
impl<T> fmt::Debug for OutputTracker<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutputTracker").finish_non_exhaustive()
    }
}

#[cfg(test)]
impl<T: Clone> OutputTracker<T> {
    /// The writes seen so far, oldest first.
    #[must_use]
    pub fn data(&self) -> Vec<T> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
impl<T> OutputTracker<T> {
    /// Return the writes seen so far and forget them. The tracker keeps
    /// recording, so the next read sees only the writes made after this.
    pub fn clear(&self) -> Vec<T> {
        std::mem::take(&mut *self.seen.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Canned answers for an embedded stub. A single answer repeats forever. A
/// sequence is used up in order, and asking past its end panics with the
/// stub's name, so a test that asks too often fails instead of passing on
/// stale data.
#[cfg(test)]
#[derive(Debug)]
pub enum ConfigurableResponses<T> {
    Repeat(T),
    Sequence(VecDeque<T>),
}

#[cfg(test)]
impl<T: Clone> ConfigurableResponses<T> {
    /// The next answer. `name` identifies the stub in the panic message.
    ///
    /// # Panics
    ///
    /// Panics when a sequence has no answers left.
    pub fn next(&mut self, name: &str) -> T {
        match self {
            Self::Repeat(response) => response.clone(),
            Self::Sequence(responses) => responses
                .pop_front()
                .unwrap_or_else(|| panic!("No more responses configured for {name}")),
        }
    }
}

#[cfg(test)]
impl<T> ConfigurableResponses<T> {
    /// Translate every answer and keep repeat or sequence as it was. A
    /// wrapper uses this to turn its own configuration into the terms of
    /// the dependency it wraps.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> ConfigurableResponses<U> {
        match self {
            Self::Repeat(response) => ConfigurableResponses::Repeat({ f }(response)),
            Self::Sequence(responses) => {
                ConfigurableResponses::Sequence(responses.into_iter().map(f).collect())
            }
        }
    }
}

#[cfg(test)]
impl<T> From<T> for ConfigurableResponses<T> {
    fn from(response: T) -> Self {
        Self::Repeat(response)
    }
}

#[cfg(test)]
impl<T, const N: usize> From<[T; N]> for ConfigurableResponses<T> {
    fn from(responses: [T; N]) -> Self {
        Self::Sequence(responses.into())
    }
}

#[cfg(test)]
impl<T> From<Vec<T>> for ConfigurableResponses<T> {
    fn from(responses: Vec<T>) -> Self {
        Self::Sequence(responses.into())
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfigurableResponses, OutputListener};

    #[test]
    fn a_tracker_sees_writes_made_after_it_starts_from_any_clone() {
        let listener = OutputListener::default();
        listener.emit(|| "before");
        let tracker = listener.track();
        listener.emit(|| "first");
        listener.clone().emit(|| "second");
        assert_eq!(tracker.data(), ["first", "second"]);
    }

    #[test]
    fn emitting_without_a_tracker_builds_nothing() {
        let listener = OutputListener::<&str>::default();
        listener.emit(|| panic!("built data nobody tracks"));
    }

    #[test]
    fn a_dropped_tracker_stops_collecting() {
        let listener = OutputListener::<&str>::default();
        drop(listener.track());
        listener.emit(|| panic!("built data for a dropped tracker"));
    }

    #[test]
    fn clearing_returns_the_writes_and_keeps_recording() {
        let listener = OutputListener::default();
        let tracker = listener.track();
        listener.emit(|| "first");
        assert_eq!(tracker.clear(), ["first"]);
        listener.emit(|| "second");
        assert_eq!(tracker.data(), ["second"]);
    }

    #[test]
    fn a_single_response_repeats() {
        let mut responses = ConfigurableResponses::from(7);
        assert_eq!([responses.next("x"), responses.next("x")], [7, 7]);
    }

    #[test]
    #[should_panic(expected = "No more responses configured for the test stub")]
    fn a_sequence_is_used_up_in_order_then_panics() {
        let mut responses = ConfigurableResponses::<i32>::from(vec![1, 2]);
        assert_eq!([responses.next("x"), responses.next("x")], [1, 2]);
        responses.next("the test stub");
    }

    #[test]
    fn an_array_is_a_sequence() {
        let mut responses = ConfigurableResponses::<i32>::from([1, 2]);
        assert_eq!([responses.next("x"), responses.next("x")], [1, 2]);
    }

    #[test]
    fn mapping_keeps_repeat_and_sequence() {
        let mut repeat = ConfigurableResponses::from(2).map(|n| n * 10);
        assert_eq!([repeat.next("r"), repeat.next("r")], [20, 20]);
        let mut sequence = ConfigurableResponses::<i32>::from([1, 2]).map(|n| n * 10);
        assert_eq!([sequence.next("s"), sequence.next("s")], [10, 20]);
        assert!(matches!(sequence, ConfigurableResponses::Sequence(ref r) if r.is_empty()));
    }
}
