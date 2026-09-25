//! The dynamic half of autolux: how the panel gets from where it is to where
//! the [`Curve`] says it should be, without visible steps.
//!
//! Two first-order filters run in series. The first low-passes the light
//! level so a passing shadow barely registers; the second eases the panel
//! toward the curve's target for that filtered light on a perceptual scale.
//! Chaining them makes brightness change with continuous speed: a fade
//! starts gently, runs, and slows to a stop, rather than jumping in steps.
//!
//! Time is an argument, not a clock, so everything here is deterministic.

use std::time::Duration;

use crate::curve::{Curve, Light};

/// How often to update the panel while fading: once per refresh of a 60 Hz
/// display, the finest change anyone can see.
///
/// No single frame moves the panel by more than about 1.7% of the perceptual
/// scale (see [`TAU_FADE`]). A settled panel is not woken at this rate at all.
pub const FRAME: Duration = Duration::from_nanos(16_666_667);

/// Time constant of the light filter, in seconds.
///
/// Short enough that the panel visibly starts following a light switch
/// within a fraction of a second and is mostly there in a few; long enough
/// that a single odd sample, or someone briefly shading the sensor, moves it
/// only a little.
const TAU_LIGHT: f64 = 2.0;

/// Time constant of the panel's approach to the target, in seconds.
///
/// Its jobs are to round off the start of a fade, so speed never jumps, and
/// to glide from a brightness someone set by hand. It also bounds each frame:
/// `1 - exp(-FRAME / TAU_FADE)` of the full scale, about 1.7%.
const TAU_FADE: f64 = 1.0;

/// Slowest a fade may move, in perceptual units per second.
///
/// An exponential approach never arrives, it just creeps by invisible
/// amounts. With this floor the last stretch runs at a steady pace and a
/// fade across the whole range finishes in about five seconds.
const MIN_SPEED: f64 = 0.02;

/// How far, on the perceptual scale, the target may drift from a settled
/// panel before a new fade starts.
///
/// About one just-noticeable difference in lightness. Smaller corrections
/// are invisible, so sensor jitter should not wake the panel for them. The
/// deadband never excuses a panel outside the configured limits, though:
/// those are hard bounds, so even an invisible excursion past one is fixed.
const DEADBAND: f64 = 0.01;

