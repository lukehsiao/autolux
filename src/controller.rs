//! Every control decision, without I/O: when to sample the light, when to
//! wake, reading the panel on a wake, fading frame by frame, which frames
//! to write, and how long the sensor may stay unreadable.

mod smoother;

use std::{
    fmt,
    time::{Duration, Instant},
};

use tracing::{debug, info, warn};

use crate::curve::Curve;
use smoother::{FRAME, Frame, Smoother};

/// How often to read the light sensor while the panel is settled.
///
/// Reading a HID ambient light sensor is a synchronous round trip to the
/// sensor hub, about 10 ms and 200 µs of CPU on a Surface Go 3, which makes
/// it nearly all of what an idle autolux costs. Once a second keeps that
/// around 0.02% of one core, and bounds how long a light switch goes unseen.
pub(crate) const SETTLED_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// How often to read the light sensor while fading, so the target follows
/// changing light closely instead of in once-a-second jumps.
const FADING_SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// How long the sensor may stay unreadable before autolux exits. This is
/// measured time, which does not count suspend.
///
/// Ten seconds rides out a sensor hub resuming from suspend. Beyond that the
/// device has probably been renumbered, and a restart re-detects it.
pub(crate) const MAX_SENSOR_OUTAGE: Duration = Duration::from_secs(10);

/// What happened since the last step. Every variant is valid at any time.
#[derive(Debug)]
pub enum Input<E> {
    /// Only time passed, as after a sleep or a write.
    Time,
    /// A sample taken now. An error means the sensor was unreadable.
    Sample(Result<f64, E>),
    /// The panel's raw brightness, read now.
    Panel(u32),
}

/// The one thing to do next.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// Take a sample and pass it in as [`Input::Sample`].
    Sample,
    /// Read the panel and pass it in as [`Input::Panel`].
    ReadPanel,
    /// Set the panel to this raw brightness.
    Write(u32),
    /// Nothing is due before this time.
    SleepUntil(Instant),
}

/// Why the controller cannot go on.
#[derive(Debug)]
pub enum Fatal<E> {
    /// The first sample was unreadable.
    Startup(E),
    /// No sample has been readable for `outage`.
    Outage { outage: Duration, source: E },
}

pub struct Controller {
    state: State,
    /// The latest `now` seen, so time never runs backward.
    now: Option<Instant>,
}

#[expect(
    clippy::large_enum_variant,
    reason = "there is one controller, and a box would cost a pointer chase every step"
)]
enum State {
    /// Waiting for the first readable sample and one panel reading.
    Starting {
        curve: Curve,
        /// The illuminance of the first readable sample, and when it was
        /// taken.
        sample: Option<(f64, Instant)>,
        panel: Option<u32>,
    },
    Running(Running),
}

struct Running {
    smoother: Smoother,
    /// The illuminance of the latest readable sample, for logs.
    illuminance: f64,
    /// When the latest readable sample was taken.
    good_sample: Instant,
    next_sample: Instant,
    /// A settled panel must start a fade, once it has been read.
    wake: bool,
    /// The deadline of the first frame not yet run. Frames sit on a grid
    /// anchored at the wake, so time spent writing shortens the next sleep
    /// instead of adding to it.
    next_frame: Instant,
    /// How far the light filter of a settled panel has caught up.
    integrated: Instant,
    /// The frames ahead, worked out once and kept until an input changes
    /// them.
    plan: Option<Plan>,
}

/// Frames run ahead of time on a copy of the smoother. Only a sample or a
/// panel reading can change what a frame does, so frames before the next
/// sample can be run early.
#[derive(Debug, Clone, Copy)]
struct Plan {
    /// After the frames before `next`, none of which writes.
    quiet: Smoother,
    /// The deadline of the last frame in `quiet`, if there is one.
    last: Option<Instant>,
    /// The deadline of the first frame not in `quiet`.
    next: Instant,
    /// When the frame at `next` writes: the smoother after it, and the raw
    /// brightness to write.
    write: Option<(Smoother, u32)>,
}

impl Controller {
    /// A controller that has seen nothing yet. The first step asks for a
    /// sample.
    #[must_use]
    pub fn new(curve: Curve) -> Self {
        Self {
            state: State::Starting {
                curve,
                sample: None,
                panel: None,
            },
            now: None,
        }
    }

