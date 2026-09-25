//! The static half of autolux: which brightness a given illuminance calls
//! for, and how raw backlight values relate to perceived brightness.

use std::num::NonZeroU32;

use crate::error::{Error, Result};

/// At or below this illuminance the panel sits at the configured minimum.
///
/// A dark room reads about 2.4 lux on a Surface Go 3's sensor, so the curve
/// starts just above that.
const DARK_LUX: f64 = 3.0;

/// At or above this illuminance the panel sits at the configured maximum.
///
/// 500 lux is what EN 12464-1 recommends for office work, about as bright as
/// indoor lighting gets. Anything brighter is daylight, where the panel
/// belongs at its maximum anyway.
const BRIGHT_LUX: f64 = 500.0;

/// Illuminance on the scale the curve works in: `ln(lux)`, limited to
/// [`DARK_LUX`]..=[`BRIGHT_LUX`].
///
/// Perceived brightness is roughly logarithmic in illuminance, so this is
/// the scale on which filtering makes sense. Readings outside the range (NaN
/// and infinities included) have the same target as the nearest end, and
/// bounding them keeps a low-pass filter from winding up past what the panel
/// can show: after a flashlight at 740 lux, the panel starts dimming as soon
/// as the light goes away instead of first discharging the excess.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Light(f64);

impl Light {
    #[must_use]
    #[expect(
        clippy::manual_clamp,
        reason = "f64::clamp propagates NaN, while max(NaN, x) is x, so an unreadable value counts as dark"
    )]
    pub fn from_lux(lux: f64) -> Self {
        Self(lux.max(DARK_LUX).min(BRIGHT_LUX).ln())
    }

    /// Back to lux, for logs.
    #[must_use]
    pub fn lux(self) -> f64 {
        self.0.exp()
    }

    /// Moves `fraction` (clamped to 0..=1) of the way toward `target`.
    #[must_use]
    #[expect(
        clippy::manual_clamp,
        reason = "f64::clamp propagates NaN, while max/min maps a NaN fraction to no movement"
    )]
    pub fn approach(self, target: Self, fraction: f64) -> Self {
        let fraction = fraction.max(0.0).min(1.0);
        // Rounding can land a hair past `target` (ln 3 moved all the way to
        // ln 24 overshoots by one ulp). Clamping between the two endpoints
        // prevents that, and keeps the range invariant since both are in it.
        let (lo, hi) = if self.0 <= target.0 {
            (self.0, target.0)
        } else {
            (target.0, self.0)
        };
        Self((self.0 + (target.0 - self.0) * fraction).max(lo).min(hi))
    }
}

/// The configured brightness range, as percentages of the panel's maximum.
///
/// Invariant: `min <= max <= 100`.
#[cfg_attr(test, derive(hegel::PrettyPrintable))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    min: u8,
    max: u8,
}

impl Limits {
    /// # Errors
    /// A percentage above 100, or `min > max`.
    pub fn new(min: u8, max: u8) -> Result<Self> {
        if let Some(percent) = [min, max].into_iter().find(|p| *p > 100) {
            return Err(Error::InvalidPercent { percent });
        }
        if min > max {
            return Err(Error::InvalidRange { min, max });
        }
        Ok(Self { min, max })
    }
}

/// Maps illuminance to a raw backlight value between two configured limits.
///
/// Invariant: `min <= max <= panel_max`.
#[cfg_attr(test, derive(hegel::PrettyPrintable))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Curve {
    min: u32,
    max: u32,
    panel_max: NonZeroU32,
}

impl Curve {
    /// Converts `limits` to raw values of a panel whose `max_brightness` is
    /// `panel_max`, each rounded to the nearest raw value.
    #[must_use]
    pub fn new(limits: Limits, panel_max: NonZeroU32) -> Self {
        // Integer arithmetic, because ties are common: 70% of 21565 is
        // exactly 15095.5, but 0.7 in binary is a hair short of it.
        let raw = |percent: u8| {
            let raw = (u64::from(panel_max.get()) * u64::from(percent) + 50) / 100;
            // percent <= 100, so raw <= panel_max and always fits.
            u32::try_from(raw).unwrap_or(panel_max.get())
        };
        Self {
            min: raw(limits.min),
            max: raw(limits.max),
            panel_max,
        }
    }

