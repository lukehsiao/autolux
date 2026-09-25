//! The two utilities that make the infrastructure wrappers observable and
//! configurable when nulled.
//!
//! [`OutputListener`] carries the write channel: a wrapper emits the domain
//! data of every write it performs, and tests assert on what an
//! [`OutputTracker`] recorded instead of spying on method calls. Nothing is
//! recorded until something tracks, so production pays one check per write.
//!
//! [`Responses`] carries the read channel of an embedded stub: a single
//! value repeats forever, a sequence is consumed in order and then fails
//! loudly, so a test that reads more than it configured cannot pass on
//! stale data.

use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::{Rc, Weak},
};

pub struct OutputListener<T> {
    trackers: RefCell<Vec<Weak<RefCell<Vec<T>>>>>,
}

impl<T: Clone> OutputListener<T> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            trackers: RefCell::new(Vec::new()),
        }
    }

    /// Records `data` in every live tracker, in emission order.
    pub fn emit(&self, data: &T) {
        let mut trackers = self.trackers.borrow_mut();
        // Dropped trackers are pruned here rather than on drop, so a tracker
        // needs no back-reference to its listener.
        trackers.retain(|tracker| tracker.strong_count() > 0);
        for tracker in trackers.iter().filter_map(Weak::upgrade) {
            tracker.borrow_mut().push(data.clone());
        }
    }

    /// Starts recording; the tracker sees only emissions after this call.
    #[must_use]
    pub fn track(&self) -> OutputTracker<T> {
        let store = Rc::new(RefCell::new(Vec::new()));
        self.trackers.borrow_mut().push(Rc::downgrade(&store));
        OutputTracker { store }
    }
}

impl<T: Clone> Default for OutputListener<T> {
    fn default() -> Self {
        Self::new()
    }
}

pub struct OutputTracker<T> {
    store: Rc<RefCell<Vec<T>>>,
}

impl<T: Clone> OutputTracker<T> {
    /// Everything recorded so far, oldest first.
    #[must_use]
    pub fn data(&self) -> Vec<T> {
        self.store.borrow().clone()
    }

    /// Returns everything recorded so far and forgets it, for tests with
    /// several phases.
    #[must_use]
    pub fn clear(&self) -> Vec<T> {
        std::mem::take(&mut *self.store.borrow_mut())
    }
}

/// What an embedded stub answers on successive reads.
#[derive(Debug, Clone, PartialEq)]
pub enum Responses<T> {
    /// The same answer every time, forever.
    Always(T),
    /// One answer per read, in order; reading past the end panics.
    Sequence(VecDeque<T>),
}

impl<T: Clone> Responses<T> {
    /// The next answer. `name` identifies the stub in the exhaustion panic.
    ///
    /// # Panics
    /// When a sequence has run out: the test read more than it configured.
    pub fn next(&mut self, name: &str) -> T {
        match self {
            Self::Always(response) => response.clone(),
            Self::Sequence(responses) => responses
                .pop_front()
                .unwrap_or_else(|| panic!("No more responses configured for {name}")),
        }
    }

    /// Translates every answer, for wrappers that decompose their
    /// configuration into their dependency's terms.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> Responses<U> {
        match self {
            Self::Always(response) => Responses::Always({ f }(response)),
            Self::Sequence(responses) => {
                Responses::Sequence(responses.into_iter().map(f).collect())
            }
        }
    }
}

impl<T> From<T> for Responses<T> {
    fn from(response: T) -> Self {
        Self::Always(response)
    }
}

impl<T, const N: usize> From<[T; N]> for Responses<T> {
    fn from(responses: [T; N]) -> Self {
        Self::Sequence(responses.into())
    }
}

impl<T> From<Vec<T>> for Responses<T> {
    fn from(responses: Vec<T>) -> Self {
        Self::Sequence(responses.into())
    }
}

#[cfg(test)]
mod tests {
    use super::{OutputListener, Responses};

    #[test]
    fn tracks_emissions_in_order_from_the_moment_tracking_starts() {
        let listener = OutputListener::new();
        listener.emit(&1);
        let tracker = listener.track();
        listener.emit(&2);
        listener.emit(&3);
        assert_eq!(tracker.data(), [2, 3]);
    }

    #[test]
    fn clear_returns_and_forgets() {
        let listener = OutputListener::new();
        let tracker = listener.track();
        listener.emit(&"a");
        assert_eq!(tracker.clear(), ["a"]);
        listener.emit(&"b");
        assert_eq!(tracker.data(), ["b"]);
    }

    #[test]
    fn dropped_trackers_stop_recording_without_affecting_others() {
        let listener = OutputListener::new();
        let dropped = listener.track();
        let kept = listener.track();
        drop(dropped);
        listener.emit(&7);
        assert_eq!(kept.data(), [7]);
    }

    #[test]
    fn single_response_repeats_forever() {
        let mut responses = Responses::<i32>::from(4);
        for _ in 0..3 {
            assert_eq!(responses.next("test"), 4);
        }
    }

    #[test]
    fn sequence_is_consumed_in_order() {
        let mut responses = Responses::<i32>::from([1, 2]);
        assert_eq!(responses.next("test"), 1);
        assert_eq!(responses.next("test"), 2);
    }

    #[test]
    #[should_panic(expected = "No more responses configured for the stub")]
    fn exhausted_sequence_fails_loudly() {
        let mut responses = Responses::<i32>::from([1]);
        responses.next("the stub");
        responses.next("the stub");
    }

    #[test]
    fn map_preserves_repetition_semantics() {
        let mut always = Responses::<i32>::from(2).map(|n| n * 10);
        assert_eq!([always.next("a"), always.next("a")], [20, 20]);
        let mut sequence = Responses::<i32>::from([1, 2]).map(|n| n * 10);
        assert_eq!([sequence.next("s"), sequence.next("s")], [10, 20]);
    }
}
