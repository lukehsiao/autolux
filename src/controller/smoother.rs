//! The dynamic half of autolux: how the panel gets from where it is to where
//! the [`Curve`] says it should be, without visible steps.
//!
//! Two first-order filters run in series. The first low-passes the light
//! level so a passing shadow barely registers; the second eases the panel
//! toward the curve's target for that filtered light in lightness. Chaining
//! them makes brightness change with continuous speed: a fade starts gently,
//! runs, and slows to a stop, rather than jumping in steps.
//!
//! A fade moves in whole frames, so the per-frame coefficients are worked
//! out once, in [`Smoother::new`]. Time is an argument, not a clock, so
//! everything here is deterministic.

use std::time::Duration;

use crate::curve::{Curve, Light};

/// How often the panel can change while fading: once per refresh of a 60 Hz
/// display, the finest change anyone can see.
///
/// No single frame moves the panel by more than about 1.7% of the lightness
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
pub const TAU_FADE: f64 = 1.0;

/// Slowest a fade may move, in lightness per second.
///
/// An exponential approach never arrives, it just creeps by invisible
/// amounts. With this floor the last stretch runs at a steady pace and a
/// fade across the whole range finishes in about five seconds.
const MIN_SPEED: f64 = 0.02;

/// How far in lightness the target may drift from a settled panel before a
/// new fade starts.
///
/// About one just-noticeable difference in lightness. Smaller corrections
/// are invisible, so sensor jitter should not wake the panel for them. The
/// deadband never excuses a panel outside the configured limits, though:
/// those are hard bounds, so even an invisible excursion past one is fixed.
pub const DEADBAND: f64 = 0.01;

/// The panel after one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// The raw brightness that shows the panel's lightness best.
    pub raw: u32,
    /// This frame reached the target exactly and ended the fade.
    pub landed: bool,
}

/// The light filter, the panel's lightness and the fade, in a few plain
/// numbers. It is `Copy`, so the controller can run frames ahead on a copy.
#[derive(Debug, Clone, Copy)]
pub struct Smoother {
    curve: Curve,
    /// One frame's share of the light filter: `1 - exp(-FRAME / TAU_LIGHT)`.
    light_per_frame: f64,
    /// One frame's share of the remaining distance: `1 - exp(-FRAME / TAU_FADE)`.
    ease_per_frame: f64,
    /// One frame at [`MIN_SPEED`].
    min_step: f64,
    /// Most recent light sample.
    sample: Light,
    /// `sample`, low-passed. The target follows this, not the raw sample.
    filtered: Light,
    /// The panel's lightness. Kept continuous so progress smaller than one
    /// raw step accumulates instead of being rounded away.
    level: f64,
    /// The raw brightness last written to or read from the panel.
    written: u32,
    fading: bool,
}

impl Smoother {
    /// Starts settled, with the panel at `brightness` and the filter primed
    /// with `lux`.
    #[must_use]
    pub fn new(curve: Curve, lux: f64, brightness: u32) -> Self {
        let frame = FRAME.as_secs_f64();
        let light = Light::from_lux(lux);
        Self {
            curve,
            light_per_frame: 1.0 - (-frame / TAU_LIGHT).exp(),
            ease_per_frame: 1.0 - (-frame / TAU_FADE).exp(),
            min_step: MIN_SPEED * frame,
            sample: light,
            filtered: light,
            level: curve.perceived(brightness),
            written: brightness,
            fading: false,
        }
    }

    #[must_use]
    pub fn curve(&self) -> &Curve {
        &self.curve
    }

    #[must_use]
    pub fn is_fading(&self) -> bool {
        self.fading
    }

    /// The raw brightness last written to or read from the panel.
    #[must_use]
    pub fn written(&self) -> u32 {
        self.written
    }

    /// The target for the filtered light.
    #[must_use]
    pub fn target(&self) -> u32 {
        self.curve.target(self.filtered)
    }

    /// Records a light sample. Frames move the filter toward it while
    /// fading; while settled, [`Smoother::integrate`] does.
    pub fn observe(&mut self, lux: f64) {
        self.sample = Light::from_lux(lux);
    }

    /// Lets `elapsed` pass on the light filter, for a settled panel that
    /// runs no frames.
    pub fn integrate(&mut self, elapsed: Duration) {
        let fraction = 1.0 - (-elapsed.as_secs_f64() / TAU_LIGHT).exp();
        self.filtered = self.filtered.approach(self.sample, fraction);
    }

    /// Whether a settled panel must start a fade: the target has left the
    /// deadband, or the panel is outside the limits.
    #[must_use]
    pub fn should_wake(&self) -> bool {
        let goal = self.curve.perceived(self.target());
        (goal - self.level).abs() >= DEADBAND || !self.curve.within_limits(self.written)
    }

    /// Starts a fade from `brightness`, read from the panel.
    pub fn wake(&mut self, brightness: u32) {
        self.resync(brightness);
        self.fading = true;
    }

    /// Takes `brightness` as what is on the panel now, as after a manual
    /// adjustment. A settled panel keeps its place in the deadband, so the
    /// next fade waits for the light to change. A fading panel continues
    /// from `brightness`. A reading of the value already on the panel keeps
    /// the progress smaller than one raw step.
    pub fn adjust(&mut self, brightness: u32) {
        if self.fading && brightness != self.written {
            self.resync(brightness);
        }
        self.written = brightness;
    }

    /// Records that `raw` is now on the panel.
    pub fn wrote(&mut self, raw: u32) {
        self.written = raw;
    }

    /// Runs one frame of a fade.
    pub fn frame(&mut self) -> Frame {
        debug_assert!(self.fading, "a settled panel runs no frames");
        self.filtered = self.filtered.approach(self.sample, self.light_per_frame);
        let target = self.target();
        let goal = self.curve.perceived(target);
        let distance = goal - self.level;
        let step = (distance.abs() * self.ease_per_frame).max(self.min_step);
        if step >= distance.abs() {
            self.level = goal;
            self.fading = false;
            return Frame {
                raw: target,
                landed: true,
            };
        }
        self.level += step.copysign(distance);
        Frame {
            raw: self.curve.raw(self.level),
            landed: false,
        }
    }

    fn resync(&mut self, brightness: u32) {
        self.written = brightness;
        self.level = self.curve.perceived(brightness);
    }
}