    /// The raw brightness the panel should settle at in `light`.
    ///
    /// Linear in `ln(lux)`: every doubling of the room's light raises the
    /// panel by the same amount, from the minimum at [`DARK_LUX`] to the
    /// maximum at [`BRIGHT_LUX`].
    #[must_use]
    pub fn target(&self, light: Light) -> u32 {
        let fraction = (light.0 - DARK_LUX.ln()) / (BRIGHT_LUX.ln() - DARK_LUX.ln());
        let span = self.max - self.min;
        self.min + round_to_u32(fraction * f64::from(span), span)
    }

    /// Whether `raw` is between the configured limits, inclusive.
    #[must_use]
    pub fn within_limits(&self, raw: u32) -> bool {
        (self.min..=self.max).contains(&raw)
    }

    /// Where `raw` sits on a perceptually uniform scale, 0 at off and 1 at
    /// the panel's maximum.
    ///
    /// Backlight drivers are linear in emitted light, but the eye is not: CIE
    /// lightness goes roughly as the cube root of luminance. Fading on this
    /// scale makes equal steps look equally large at any brightness, so the
    /// dim end does not lurch while the bright end crawls.
    #[must_use]
    pub fn perceived(&self, raw: u32) -> f64 {
        (f64::from(raw) / f64::from(self.panel_max.get())).cbrt()
    }

    /// Inverse of [`Curve::perceived`], rounded to the nearest raw value and
    /// clamped to the panel's range.
    #[must_use]
    pub fn raw(&self, perceived: f64) -> u32 {
        let panel = self.panel_max.get();
        round_to_u32(perceived.powi(3) * f64::from(panel), panel)
    }

    /// `raw` as a whole percentage of the panel's maximum, for logs.
    #[must_use]
    pub fn percent(&self, raw: u32) -> u32 {
        round_to_u32(
            f64::from(raw) * 100.0 / f64::from(self.panel_max.get()),
            100,
        )
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is rounded and clamped to 0..=limit first, so the cast is exact"
)]
fn round_to_u32(value: f64, limit: u32) -> u32 {
    // max(NaN, 0.0) is 0.0, which keeps NaN from reaching the cast.
    value.round().max(0.0).min(f64::from(limit)) as u32
}

#[cfg(test)]
pub(crate) mod tests {
    use std::num::NonZeroU32;

    use hegel::{TestCase, generators};

    use super::{BRIGHT_LUX, Curve, DARK_LUX, Light, Limits};
    use crate::error::Error;

    #[hegel::composite]
    pub(crate) fn limits(tc: &TestCase) -> Limits {
        let max = tc.draw(generators::integers::<u8>().max_value(100));
        let min = tc.draw(generators::integers::<u8>().max_value(max));
        Limits::new(min, max).unwrap()
    }

    #[hegel::composite]
    pub(crate) fn curves(tc: &TestCase) -> Curve {
        let panel_max = tc.draw(generators::integers::<u32>().min_value(1));
        Curve::new(tc.draw(limits()), NonZeroU32::new(panel_max).unwrap())
    }

    /// Every f64, NaN and infinities included: what a broken sensor could hand us.
    pub(crate) fn any_lux() -> generators::FloatGenerator<f64> {
        generators::floats::<f64>()
            .allow_nan(true)
            .allow_infinity(true)
    }

    /// Illuminance inside the curve's responsive range.
    fn responsive_lux() -> generators::FloatGenerator<f64> {
        generators::floats::<f64>()
            .min_value(DARK_LUX)
            .max_value(BRIGHT_LUX)
    }