    /// Takes in what happened and says what to do next.
    ///
    /// An unasked sample counts as a sample. An unasked panel reading is a
    /// manual adjustment: a fade continues from it, and a settled panel is
    /// left there until the next fade. A request that gets no answer is
    /// made again. A `now` earlier than the one before counts as equal to
    /// it.
    ///
    /// # Errors
    /// [`Fatal::Startup`] when the first sample is unreadable, and
    /// [`Fatal::Outage`] when no sample has been readable for
    /// [`MAX_SENSOR_OUTAGE`].
    pub fn step<E: std::error::Error>(
        &mut self,
        now: Instant,
        input: Input<E>,
    ) -> Result<Output, Fatal<E>> {
        let now = self.now.map_or(now, |last| last.max(now));
        self.now = Some(now);
        match &mut self.state {
            State::Starting {
                curve,
                sample,
                panel,
            } => {
                match input {
                    Input::Time => {}
                    Input::Sample(Ok(lux)) => *sample = Some((lux, now)),
                    Input::Sample(Err(err)) => match *sample {
                        None => return Err(Fatal::Startup(err)),
                        Some((_, good)) => skip_sample(now, good, err)?,
                    },
                    Input::Panel(raw) => *panel = Some(raw),
                }
                let (Some((lux, good)), Some(raw)) = (*sample, *panel) else {
                    return Ok(if sample.is_none() {
                        Output::Sample
                    } else {
                        Output::ReadPanel
                    });
                };
                let running = Running::start(*curve, lux, good, raw, now);
                self.state = State::Running(running);
            }
            State::Running(running) => {
                running.catch_up(now);
                running.absorb(now, input)?;
            }
        }
        let State::Running(running) = &mut self.state else {
            unreachable!("startup ends in the running state");
        };
        Ok(running.decide(now))
    }
}

impl Running {
    fn start(curve: Curve, lux: f64, good_sample: Instant, panel: u32, now: Instant) -> Self {
        info!("{lux:.1} lux, panel at {}%", curve.percent(panel));
        let mut running = Self {
            smoother: Smoother::new(curve, lux, panel),
            illuminance: lux,
            good_sample,
            next_sample: now + SETTLED_SAMPLE_INTERVAL,
            wake: false,
            next_frame: now,
            integrated: now,
            plan: None,
        };
        // The startup reading of the panel is fresh, so a wake needs no
        // second one.
        if running.smoother.should_wake() {
            running.begin_fade(now, panel);
        }
        running
    }

    fn begin_fade(&mut self, now: Instant, panel: u32) {
        self.smoother.wake(panel);
        let curve = self.smoother.curve();
        info!(
            "{:.1} lux: fading from {}% to {}%",
            self.illuminance,
            curve.percent(panel),
            curve.percent(self.smoother.target()),
        );
        self.wake = false;
        self.plan = None;
        self.next_frame = now + FRAME;
        self.next_sample = self.next_sample.min(now + FADING_SAMPLE_INTERVAL);
    }

    /// Runs the frames due by `now` that write nothing, with what was known
    /// before this step's input. A frame that writes waits for
    /// [`Running::decide`].
    fn catch_up(&mut self, now: Instant) {
        if !self.smoother.is_fading() {
            return;
        }
        let plan = match self.plan {
            // Every quiet frame of the plan is due, so it can be kept.
            Some(plan) if plan.last.is_none_or(|last| last <= now) => plan,
            _ => {
                self.plan = None;
                self.look_ahead(now)
            }
        };
        self.smoother = plan.quiet;
        self.next_frame = plan.next;
        if !self.smoother.is_fading()
            && let Some(landed) = plan.last
        {
            self.integrated = landed;
        }
    }

    fn absorb<E: std::error::Error>(
        &mut self,
        now: Instant,
        input: Input<E>,
    ) -> Result<(), Fatal<E>> {
        match input {
            Input::Time => {}
            Input::Sample(Ok(lux)) => {
                self.illuminance = lux;
                self.good_sample = now;
                self.smoother.observe(lux);
                if !self.smoother.is_fading() {
                    self.smoother
                        .integrate(now.saturating_duration_since(self.integrated));
                    self.integrated = now;
                    self.wake |= self.smoother.should_wake();
                }
                self.plan = None;
                self.reschedule(now);
            }
            Input::Sample(Err(err)) => {
                skip_sample(now, self.good_sample, err)?;
                self.reschedule(now);
            }
            Input::Panel(raw) if self.wake => self.begin_fade(now, raw),
            Input::Panel(raw) => {
                self.smoother.adjust(raw);
                self.plan = None;
            }
        }
        Ok(())
    }

    /// Moves the next sample on, if this one was due. A plan stops at the
    /// next sample, so it no longer reaches far enough.
    fn reschedule(&mut self, now: Instant) {
        if now < self.next_sample {
            return;
        }
        let interval = if self.smoother.is_fading() || self.wake {
            FADING_SAMPLE_INTERVAL
        } else {
            SETTLED_SAMPLE_INTERVAL
        };
        self.next_sample = next_deadline(self.next_sample, interval, now);
        self.plan = None;
    }

    fn decide(&mut self, now: Instant) -> Output {
        if now >= self.next_sample {
            return Output::Sample;
        }
        if self.wake {
            return Output::ReadPanel;
        }
        if !self.smoother.is_fading() {
            return Output::SleepUntil(self.next_sample);
        }
        let plan = match self.plan {
            Some(plan) => plan,
            None => *self.plan.insert(self.look_ahead(self.next_sample)),
        };
        match plan.write {
            Some((after, raw)) if plan.next <= now => {
                self.smoother = after;
                self.next_frame = next_deadline(plan.next, FRAME, now);
                if !after.is_fading() {
                    self.integrated = plan.next;
                }
                self.plan = None;
                debug!(raw, "set");
                Output::Write(raw)
            }
            Some(_) => Output::SleepUntil(plan.next),
            None => Output::SleepUntil(self.next_sample),
        }
    }

