//! The control loop: sample the light every [`SAMPLE_INTERVAL`], and while
//! the panel needs to move, update it every [`FRAME`].

use std::time::Duration;

use tracing::{debug, info, warn};

use crate::{
    backlight::Backlight,
    clock::Clock,
    curve::{Curve, Limits},
    error::{Error, Result},
    sensor::Sensor,
    smoother::{FRAME, Smoother, Step},
};

/// How often to read the light sensor while the panel is settled.
///
/// Reading a HID ambient light sensor is a synchronous round trip to the
/// sensor hub, about 10 ms and 200 µs of CPU on a Surface Go 3, which makes
/// it nearly all of what an idle autolux costs. Once a second keeps that
/// around 0.02% of one core, and bounds how long a light switch goes unseen.
pub const SETTLED_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// How often to read the light sensor while fading, so the target follows
/// changing light closely instead of in once-a-second jumps.
pub const FADING_SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// How long the sensor may stay unreadable before autolux exits.
///
/// Ten seconds rides out a sensor hub resuming from suspend. Beyond that the
/// device has probably been renumbered, and a restart re-detects it.
const MAX_SENSOR_OUTAGE: Duration = Duration::from_secs(10);

pub struct Controller {
    sensor: Sensor,
    backlight: Backlight,
    clock: Clock,
    smoother: Smoother,
    /// Most recent good reading, for logs.
    lux: f64,
    until_sample: Duration,
    /// How long the previous step slept; the next advance covers it.
    slept: Duration,
    /// Time since the sensor last read successfully.
    since_good_read: Duration,
}

impl Controller {
    /// Reads the light and the panel once, so a broken sensor or panel fails
    /// at startup rather than at the first fade.
    ///
    /// # Errors
    /// Any error reading the sensor or the panel.
    pub fn start(
        sensor: Sensor,
        backlight: Backlight,
        clock: Clock,
        limits: Limits,
    ) -> Result<Self> {
        let curve = Curve::new(limits, backlight.max());
        let lux = sensor.lux()?;
        let brightness = backlight.brightness()?;
        info!(
            sensor = %sensor.device().display(),
            backlight = backlight.name(),
            max_brightness = backlight.max(),
            "starting at {lux:.1} lux, {}%",
            curve.percent(brightness),
        );
        Ok(Self {
            sensor,
            backlight,
            clock,
            smoother: Smoother::new(curve, brightness, lux),
            lux,
            // The first step reacts to the startup reading before sampling again.
            until_sample: SETTLED_SAMPLE_INTERVAL,
            slept: Duration::ZERO,
            since_good_read: Duration::ZERO,
        })
    }

    /// One pass of the loop: sample if due, act, then sleep until there is
    /// something to do.
    ///
    /// # Errors
    /// A write the panel refused, an unreadable panel, or a sensor that has
    /// been unreadable for [`MAX_SENSOR_OUTAGE`].
    pub fn step(&mut self) -> Result<()> {
        if self.until_sample.is_zero() {
            self.sample()?;
            self.until_sample = if self.smoother.is_fading() {
                FADING_SAMPLE_INTERVAL
            } else {
                SETTLED_SAMPLE_INTERVAL
            };
        }
        let pause = match self.smoother.advance(self.slept) {
            Step::Settled => self.until_sample,
            Step::Wake { target } => {
                let brightness = self.backlight.brightness()?;
                let curve = self.smoother.curve();
                info!(
                    "{:.1} lux: fading from {}% to {}%",
                    self.lux,
                    curve.percent(brightness),
                    curve.percent(target),
                );
                self.smoother.resync(brightness);
                self.until_sample = self.until_sample.min(FADING_SAMPLE_INTERVAL);
                FRAME
            }
            Step::Fade(write) => {
                if let Some(raw) = write {
                    debug!(raw, "set");
                    self.backlight.set(raw)?;
                }
                FRAME
            }
        }
        .min(self.until_sample);
        self.clock.sleep(pause);
        self.until_sample -= pause;
        self.slept = pause;
        self.since_good_read += pause;
        Ok(())
    }