    // Every percentage pair with min <= max <= 100 is accepted, and a curve's
    // raw limits are those percentages of the panel maximum, rounded.
    #[hegel::test]
    fn ordered_percentages_become_rounded_raw_limits(tc: TestCase) {
        let max = tc.draw(generators::integers::<u8>().max_value(100));
        let min = tc.draw(generators::integers::<u8>().max_value(max));
        let panel_max = tc.draw(generators::integers::<u32>().min_value(1));
        let curve = Curve::new(
            Limits::new(min, max).unwrap(),
            NonZeroU32::new(panel_max).unwrap(),
        );
        // Nearest raw value, ties up: 100 * raw lies in (p * max - 50, p * max + 50].
        let scaled = |percent: u8| i128::from(panel_max) * i128::from(percent);
        for (percent, raw) in [(min, curve.min), (max, curve.max)] {
            let hundredfold = i128::from(raw) * 100;
            assert!(
                scaled(percent) - 50 < hundredfold && hundredfold <= scaled(percent) + 50,
                "{percent}% of {panel_max} became {raw}"
            );
        }
    }

    // A percentage above 100 is rejected whichever limit it is in.
    #[hegel::test]
    fn limits_reject_percentages_above_100(tc: TestCase) {
        let bad = tc.draw(generators::integers::<u8>().min_value(101));
        let other = tc.draw(generators::integers::<u8>());
        let (min, max) = if tc.draw(generators::booleans()) {
            (bad, other)
        } else {
            (other, bad)
        };
        let err = Limits::new(min, max).unwrap_err();
        let first_bad = if min > 100 { min } else { max };
        assert!(
            matches!(err, Error::InvalidPercent { percent } if percent == first_bad),
            "{err:?}"
        );
    }

    // An inverted range is rejected rather than silently swapped.
    #[hegel::test]
    fn limits_reject_min_above_max(tc: TestCase) {
        let min = tc.draw(generators::integers::<u8>().min_value(1).max_value(100));
        let max = tc.draw(generators::integers::<u8>().max_value(min - 1));
        let err = Limits::new(min, max).unwrap_err();
        assert!(
            matches!(err, Error::InvalidRange { min: a, max: b } if a == min && b == max),
            "{err:?}"
        );
    }

    // Whatever the sensor reports, the light level is finite and inside the
    // curve's responsive range.
    #[hegel::test]
    fn light_is_always_finite_and_in_range(tc: TestCase) {
        let light = Light::from_lux(tc.draw(any_lux()));
        let range = DARK_LUX.ln()..=BRIGHT_LUX.ln();
        assert!(range.contains(&light.0), "{light:?}");
    }

    // Moving one light level toward another, by any fraction at all, lands
    // between the two.
    #[hegel::test]
    fn approach_stays_between_its_endpoints(tc: TestCase) {
        let a = Light::from_lux(tc.draw(any_lux()));
        let b = Light::from_lux(tc.draw(any_lux()));
        let fraction = tc.draw(any_lux());
        let c = a.approach(b, fraction);
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        assert!(
            lo <= c && c <= hi,
            "{a:?} toward {b:?} by {fraction} gave {c:?}"
        );
    }

    // Whatever the sensor reports, the target stays within the configured limits.
    #[hegel::test]
    fn target_stays_within_limits(tc: TestCase) {
        let curve = tc.draw(curves());
        let target = curve.target(Light::from_lux(tc.draw(any_lux())));
        assert!((curve.min..=curve.max).contains(&target), "{target}");
    }

    // More light never means a dimmer panel.
    #[hegel::test]
    fn target_is_monotonic_in_lux(tc: TestCase) {
        let curve = tc.draw(curves());
        let a = tc.draw(
            generators::floats::<f64>()
                .allow_nan(false)
                .allow_infinity(true),
        );
        let b = tc.draw(
            generators::floats::<f64>()
                .allow_nan(false)
                .allow_infinity(true),
        );
        let (lo, hi) = (a.min(b), a.max(b));
        assert!(curve.target(Light::from_lux(lo)) <= curve.target(Light::from_lux(hi)));
    }