    /// Runs frames on a copy of the smoother, from the next one up to the
    /// first that writes, the end of the fade, `until` or the next sample,
    /// whichever comes first. Through [`writes`], this is the only place
    /// that decides which frames are written.
    fn look_ahead(&self, until: Instant) -> Plan {
        let mut quiet = self.smoother;
        let mut last = None;
        let mut next = self.next_frame;
        while quiet.is_fading() && next <= until && next < self.next_sample {
            let mut after = quiet;
            let frame = after.frame();
            if writes(&quiet, frame) {
                after.wrote(frame.raw);
                return Plan {
                    quiet,
                    last,
                    next,
                    write: Some((after, frame.raw)),
                };
            }
            quiet = after;
            last = Some(next);
            next += FRAME;
        }
        Plan {
            quiet,
            last,
            next,
            write: None,
        }
    }
}

/// The write quantum: the smallest change in lightness a fade writes.
///
/// Each write costs logind a D-Bus round trip, so a fade skips the frames
/// that move the panel less than this. A fifth of the deadband is still far
/// below one just-noticeable difference, so the steps stay invisible.
const WRITE_QUANTUM: f64 = 0.002;

/// The write policy: whether `frame`, run on `before`, changes the panel.
///
/// A frame that lands writes the target exactly, if the panel is not there
/// yet. Any other frame writes only once the panel has moved at least
/// [`WRITE_QUANTUM`] in lightness since the last write.
fn writes(before: &Smoother, frame: Frame) -> bool {
    let written = before.written();
    if frame.landed {
        return frame.raw != written;
    }
    let curve = before.curve();
    (curve.perceived(frame.raw) - curve.perceived(written)).abs() >= WRITE_QUANTUM
}

/// Logs an unreadable sample, unless the sensor has been unreadable since
/// `good` for too long.
fn skip_sample<E: std::error::Error>(now: Instant, good: Instant, err: E) -> Result<(), Fatal<E>> {
    let outage = now.saturating_duration_since(good);
    if outage >= MAX_SENSOR_OUTAGE {
        return Err(Fatal::Outage {
            outage,
            source: err,
        });
    }
    warn!("skipping a light sample: {}", Chain(&err));
    Ok(())
}

/// The deadline one `interval` after `deadline`. If that has passed too,
/// as after a stall, the missed deadlines are skipped and the next one is
/// one `interval` after `now`.
fn next_deadline(deadline: Instant, interval: Duration, now: Instant) -> Instant {
    let next = deadline + interval;
    if next > now { next } else { now + interval }
}

/// An error and its causes, joined, for one-line logs.
struct Chain<'a>(&'a dyn std::error::Error);