    fn sample(&mut self) -> Result<()> {
        match self.sensor.lux() {
            Ok(lux) => {
                self.since_good_read = Duration::ZERO;
                self.lux = lux;
                self.smoother.observe(lux);
                Ok(())
            }
            Err(err) if self.since_good_read >= MAX_SENSOR_OUTAGE => {
                Err(Error::SensorUnavailable {
                    outage: self.since_good_read,
                    source: Box::new(err),
                })
            }
            Err(err) => {
                warn!("skipping a light sample: {}", chain(&err));
                Ok(())
            }
        }
    }
}

/// `err` and its causes, joined, for one-line logs.
fn chain(err: &dyn std::error::Error) -> String {
    std::iter::successors(Some(err), |e| e.source())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Controller, FRAME, MAX_SENSOR_OUTAGE, SETTLED_SAMPLE_INTERVAL};
    use crate::{
        backlight::{Backlight, NulledBacklight},
        clock::Clock,
        curve::Limits,
        error::Error,
        nullable::{OutputTracker, Responses},
        sensor::{NulledReading, Sensor},
    };

    /// A Surface Go 3 panel.
    const MAX_BRIGHTNESS: u32 = 7500;
    /// Raw values of 2% and 75% of [`MAX_BRIGHTNESS`], worked out by hand.
    const MIN_RAW: u32 = 150;
    const MAX_RAW: u32 = 5625;
    /// Below the curve's dark end, so the target is exactly the minimum.
    const DARK: NulledReading = NulledReading::Lux(1.0);
    /// Above the curve's bright end, so the target is exactly the maximum.
    const BRIGHT: NulledReading = NulledReading::Lux(2_000.0);
    const IRRELEVANT_LUX: NulledReading = NulledReading::Lux(123.0);
    const IRRELEVANT_BRIGHTNESS: u32 = 1234;

    struct Setup {
        lux: Responses<NulledReading>,
        brightness: Responses<u32>,
        refusal: Option<&'static str>,
    }

    impl Default for Setup {
        fn default() -> Self {
            Self {
                lux: IRRELEVANT_LUX.into(),
                brightness: IRRELEVANT_BRIGHTNESS.into(),
                refusal: None,
            }
        }
    }

    struct Harness {
        controller: Controller,
        writes: OutputTracker<u32>,
        sleeps: OutputTracker<Duration>,
    }

    fn start(setup: Setup) -> crate::error::Result<Harness> {
        let sensor = Sensor::create_null_with(setup.lux);
        let backlight = Backlight::create_null_with(NulledBacklight {
            max_brightness: MAX_BRIGHTNESS,
            brightness: setup.brightness,
            refusal: setup.refusal.map(str::to_owned),
        });
        let writes = backlight.track_writes();
        let clock = Clock::create_null();
        let sleeps = clock.track_sleeps();
        let controller = Controller::start(sensor, backlight, clock, Limits::new(2, 75).unwrap())?;
        Ok(Harness {
            controller,
            writes,
            sleeps,
        })
    }

    #[test]
    fn a_settled_panel_is_left_alone_and_checked_every_sample() {
        let mut harness = start(Setup {
            lux: DARK.into(),
            // One read, at startup: re-reading while settled would exhaust it.
            brightness: [MIN_RAW].into(),
            ..Setup::default()
        })
        .unwrap();
        for _ in 0..5 {
            harness.controller.step().unwrap();
        }
        assert_eq!(harness.writes.data(), [], "should not write");
        assert_eq!(harness.sleeps.data(), [SETTLED_SAMPLE_INTERVAL; 5]);
    }

    #[test]
    fn fades_frame_by_frame_to_exactly_the_target_then_settles() {
        let mut harness = start(Setup {
            lux: BRIGHT.into(),
            brightness: MIN_RAW.into(),
            ..Setup::default()
        })
        .unwrap();
        let mut elapsed = Duration::ZERO;
        // A whole settled interval of sleep only happens once the fade is over.
        while harness.sleeps.data().last() != Some(&SETTLED_SAMPLE_INTERVAL) {
            for slept in harness.sleeps.clear() {
                assert!(slept <= FRAME, "slept {slept:?} mid-fade");
                elapsed += slept;
            }
            assert!(elapsed < Duration::from_secs(6), "fade did not finish");
            harness.controller.step().unwrap();
        }
        let writes = harness.writes.clear();
        assert_eq!(writes.last(), Some(&MAX_RAW));
        assert!(writes.windows(2).all(|w| w[0] < w[1]), "{writes:?}");
        assert!(
            writes.len() > 100,
            "should take many small steps, took {}",
            writes.len()
        );
        let _ = harness.sleeps.clear();
        for _ in 0..3 {
            harness.controller.step().unwrap();
        }
        assert_eq!(harness.writes.data(), [], "should stay settled");
        assert_eq!(harness.sleeps.data(), [SETTLED_SAMPLE_INTERVAL; 3]);
    }

    #[test]
    fn a_fade_starts_from_wherever_someone_left_the_panel() {
        let mut harness = start(Setup {
            // Dark at startup and for the first sample; then the lights come
            // on for the second.
            lux: Responses::from([vec![DARK; 2], vec![BRIGHT; 30]].concat()),
            // Settled at the minimum, then someone turns it up by hand.
            brightness: [MIN_RAW, 3000].into(),
            ..Setup::default()
        })
        .unwrap();
        // Only the first write matters; a later wake would read the panel again.
        for _ in 0..100 {
            if !harness.writes.data().is_empty() {
                break;
            }
            harness.controller.step().unwrap();
        }
        let first = *harness.writes.data().first().expect("never wrote");
        assert!(
            (2500..3000).contains(&first),
            "should start near 3000, where the panel was, but wrote {first}"
        );
    }

    #[test]
    fn unreadable_samples_are_skipped_until_the_sensor_has_been_out_too_long() {
        // A settled panel samples once per SETTLED_SAMPLE_INTERVAL.
        let failures =
            usize::try_from(MAX_SENSOR_OUTAGE.as_millis() / SETTLED_SAMPLE_INTERVAL.as_millis())
                .unwrap();
        let lux = [
            vec![DARK],
            vec![NulledReading::Unreadable; failures - 1],
            vec![DARK],
            vec![NulledReading::Unreadable; failures],
        ]
        .concat();
        let mut harness = start(Setup {
            lux: lux.into(),
            brightness: MIN_RAW.into(),
            ..Setup::default()
        })
        .unwrap();
        // The first step reacts to the startup reading without sampling.
        harness.controller.step().unwrap();
        for sample in 1..2 * failures {
            harness
                .controller
                .step()
                .unwrap_or_else(|e| panic!("sample {sample} failed early: {e:?}"));
        }
        let err = harness.controller.step().unwrap_err();
        assert!(
            matches!(&err, Error::SensorUnavailable { outage, source }
                if *outage == MAX_SENSOR_OUTAGE && matches!(**source, Error::Read { .. })),
            "{err:?}"
        );
        assert_eq!(harness.writes.data(), [], "should not write");
    }

    #[test]
    fn a_refused_write_stops_the_loop() {
        let mut harness = start(Setup {
            lux: BRIGHT.into(),
            brightness: MIN_RAW.into(),
            refusal: Some("Your session has no seat, refusing."),
        })
        .unwrap();
        harness.controller.step().unwrap();
        let err = harness.controller.step().unwrap_err();
        assert!(matches!(err, Error::Logind { .. }), "{err:?}");
        assert_eq!(
            harness.writes.data().len(),
            1,
            "should stop at the first refusal"
        );
    }

    #[test]
    fn an_unreadable_sensor_fails_startup() {
        let err = start(Setup {
            lux: NulledReading::Unreadable.into(),
            ..Setup::default()
        })
        .err()
        .unwrap();
        assert!(matches!(err, Error::Read { .. }), "{err:?}");
    }
}