    // Dark rooms (and unreadable NaN) get exactly the minimum; bright rooms
    // exactly the maximum.
    #[hegel::test]
    fn target_saturates_at_the_ends(tc: TestCase) {
        let curve = tc.draw(curves());
        let dark = tc.draw(
            generators::floats::<f64>()
                .allow_infinity(true)
                .max_value(DARK_LUX),
        );
        let bright = tc.draw(
            generators::floats::<f64>()
                .allow_infinity(true)
                .min_value(BRIGHT_LUX),
        );
        assert_eq!(curve.target(Light::from_lux(dark)), curve.min);
        assert_eq!(curve.target(Light::from_lux(f64::NAN)), curve.min);
        assert_eq!(curve.target(Light::from_lux(bright)), curve.max);
    }

    // The curve is logarithmic: multiplying the light by the same factor
    // raises the target by the same amount wherever it starts, give or take
    // the rounding of the four values involved.
    #[hegel::test]
    fn equal_light_ratios_give_equal_steps(tc: TestCase) {
        let curve = tc.draw(curves());
        let a = tc.draw(responsive_lux());
        let b = tc.draw(responsive_lux());
        let ratio = tc.draw(
            generators::floats::<f64>()
                .min_value(1.0)
                .max_value(BRIGHT_LUX / a.max(b)),
        );
        let step = |lux: f64| {
            i64::from(curve.target(Light::from_lux(lux * ratio)))
                - i64::from(curve.target(Light::from_lux(lux)))
        };
        assert!(
            (step(a) - step(b)).abs() <= 2,
            "x{ratio} from {a} lux rose {}, from {b} lux rose {}",
            step(a),
            step(b)
        );
    }

    // Halfway through the range on a log scale, the geometric mean of the
    // ends (about 38.7 lux), is halfway between the limits.
    #[hegel::test]
    fn geometric_middle_is_halfway(tc: TestCase) {
        let curve = tc.draw(curves());
        let target = curve.target(Light::from_lux((DARK_LUX * BRIGHT_LUX).sqrt()));
        let halfway = f64::midpoint(f64::from(curve.min), f64::from(curve.max));
        assert!(
            (f64::from(target) - halfway).abs() <= 1.0,
            "{target} is not halfway between {} and {}",
            curve.min,
            curve.max
        );
    }

    // Converting a raw value to the perceptual scale and back is lossless, so
    // a fade that lands on a target's perceived level writes that exact target.
    #[hegel::test]
    fn perceived_round_trips_through_raw(tc: TestCase) {
        let curve = tc.draw(curves());
        let raw = tc.draw(generators::integers::<u32>().max_value(curve.panel_max.get()));
        assert_eq!(curve.raw(curve.perceived(raw)), raw);
    }

    // Any perceived level, even nonsense, maps back into the panel's range.
    #[hegel::test]
    fn raw_stays_within_the_panel_range(tc: TestCase) {
        let curve = tc.draw(curves());
        assert!(curve.raw(tc.draw(any_lux())) <= curve.panel_max.get());
    }

    // The perceptual scale preserves order.
    #[hegel::test]
    fn perceived_is_monotonic(tc: TestCase) {
        let curve = tc.draw(curves());
        let a = tc.draw(generators::integers::<u32>().max_value(curve.panel_max.get()));
        let b = tc.draw(generators::integers::<u32>().max_value(curve.panel_max.get()));
        assert!(curve.perceived(a.min(b)) <= curve.perceived(a.max(b)));
    }

    // The perceptual scale runs from exactly 0 (off) to exactly 1 (the
    // panel's maximum), whatever that maximum is.
    #[hegel::test]
    #[expect(clippy::float_cmp, reason = "cbrt(0) and cbrt(1) are exact")]
    fn perceived_spans_zero_to_one(tc: TestCase) {
        let curve = tc.draw(curves());
        assert_eq!(curve.perceived(0), 0.0);
        assert_eq!(curve.perceived(curve.panel_max.get()), 1.0);
    }

    // Log percentages are the rounded share of the panel maximum, capped at 100.
    #[hegel::test]
    fn percent_is_the_rounded_share(tc: TestCase) {
        let curve = tc.draw(curves());
        let raw = tc.draw(generators::integers::<u32>());
        let panel = u64::from(curve.panel_max.get());
        let expected = ((u64::from(raw) * 100 + panel / 2) / panel).min(100);
        assert_eq!(u64::from(curve.percent(raw)), expected);
    }
}