impl fmt::Display for Chain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)?;
        for cause in std::iter::successors(self.0.source(), |e| e.source()) {
            write!(f, ": {cause}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        num::NonZeroU32,
        time::{Duration, Instant},
    };

    use hegel::{TestCase, generators};

    use super::{
        Controller, FRAME, Fatal, Input, MAX_SENSOR_OUTAGE, Output, SETTLED_SAMPLE_INTERVAL,
        WRITE_QUANTUM,
        smoother::{DEADBAND, TAU_FADE},
    };
    use crate::curve::{
        Curve, Light, Limits,
        tests::{any_lux, curves},
    };

    /// Longest a fade across the whole range may take. The constants give
    /// just under five seconds; the rest is margin for float rounding.
    const LONGEST_FADE: Duration = Duration::from_secs(6);

    #[derive(Debug, thiserror::Error)]
    #[error("unreadable")]
    struct Unreadable;

    /// Something the controller saw or did, as the world records it.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Seen {
        /// A sample it asked for. `None` was unreadable.
        Sample(Option<f64>),
        /// A sample it did not ask for.
        Unasked(Option<f64>),
        /// A panel reading it asked for.
        Read(u32),
        /// A panel reading it did not ask for.
        Report(u32),
        Write(u32),
    }

    /// Something that happens to the world at a given time.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Event {
        /// The illuminance from now on. `None` makes the sensor unreadable.
        Light(Option<f64>),
        /// Someone sets the panel by hand, and the controller is not told.
        ManualAdjustment(u32),
        /// The controller gets a sample it did not ask for.
        Sample,
        /// The controller gets a panel reading it did not ask for.
        Report,
    }

    /// A pure in-memory room and panel.
    struct World {
        curve: Curve,
        /// The panel's raw brightness at the start.
        panel: u32,
        /// The illuminance at the start.
        lux: Option<f64>,
        /// In time order.
        events: Vec<(Duration, Event)>,
        /// How long each write takes, during which time passes.
        latency: Duration,
    }

    impl World {
        fn new(curve: Curve, panel: u32, lux: f64) -> Self {
            Self {
                curve,
                panel,
                lux: Some(lux),
                events: Vec::new(),
                latency: Duration::ZERO,
            }
        }
    }

    struct Run {
        seen: Vec<(Duration, Seen)>,
        fatal: Option<Fatal<Unreadable>>,
        /// The panel's raw brightness at the end.
        panel: u32,
    }

    impl Run {
        fn writes(&self) -> Vec<u32> {
            self.seen
                .iter()
                .filter_map(|(_, seen)| match seen {
                    Seen::Write(raw) => Some(*raw),
                    _ => None,
                })
                .collect()
        }
    }

    /// Runs a controller in `world` until it sleeps past `horizon` or fails.
    fn simulate(world: &World, horizon: Duration) -> Run {
        let t0 = Instant::now();
        let mut controller = Controller::new(world.curve);
        let mut panel = world.panel;
        let mut lux = world.lux;
        let mut events = world.events.iter().peekable();
        let mut seen = Vec::new();
        let mut at = Duration::ZERO;
        let mut input = Input::Time;
        // A controller that never sleeps would hang the test. Ten million
        // steps is far more than any horizon here needs.
        for _ in 0..10_000_000 {
            let output = match controller.step(t0 + at, input) {
                Ok(output) => output,
                Err(fatal) => {
                    return Run {
                        seen,
                        fatal: Some(fatal),
                        panel,
                    };
                }
            };
            input = match output {
                Output::Sample => {
                    seen.push((at, Seen::Sample(lux)));
                    Input::Sample(lux.ok_or(Unreadable))
                }
                Output::ReadPanel => {
                    seen.push((at, Seen::Read(panel)));
                    Input::Panel(panel)
                }
                Output::Write(raw) => {
                    panel = raw;
                    seen.push((at, Seen::Write(raw)));
                    at += world.latency;
                    Input::Time
                }
                Output::SleepUntil(deadline) => {
                    let wake = deadline.saturating_duration_since(t0).max(at);
                    match events.next_if(|(when, _)| *when <= wake) {
                        Some(&(when, event)) => {
                            at = at.max(when);
                            match event {
                                Event::Light(new) => {
                                    lux = new;
                                    Input::Time
                                }
                                Event::ManualAdjustment(raw) => {
                                    panel = raw;
                                    Input::Time
                                }
                                Event::Sample => {
                                    seen.push((at, Seen::Unasked(lux)));
                                    Input::Sample(lux.ok_or(Unreadable))
                                }
                                Event::Report => {
                                    seen.push((at, Seen::Report(panel)));
                                    Input::Panel(panel)
                                }
                            }
                        }
                        None if wake > horizon => {
                            return Run {
                                seen,
                                fatal: None,
                                panel,
                            };
                        }
                        None => {
                            at = wake;
                            Input::Time
                        }
                    }
                }
            };
        }
        panic!("the controller never slept past {horizon:?}");
    }

    #[hegel::composite]
    fn scenarios(tc: &TestCase) -> (Curve, u32, f64) {
        let curve = tc.draw(curves());
        let panel_max = curve.raw(1.0);
        let start = tc.draw(generators::integers::<u32>().max_value(panel_max));
        let lux = tc.draw(any_lux());
        (curve, start, lux)
    }

    /// Unasked samples and panel readings at random times before `until`.
    /// In a world without nudges they change nothing the controller sees.
    fn interruptions(tc: &TestCase, until: Duration) -> Vec<(Duration, Event)> {
        let count = tc.draw(generators::integers::<usize>().max_value(20));
        let limit = u64::try_from(until.as_millis()).unwrap();
        let mut events: Vec<_> = (0..count)
            .map(|_| {
                let at = Duration::from_millis(tc.draw(generators::integers().max_value(limit)));
                let event = if tc.draw(generators::booleans()) {
                    Event::Sample
                } else {
                    Event::Report
                };
                (at, event)
            })
            .collect();
        events.sort_by_key(|(at, _)| *at);
        events
    }

    /// A world in steady light, interrupted at random during the fade.
    fn steady(tc: &TestCase) -> (World, f64) {
        let (curve, start, lux) = tc.draw(scenarios());
        let mut world = World::new(curve, start, lux);
        world.events = interruptions(tc, LONGEST_FADE);
        (world, lux)
    }

    // In steady light, the panel always settles, quickly, and a fade that
    // starts lands exactly on the curve's target. A panel that never woke
    // was already within the deadband. Once settled, the controller neither
    // writes nor reads the panel.
    #[hegel::test]
    fn settles_on_target_in_steady_light(tc: TestCase) {
        let (world, lux) = steady(&tc);
        let (curve, start) = (world.curve, world.panel);
        let horizon = LONGEST_FADE + Duration::from_secs(2);
        let run = simulate(&world, horizon);
        assert!(run.fatal.is_none());
        let target = curve.target(Light::from_lux(lux));
        match run.writes().last() {
            Some(last) => assert_eq!(*last, target),
            None => assert!(
                (curve.perceived(start) - curve.perceived(target)).abs() < DEADBAND,
                "never moved from {start} although the target is {target}"
            ),
        }
        assert!(
            run.seen
                .iter()
                .all(|(at, seen)| *at <= LONGEST_FADE
                    || !matches!(seen, Seen::Write(_) | Seen::Read(_))),
            "still busy after {LONGEST_FADE:?}: {:?}",
            run.seen
        );
    }

    // The configured limits are hard bounds: in steady light, the panel
    // always ends up inside them, even when it starts outside by less than
    // the deadband.
    #[hegel::test]
    fn always_settles_within_the_limits(tc: TestCase) {
        let (world, _) = steady(&tc);
        let (curve, start) = (world.curve, world.panel);
        let run = simulate(&world, LONGEST_FADE);
        let floor = curve.target(Light::from_lux(0.0));
        let ceiling = curve.target(Light::from_lux(f64::INFINITY));
        assert!(
            (floor..=ceiling).contains(&run.panel),
            "started at {start}, settled at {}, outside {floor}..={ceiling}",
            run.panel
        );
    }

    /// A Surface Go 3 panel with limits of 2% and 75%.
    fn surface_go() -> Curve {
        Curve::new(Limits::new(2, 75).unwrap(), NonZeroU32::new(7500).unwrap())
    }

    // 5700 of 7500 is 76%, just past a 75% ceiling and only 0.004 from it
    // in lightness, well inside the deadband. It must still come down to
    // exactly the ceiling, 5625.
    #[test]
    fn a_panel_just_above_the_ceiling_comes_down_to_it() {
        let run = simulate(&World::new(surface_go(), 5700, 2_000.0), LONGEST_FADE);
        assert_eq!(run.writes().last(), Some(&5625));
    }

    // With min == max there is exactly one right brightness, 50% of 7500,
    // and a panel a hair off it must still land on it.
    #[test]
    fn a_fixed_brightness_is_reached_exactly() {
        let curve = Curve::new(Limits::new(50, 50).unwrap(), NonZeroU32::new(7500).unwrap());
        let run = simulate(&World::new(curve, 3760, 123.0), LONGEST_FADE);
        assert_eq!(run.writes().last(), Some(&3750));
    }

    // In steady light, a fade heads straight for the target: every write is
    // between the start and the target, and each is closer than the last.
    #[hegel::test]
    fn fades_monotonically_without_overshoot(tc: TestCase) {
        let (world, lux) = steady(&tc);
        let (curve, start) = (world.curve, world.panel);
        let run = simulate(&world, LONGEST_FADE);
        let target = curve.target(Light::from_lux(lux));
        let mut previous = start;
        for raw in run.writes() {
            let (lo, hi) = (previous.min(target), previous.max(target));
            assert!(
                (lo..=hi).contains(&raw) && raw.abs_diff(target) < previous.abs_diff(target),
                "{start} to {target}: {previous} then {raw}"
            );
            previous = raw;
        }
    }

    // No write visibly jumps: between consecutive writes the panel moves by
    // at most the write quantum plus one frame's worth of easing across the
    // full scale, plus the width of one raw step, which is all rounding can
    // add. Writes are at least a frame apart, however long each one takes.
    #[hegel::test]
    fn each_write_moves_the_panel_a_little(tc: TestCase) {
        let (mut world, _) = steady(&tc);
        // Up to three frames, in microseconds.
        world.latency = Duration::from_micros(tc.draw(generators::integers().max_value(50_000)));
        let (curve, start) = (world.curve, world.panel);
        let run = simulate(&world, Duration::from_secs(30));
        let bound = WRITE_QUANTUM + one_frame_of_easing();
        let writes: Vec<_> = run
            .seen
            .iter()
            .filter_map(|(at, seen)| match seen {
                Seen::Write(raw) => Some((*at, *raw)),
                _ => None,
            })
            .collect();
        for pair in std::iter::once((Duration::ZERO, start))
            .chain(writes.iter().copied())
            .collect::<Vec<_>>()
            .windows(2)
        {
            assert_moved_at_most(curve, pair[0].1, pair[1].1, bound);
        }
        for pair in writes.windows(2) {
            assert!(pair[1].0.abs_diff(pair[0].0) >= FRAME, "{pair:?}");
        }
    }

    // A fade writes only once the panel has moved by at least the write
    // quantum since the last write. In steady light the target is fixed, so
    // only the last write lands, and only it may move less.
    #[hegel::test]
    fn writes_are_at_least_a_quantum_apart(tc: TestCase) {
        let (world, _) = steady(&tc);
        let (curve, start) = (world.curve, world.panel);
        let run = simulate(&world, LONGEST_FADE);
        let writes = run.writes();
        let Some((_, before_landing)) = writes.split_last() else {
            return;
        };
        let mut previous = start;
        for &raw in before_landing {
            let moved = (curve.perceived(raw) - curve.perceived(previous)).abs();
            assert!(
                moved >= WRITE_QUANTUM,
                "{previous} to {raw} moved {moved}, less than {WRITE_QUANTUM}"
            );
            previous = raw;
        }
    }

    // A settled panel is not woken by light whose target is within the
    // deadband, however long it keeps arriving.
    #[hegel::test]
    fn small_changes_do_not_wake_a_settled_panel(tc: TestCase) {
        let curve = tc.draw(curves());
        let lux = tc.draw(any_lux());
        let target = curve.target(Light::from_lux(lux));
        let nearby = tc.draw(any_lux());
        let nearby_target = curve.target(Light::from_lux(nearby));
        tc.assume((curve.perceived(nearby_target) - curve.perceived(target)).abs() < DEADBAND);
        let minute = Duration::from_secs(60);
        let mut world = World::new(curve, target, lux);
        world.events = interruptions(&tc, minute);
        let change = Duration::from_millis(tc.draw(generators::integers().max_value(2_000)));
        world.events.push((change, Event::Light(Some(nearby))));
        world.events.sort_by_key(|(at, _)| *at);
        let run = simulate(&world, minute);
        let busy: Vec<_> = run
            .seen
            .iter()
            .filter(|(_, seen)| matches!(seen, Seen::Read(_) | Seen::Write(_)))
            .collect();
        assert_eq!(busy, [&(Duration::ZERO, Seen::Read(target))]);
    }

    // A panel visibly off target at startup starts fading at once: the
    // startup reading serves as the wake's reading of the panel.
    #[hegel::test]
    fn a_visibly_wrong_panel_fades_from_startup(tc: TestCase) {
        let (world, lux) = steady(&tc);
        let (curve, start) = (world.curve, world.panel);
        let target = curve.target(Light::from_lux(lux));
        tc.assume((curve.perceived(start) - curve.perceived(target)).abs() >= DEADBAND);
        let run = simulate(&world, LONGEST_FADE);
        // Compare the sample by position only: NaN, a valid illuminance
        // here, never equals itself.
        assert!(
            matches!(
                run.seen[..2],
                [
                    (Duration::ZERO, Seen::Sample(Some(_))),
                    (Duration::ZERO, Seen::Read(read)),
                ] if read == start
            ),
            "{:?}",
            run.seen
        );
        let reads = run
            .seen
            .iter()
            .filter(|(_, seen)| matches!(seen, Seen::Read(_)))
            .count();
        assert_eq!(reads, 1, "{:?}", run.seen);
        assert_eq!(run.writes().last(), Some(&target));
    }

    // A settled panel wakes on the very sample that takes its target out of
    // the deadband, and reads the panel before anything else.
    #[hegel::test]
    fn visible_changes_wake_on_the_sample_that_shows_them(tc: TestCase) {
        let curve = tc.draw(curves());
        let before = tc.draw(any_lux());
        let after = tc.draw(any_lux());
        let (from, to) = (
            curve.target(Light::from_lux(before)),
            curve.target(Light::from_lux(after)),
        );
        // Twice the deadband, so the light filter gets there in seconds.
        tc.assume((curve.perceived(from) - curve.perceived(to)).abs() >= 2.0 * DEADBAND);
        let change = Duration::from_millis(tc.draw(generators::integers().max_value(5_000)));
        let mut world = World::new(curve, from, before);
        world.events = interruptions(&tc, change + Duration::from_secs(10));
        world.events.push((change, Event::Light(Some(after))));
        world.events.sort_by_key(|(at, _)| *at);
        // The light filter covers 99% of a change in 10 s.
        let run = simulate(&world, change + Duration::from_secs(11));
        let wake = run
            .seen
            .iter()
            .skip(2)
            .position(|(_, seen)| matches!(seen, Seen::Read(_)))
            .map(|i| i + 2)
            .expect("never woke");
        assert!(
            matches!(
                run.seen[wake - 1].1,
                Seen::Sample(Some(lux)) | Seen::Unasked(Some(lux)) if lux.total_cmp(&after).is_eq()
            ),
            "{:?}",
            &run.seen[..=wake]
        );
    }

    // Under any light, manual adjustments and unasked inputs, every value
    // written lies between the panel's previous brightness, as last written
    // or read, and the targets of all samples so far: the panel never
    // overshoots, even while the target moves.
    #[hegel::test]
    fn never_overshoots_a_moving_target(tc: TestCase) {
        let (curve, start, lux) = tc.draw(scenarios());
        let panel_max = curve.raw(1.0);
        let mut world = World::new(curve, start, lux);
        let count = tc.draw(generators::integers::<usize>().max_value(50));
        world.events = (0..count)
            .map(|_| {
                let at = Duration::from_millis(tc.draw(generators::integers().max_value(20_000)));
                let event = match tc.draw(generators::integers::<u8>().max_value(4)) {
                    0 => Event::Light(Some(tc.draw(any_lux()))),
                    1 => Event::Light(None),
                    2 => Event::ManualAdjustment(
                        tc.draw(generators::integers().max_value(panel_max)),
                    ),
                    3 => Event::Sample,
                    _ => Event::Report,
                };
                (at, event)
            })
            .collect();
        world.events.sort_by_key(|(at, _)| *at);
        let run = simulate(&world, Duration::from_secs(30));
        let mut previous = start;
        let first = curve.target(Light::from_lux(lux));
        let (mut lo, mut hi) = (first, first);
        for (at, seen) in run.seen {
            match seen {
                Seen::Sample(Some(lux)) | Seen::Unasked(Some(lux)) => {
                    let target = curve.target(Light::from_lux(lux));
                    (lo, hi) = (lo.min(target), hi.max(target));
                }
                Seen::Read(raw) | Seen::Report(raw) => previous = raw,
                Seen::Write(raw) => {
                    assert!(
                        (previous.min(lo)..=previous.max(hi)).contains(&raw),
                        "at {at:?}, {previous} toward {lo}..={hi} wrote {raw}"
                    );
                    previous = raw;
                }
                Seen::Sample(None) | Seen::Unasked(None) => {}
            }
        }
    }

    // A fade across the whole range, either way, finishes in under
    // LONGEST_FADE.
    #[hegel::test]
    fn a_fade_across_the_whole_range_is_quick(tc: TestCase) {
        let curve = tc.draw(curves());
        let floor = curve.target(Light::from_lux(0.0));
        let ceiling = curve.target(Light::from_lux(f64::INFINITY));
        let (start, lux) = if tc.draw(generators::booleans()) {
            (floor, f64::INFINITY)
        } else {
            (ceiling, 0.0)
        };
        let mut world = World::new(curve, start, lux);
        world.events = interruptions(&tc, LONGEST_FADE);
        let run = simulate(&world, LONGEST_FADE + Duration::from_secs(2));
        let last = run
            .seen
            .iter()
            .rev()
            .find(|(_, seen)| matches!(seen, Seen::Write(_)));
        if let Some((at, _)) = last {
            assert!(*at < LONGEST_FADE, "took {at:?}");
        }
    }

    // A fade starts from wherever someone left the panel: the first write
    // after the wake is within one frame of it.
    #[hegel::test]
    fn a_fade_starts_from_a_manual_adjustment(tc: TestCase) {
        let curve = tc.draw(curves());
        let floor = curve.target(Light::from_lux(0.0));
        let ceiling = curve.target(Light::from_lux(f64::INFINITY));
        // Twice the deadband, so the light filter gets there in seconds.
        tc.assume(curve.perceived(ceiling) - curve.perceived(floor) >= 2.0 * DEADBAND);
        let nudge = tc.draw(generators::integers().max_value(curve.raw(1.0)));
        // A nudge within the deadband of where the light change leads needs
        // no fade at all.
        tc.assume((curve.perceived(nudge) - curve.perceived(ceiling)).abs() >= DEADBAND);
        let nudged = Duration::from_millis(tc.draw(generators::integers().max_value(5_000)));
        let change =
            nudged + Duration::from_millis(tc.draw(generators::integers().max_value(5_000)));
        let mut world = World::new(curve, floor, 0.0);
        world.events = interruptions(&tc, change + Duration::from_secs(10));
        world.events.extend([
            (nudged, Event::ManualAdjustment(nudge)),
            (change, Event::Light(Some(f64::INFINITY))),
        ]);
        world.events.sort_by_key(|(at, _)| *at);
        // The light filter covers 99% of a change in 10 s.
        let run = simulate(&world, change + Duration::from_secs(11));
        let after_nudge = |seen: &&(Duration, Seen)| seen.0 >= nudged;
        let mut later = run.seen.iter().filter(after_nudge);
        let Some(_) = later.find(|(_, seen)| matches!(seen, Seen::Read(_) | Seen::Report(_)))
        else {
            panic!("never read the panel after the nudge: {:?}", run.seen);
        };
        let Some(&(_, Seen::Write(first))) = later.find(|(_, seen)| matches!(seen, Seen::Write(_)))
        else {
            panic!("never wrote after the nudge: {:?}", run.seen);
        };
        assert_moved_at_most(curve, nudge, first, one_frame_of_easing());
    }

    // An outage shorter than MAX_SENSOR_OUTAGE is ridden out. A longer one
    // is fatal once it reaches MAX_SENSOR_OUTAGE, and no later than the next
    // settled sample after that.
    #[hegel::test]
    fn only_a_long_outage_is_fatal(tc: TestCase) {
        let (world, lux) = steady(&tc);
        let mut world = world;
        let begins =
            Duration::from_millis(tc.draw(generators::integers().min_value(1).max_value(20_000)));
        let lasts = Duration::from_millis(tc.draw(generators::integers().max_value(20_000)));
        world.events.extend([
            (begins, Event::Light(None)),
            (begins + lasts, Event::Light(Some(lux))),
        ]);
        world.events.sort_by_key(|(at, _)| *at);
        let run = simulate(&world, begins + lasts + Duration::from_secs(2));
        // The last readable sample came up to one settled interval before
        // the outage began, so up to 9 s of it is always ridden out.
        let ridden_out = MAX_SENSOR_OUTAGE.saturating_sub(SETTLED_SAMPLE_INTERVAL);
        match run.fatal {
            None => assert!(
                lasts < MAX_SENSOR_OUTAGE + SETTLED_SAMPLE_INTERVAL,
                "{lasts:?}"
            ),
            Some(Fatal::Outage { outage, .. }) => {
                assert!(lasts > ridden_out, "failed after {lasts:?}");
                assert!(
                    (MAX_SENSOR_OUTAGE..MAX_SENSOR_OUTAGE + SETTLED_SAMPLE_INTERVAL)
                        .contains(&outage),
                    "{outage:?}"
                );
            }
            Some(fatal @ Fatal::Startup(_)) => panic!("{fatal:?}"),
        }
    }

    #[test]
    fn an_unanswered_request_is_asked_again() {
        let t0 = Instant::now();
        let mut controller = Controller::new(surface_go());
        let mut step = |input| controller.step::<Unreadable>(t0, input).unwrap();
        assert_eq!(step(Input::Time), Output::Sample);
        assert_eq!(step(Input::Time), Output::Sample);
        assert_eq!(step(Input::Sample(Ok(0.0))), Output::ReadPanel);
        assert_eq!(step(Input::Time), Output::ReadPanel);
        assert!(matches!(step(Input::Panel(150)), Output::SleepUntil(_)));
    }

    #[test]
    fn an_unreadable_first_sample_fails_startup() {
        let mut world = World::new(surface_go(), 150, 0.0);
        world.lux = None;
        let run = simulate(&world, Duration::from_secs(1));
        assert!(matches!(run.fatal, Some(Fatal::Startup(_))));
    }

    /// When each write of one full-range fade on a Surface Go 3 happened,
    /// if each write takes `latency`.
    fn fade_write_times(latency: Duration) -> Vec<Duration> {
        let mut world = World::new(surface_go(), 150, 2_000.0);
        world.latency = latency;
        simulate(&world, LONGEST_FADE)
            .seen
            .into_iter()
            .filter_map(|(at, seen)| matches!(seen, Seen::Write(_)).then_some(at))
            .collect()
    }

    // Unreadable samples while fading leave the fade as it was: the same
    // writes at the same times.
    #[test]
    fn an_unreadable_sample_does_not_pause_a_fade() {
        let steady = World::new(surface_go(), 150, 2_000.0);
        let mut flaky = World::new(surface_go(), 150, 2_000.0);
        // Fading samples fall every 250 ms from the wake at 0, so these
        // make the samples at 1 s and 1.25 s unreadable.
        flaky.events = vec![
            (Duration::from_secs(1), Event::Light(None)),
            (Duration::from_millis(1_400), Event::Light(Some(2_000.0))),
        ];
        let writes = |world: &World| {
            simulate(world, LONGEST_FADE)
                .seen
                .into_iter()
                .filter(|(_, seen)| matches!(seen, Seen::Write(_)))
                .collect::<Vec<_>>()
        };
        assert_eq!(writes(&flaky), writes(&steady));
    }

    #[test]
    fn write_latency_does_not_lengthen_a_fade() {
        // About what one logind write costs on a Surface Go 3.
        let latency = Duration::from_millis(2);
        let times = fade_write_times(latency);
        let with_latency = times.last().unwrap().abs_diff(times[0]);
        let without = {
            let times = fade_write_times(Duration::ZERO);
            times.last().unwrap().abs_diff(times[0])
        };
        assert!(
            with_latency.abs_diff(without) <= FRAME,
            "fade took {with_latency:?} with latency, {without:?} without"
        );
    }

    // The outage counts measured time, not samples. A late shell that wakes
    // one and a half settled intervals after each deadline takes a sample
    // every 2.5 s, so the outage fires at 10 s, on the fourth unreadable
    // sample rather than the tenth.
    #[test]
    fn the_outage_counts_measured_time_not_samples() {
        let oversleep = SETTLED_SAMPLE_INTERVAL * 3 / 2;
        let mut controller = Controller::new(surface_go());
        let mut now = Instant::now();
        let mut input = Input::Time;
        let mut readable = true;
        let mut unreadable_samples = 0;
        let fatal = loop {
            input = match controller.step(now, input) {
                Err(fatal) => break fatal,
                Ok(Output::Sample) if readable => {
                    readable = false;
                    Input::Sample(Ok(0.0))
                }
                Ok(Output::Sample) => {
                    unreadable_samples += 1;
                    Input::Sample(Err(Unreadable))
                }
                Ok(Output::ReadPanel) => Input::Panel(150),
                Ok(Output::Write(raw)) => panic!("wrote {raw} to a settled panel"),
                Ok(Output::SleepUntil(deadline)) => {
                    now = deadline + oversleep;
                    Input::Time
                }
            };
        };
        let Fatal::Outage { outage, .. } = fatal else {
            panic!("{fatal:?}");
        };
        assert_eq!(outage, MAX_SENSOR_OUTAGE);
        assert_eq!(unreadable_samples, 4);
    }

    /// The most one frame of easing can move the panel in lightness: the
    /// share of the gap that a first-order filter with time constant
    /// `TAU_FADE` closes in one `FRAME`, across the full scale of 1.
    fn one_frame_of_easing() -> f64 {
        1.0 - (-FRAME.as_secs_f64() / TAU_FADE).exp()
    }

    /// Asserts that the panel moved at most `bound` in lightness between
    /// raw `a` and `b`, plus the width of one raw step, which is all
    /// rounding can add.
    fn assert_moved_at_most(curve: Curve, a: u32, b: u32, bound: f64) {
        let (lo, hi) = (a.min(b), a.max(b));
        let moved = curve.perceived(hi) - curve.perceived(lo);
        let one_step = curve.perceived(lo + 1) - curve.perceived(lo);
        // The last term absorbs float rounding in the cube and cube root.
        assert!(
            moved <= bound + one_step + 1e-12,
            "{a} to {b} moved {moved}, more than {bound} + {one_step}"
        );
    }
}