/// What the caller should do after [`Smoother::advance`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Step {
    /// The panel is inside the configured limits and within the deadband of
    /// its target. Nothing to do until the next light sample.
    Settled,
    /// The target moved out of the deadband, or the panel is outside the
    /// limits, and a fade is starting toward `target`. The panel may have
    /// been adjusted by someone else while settled, so read it and pass it
    /// to [`Smoother::resync`] before the next frame.
    Wake { target: u32 },
    /// Mid-fade. Write the raw value if there is one; `None` means this frame
    /// rounded to the value already on the panel.
    Fade(Option<u32>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Motion {
    Settled,
    Fading,
}

#[derive(Debug, Clone)]
pub struct Smoother {
    curve: Curve,
    /// Most recent light sample.
    sample: Light,
    /// `sample`, low-passed. The target follows this, not the raw sample.
    filtered: Light,
    /// The panel on the perceptual scale. Kept continuous so progress
    /// smaller than one raw step accumulates instead of being rounded away.
    level: f64,
    /// Last raw value written to or read from the panel.
    written: u32,
    motion: Motion,
}

impl Smoother {
    /// Starts settled, with the panel at `brightness` and the filter primed
    /// with `lux`, so the first [`Smoother::advance`] wakes if the panel is
    /// visibly off target.
    #[must_use]
    pub fn new(curve: Curve, brightness: u32, lux: f64) -> Self {
        let light = Light::from_lux(lux);
        Self {
            curve,
            sample: light,
            filtered: light,
            level: curve.perceived(brightness),
            written: brightness,
            motion: Motion::Settled,
        }
    }

    #[must_use]
    pub fn curve(&self) -> &Curve {
        &self.curve
    }

    /// Whether a fade is in progress, from [`Step::Wake`] until it lands.
    #[must_use]
    pub fn is_fading(&self) -> bool {
        self.motion == Motion::Fading
    }

    /// Records a new light sample; the filter moves toward it over the
    /// following calls to [`Smoother::advance`].
    pub fn observe(&mut self, lux: f64) {
        self.sample = Light::from_lux(lux);
    }

    /// Replaces what the smoother believes is on the panel with a fresh
    /// reading, so a fade starts from where the panel actually is.
    pub fn resync(&mut self, brightness: u32) {
        self.written = brightness;
        self.level = self.curve.perceived(brightness);
    }

    /// Lets `elapsed` pass and says what to do about it.
    ///
    /// While fading, call this once per [`FRAME`]; while settled, once per
    /// light sample is enough. The filters integrate exactly for any
    /// `elapsed`, so the pace can change freely between calls.
    pub fn advance(&mut self, elapsed: Duration) -> Step {
        let dt = elapsed.as_secs_f64();
        self.filtered = self
            .filtered
            .approach(self.sample, 1.0 - (-dt / TAU_LIGHT).exp());
        let target = self.curve.target(self.filtered);
        let goal = self.curve.perceived(target);
        let distance = goal - self.level;

        if self.motion == Motion::Settled {
            if distance.abs() < DEADBAND && self.curve.within_limits(self.written) {
                return Step::Settled;
            }
            // Wake without moving: the caller resyncs first, and the fade
            // proper starts on the next frame.
            self.motion = Motion::Fading;
            return Step::Wake { target };
        }

        let step = (distance.abs() * (1.0 - (-dt / TAU_FADE).exp())).max(MIN_SPEED * dt);
        let raw = if step >= distance.abs() {
            self.level = goal;
            self.motion = Motion::Settled;
            target
        } else {
            self.level += step.copysign(distance);
            self.curve.raw(self.level)
        };
        if raw == self.written {
            return Step::Fade(None);
        }
        self.written = raw;
        Step::Fade(Some(raw))
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU32, time::Duration};

    use hegel::{TestCase, generators};

    use super::{DEADBAND, FRAME, Smoother, Step, TAU_FADE};
    use crate::curve::{
        Curve, Light, Limits,
        tests::{any_lux, curves},
    };

    /// Longest a fade across the whole range may take. The constants give
    /// just under five seconds; the rest is margin for float rounding.
    const LONGEST_FADE: Duration = Duration::from_secs(6);

    /// A panel that reads back whatever was last written to it, driven the
    /// way the controller drives it: one frame at a time, resyncing on wake.
    /// Returns every value written, in order, or `None` if it never settled
    /// within `limit`.
    fn fade_until_settled(smoother: &mut Smoother, limit: Duration) -> Option<Vec<u32>> {
        let mut panel = smoother.written;
        let mut writes = Vec::new();
        let mut step = smoother.advance(Duration::ZERO);
        let mut elapsed = Duration::ZERO;
        loop {
            match step {
                Step::Settled => return Some(writes),
                Step::Wake { .. } => smoother.resync(panel),
                Step::Fade(Some(raw)) => {
                    panel = raw;
                    writes.push(raw);
                }
                Step::Fade(None) => {}
            }
            if elapsed > limit {
                return None;
            }
            elapsed += FRAME;
            step = smoother.advance(FRAME);
        }
    }

    #[hegel::composite]
    fn scenarios(tc: &TestCase) -> (Curve, u32, f64) {
        let curve = tc.draw(curves());
        let panel_max = curve.raw(1.0);
        let start = tc.draw(generators::integers::<u32>().max_value(panel_max));
        let lux = tc.draw(any_lux());
        (curve, start, lux)
    }

    // In steady light, the panel always settles, quickly, and a fade that
    // starts lands exactly on the curve's target. A panel that never woke
    // was already within the deadband.
    #[hegel::test]
    fn settles_on_target_in_steady_light(tc: TestCase) {
        let (curve, start, lux) = tc.draw(scenarios());
        let mut smoother = Smoother::new(curve, start, lux);
        let writes = fade_until_settled(&mut smoother, LONGEST_FADE)
            .expect("did not settle within the longest fade");
        let target = curve.target(Light::from_lux(lux));
        match writes.last() {
            Some(last) => assert_eq!(*last, target),
            None => assert!(
                (curve.perceived(start) - curve.perceived(target)).abs() < DEADBAND,
                "never moved from {start} although the target is {target}"
            ),
        }
    }

    // The configured limits are hard bounds: in steady light, the panel
    // always ends up inside them, even when it starts outside by less than
    // the deadband.
    #[hegel::test]
    fn always_settles_within_the_limits(tc: TestCase) {
        let (curve, start, lux) = tc.draw(scenarios());
        let mut smoother = Smoother::new(curve, start, lux);
        let writes = fade_until_settled(&mut smoother, LONGEST_FADE).unwrap();
        let settled = writes.last().copied().unwrap_or(start);
        let floor = curve.target(Light::from_lux(0.0));
        let ceiling = curve.target(Light::from_lux(f64::INFINITY));
        assert!(
            (floor..=ceiling).contains(&settled),
            "started at {start}, settled at {settled}, outside {floor}..={ceiling}"
        );
    }

    // 5700 of 7500 is 76%, just past a 75% ceiling and only 0.004 from it
    // on the perceptual scale, well inside the deadband. It must still come
    // down to exactly the ceiling.
    #[test]
    fn a_panel_just_above_the_ceiling_comes_down_to_it() {
        let curve = Curve::new(Limits::new(2, 75).unwrap(), NonZeroU32::new(7500).unwrap());
        let mut smoother = Smoother::new(curve, 5700, 2_000.0);
        let writes = fade_until_settled(&mut smoother, LONGEST_FADE).unwrap();
        assert_eq!(writes.last(), Some(&5625));
    }

    // With min == max there is exactly one right brightness, and a panel a
    // hair off it must still land on it.
    #[test]
    fn a_fixed_brightness_is_reached_exactly() {
        let curve = Curve::new(Limits::new(50, 50).unwrap(), NonZeroU32::new(7500).unwrap());
        let mut smoother = Smoother::new(curve, 3760, 123.0);
        let writes = fade_until_settled(&mut smoother, LONGEST_FADE).unwrap();
        assert_eq!(writes.last(), Some(&3750));
    }

    // In steady light, a fade heads straight for the target: every write is
    // between the start and the target, and each is closer than the last.
    #[hegel::test]
    fn fades_monotonically_without_overshoot(tc: TestCase) {
        let (curve, start, lux) = tc.draw(scenarios());
        let mut smoother = Smoother::new(curve, start, lux);
        let writes = fade_until_settled(&mut smoother, LONGEST_FADE).unwrap();
        let target = curve.target(Light::from_lux(lux));
        let mut previous = start;
        for raw in writes {
            let (lo, hi) = (previous.min(target), previous.max(target));
            assert!(
                (lo..=hi).contains(&raw) && raw.abs_diff(target) < previous.abs_diff(target),
                "{start} to {target}: {previous} then {raw}"
            );
            previous = raw;
        }
    }

    // No frame visibly jumps: between consecutive writes the panel moves by at
    // most one frame's worth of easing across the full scale, plus the width
    // of one raw step, which is all rounding can add.
    #[hegel::test]
    fn each_frame_moves_the_panel_a_little(tc: TestCase) {
        let (curve, start, lux) = tc.draw(scenarios());
        let mut smoother = Smoother::new(curve, start, lux);
        let writes = fade_until_settled(&mut smoother, LONGEST_FADE).unwrap();
        let easing = 1.0 - (-FRAME.as_secs_f64() / TAU_FADE).exp();
        for pair in std::iter::once(start)
            .chain(writes)
            .collect::<Vec<_>>()
            .windows(2)
        {
            let (lo, hi) = (pair[0].min(pair[1]), pair[0].max(pair[1]));
            let moved = curve.perceived(hi) - curve.perceived(lo);
            let one_step = curve.perceived(lo + 1) - curve.perceived(lo);
            // The last term absorbs float rounding in the cube and cube root.
            assert!(
                moved <= easing + one_step + 1e-12,
                "{lo} to {hi} moved {moved}, more than {easing} + {one_step}"
            );
        }
    }

    // A settled panel is not woken by light whose target is within the
    // deadband, however long it keeps arriving.
    #[hegel::test]
    fn small_changes_do_not_wake_a_settled_panel(tc: TestCase) {
        let curve = tc.draw(curves());
        let lux = tc.draw(any_lux());
        let target = curve.target(Light::from_lux(lux));
        let mut smoother = Smoother::new(curve, target, lux);
        let nearby = tc.draw(any_lux());
        let nearby_target = curve.target(Light::from_lux(nearby));
        tc.assume((curve.perceived(nearby_target) - curve.perceived(target)).abs() < DEADBAND);
        smoother.observe(nearby);
        for _ in 0..60 {
            assert_eq!(smoother.advance(Duration::from_secs(1)), Step::Settled);
        }
    }

    // A settled panel visibly off target wakes on the very next call, naming
    // the target, and moves nothing until it has been resynced.
    #[hegel::test]
    fn visible_changes_wake_immediately(tc: TestCase) {
        let (curve, start, lux) = tc.draw(scenarios());
        let target = curve.target(Light::from_lux(lux));
        tc.assume((curve.perceived(start) - curve.perceived(target)).abs() >= DEADBAND);
        let mut smoother = Smoother::new(curve, start, lux);
        assert_eq!(smoother.advance(Duration::ZERO), Step::Wake { target });
    }

    // Under any interleaving of samples, resyncs and time steps, every value
    // written lies between the one before it and the target at that moment:
    // the panel never overshoots, even while the target moves.
    #[hegel::test]
    fn never_overshoots_a_moving_target(tc: TestCase) {
        let (curve, start, lux) = tc.draw(scenarios());
        let panel_max = curve.raw(1.0);
        let mut smoother = Smoother::new(curve, start, lux);
        let ops = tc.draw(generators::integers::<usize>().max_value(200));
        for _ in 0..ops {
            match tc.draw(generators::integers::<u8>().max_value(3)) {
                0 => smoother.observe(tc.draw(any_lux())),
                1 => smoother.resync(tc.draw(generators::integers::<u32>().max_value(panel_max))),
                _ => {
                    let before = smoother.written;
                    let elapsed = tc.draw(generators::integers::<u64>().max_value(2_000));
                    let step = smoother.advance(Duration::from_millis(elapsed));
                    let target = curve.target(smoother.filtered);
                    if let Step::Fade(Some(raw)) = step {
                        let (lo, hi) = (before.min(target), before.max(target));
                        assert!(
                            (lo..=hi).contains(&raw),
                            "{before} toward {target} wrote {raw}"
                        );
                    }
                }
            }
        }
    }
}
