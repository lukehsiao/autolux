//! The control loop: sample the light every [`SAMPLE_INTERVAL`], and while
//! the panel needs to move, update it every [`FRAME`].

use std::time::{Duration, Instant};

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

/// How long the sensor may stay unreadable before autolux exits. This is
/// measured time, which does not count suspend.
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
    next_sample: Instant,
    /// The next frame deadline while fading. Frames sit on a grid anchored
    /// at the wake, so time spent writing shortens the next sleep instead
    /// of adding to it.
    next_frame: Instant,
    /// When the smoother last advanced.
    advanced: Instant,
    /// When the sensor last read successfully.
    good_read: Instant,
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
        let now = clock.now();
        Ok(Self {
            sensor,
            backlight,
            clock,
            smoother: Smoother::new(curve, brightness, lux),
            lux,
            // The first step reacts to the startup reading before sampling again.
            next_sample: now + SETTLED_SAMPLE_INTERVAL,
            next_frame: now,
            advanced: now,
            good_read: now,
        })
    }

    /// One pass of the loop: sample if due, advance the panel if it is
    /// settled or a frame is due, then sleep until the next deadline.
    ///
    /// # Errors
    /// A write the panel refused, an unreadable panel, or a sensor that has
    /// been unreadable for [`MAX_SENSOR_OUTAGE`].
    pub fn step(&mut self) -> Result<()> {
        let now = self.clock.now();
        if now >= self.next_sample {
            self.sample(now)?;
            let interval = if self.smoother.is_fading() {
                FADING_SAMPLE_INTERVAL
            } else {
                SETTLED_SAMPLE_INTERVAL
            };
            self.next_sample = next_deadline(self.next_sample, interval, now);
        }
        if !self.smoother.is_fading() || now >= self.next_frame {
            let elapsed = now.saturating_duration_since(self.advanced);
            self.advanced = now;
            match self.smoother.advance(elapsed) {
                Step::Settled => {}
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
                    self.next_frame = now + FRAME;
                    self.next_sample = self.next_sample.min(now + FADING_SAMPLE_INTERVAL);
                }
                Step::Fade(write) => {
                    if let Some(raw) = write {
                        debug!(raw, "set");
                        self.backlight.set(raw)?;
                    }
                    self.next_frame = next_deadline(self.next_frame, FRAME, now);
                }
            }
        }
        let deadline = if self.smoother.is_fading() {
            self.next_frame.min(self.next_sample)
        } else {
            self.next_sample
        };
        self.clock.sleep_until(deadline);
        Ok(())
    }

    fn sample(&mut self, now: Instant) -> Result<()> {
        match self.sensor.lux() {
            Ok(lux) => {
                self.good_read = now;
                self.lux = lux;
                self.smoother.observe(lux);
                Ok(())
            }
            Err(err) if now - self.good_read >= MAX_SENSOR_OUTAGE => {
                Err(Error::SensorUnavailable {
                    outage: now - self.good_read,
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

/// The deadline one `interval` after `deadline`. If that has passed too,
/// as after a stall, the missed deadlines are skipped and the next one is
/// one `interval` after `now`.
fn next_deadline(deadline: Instant, interval: Duration, now: Instant) -> Instant {
    let next = deadline + interval;
    if next > now { next } else { now + interval }
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
    use std::time::{Duration, Instant};

    use super::{Controller, FRAME, MAX_SENSOR_OUTAGE, SETTLED_SAMPLE_INTERVAL};
    use crate::{
        backlight::{Backlight, NulledBacklight},
        clock::Clock,
        curve::Limits,
        error::Error,
        nullable::{ConfigurableResponses, OutputTracker},
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
        lux: ConfigurableResponses<NulledReading>,
        brightness: ConfigurableResponses<u32>,
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
        /// Shares the controller's virtual time.
        clock: Clock,
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
        let controller = Controller::start(
            sensor,
            backlight,
            clock.clone(),
            Limits::new(2, 75).unwrap(),
        )?;
        Ok(Harness {
            controller,
            writes,
            sleeps,
            clock,
        })
    }

    /// When each write of one full-range fade happened, if every step
    /// spends `io` on I/O before it sleeps again.
    fn fade_write_times(io: Duration) -> Vec<Instant> {
        let mut harness = start(Setup {
            lux: BRIGHT.into(),
            brightness: MIN_RAW.into(),
            ..Setup::default()
        })
        .unwrap();
        let mut times = Vec::new();
        // A full-range fade takes about five seconds, or 300 frames.
        for _ in 0..1_000 {
            harness.clock.advance(io);
            let at = harness.clock.now();
            harness.controller.step().unwrap();
            let writes = harness.writes.clear();
            if !writes.is_empty() {
                times.push(at);
            }
            if writes.last() == Some(&MAX_RAW) {
                return times;
            }
        }
        panic!("fade did not finish");
    }

    #[test]
    fn time_spent_in_io_does_not_lengthen_the_frame_interval() {
        // About what one logind write costs on a Surface Go 3.
        let io = Duration::from_millis(2);
        let times = fade_write_times(io);
        let mut intervals: Vec<Duration> = times.windows(2).map(|w| w[1] - w[0]).collect();
        intervals.sort();
        assert_eq!(
            intervals[intervals.len() / 2],
            FRAME,
            "median write interval"
        );
        let with_io = *times.last().unwrap() - times[0];
        let without_io = {
            let times = fade_write_times(Duration::ZERO);
            *times.last().unwrap() - times[0]
        };
        assert!(
            with_io.abs_diff(without_io) <= FRAME,
            "fade took {with_io:?} with I/O, {without_io:?} without"
        );
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
        while harness.writes.data().last() != Some(&MAX_RAW) {
            for slept in harness.sleeps.clear() {
                assert!(slept <= FRAME, "slept {slept:?} mid-fade");
                elapsed += slept;
            }
            assert!(elapsed < Duration::from_secs(6), "fade did not finish");
            harness.controller.step().unwrap();
        }
        let writes = harness.writes.clear();
        assert!(writes.windows(2).all(|w| w[0] < w[1]), "{writes:?}");
        assert!(
            writes.len() > 100,
            "should take many small steps, took {}",
            writes.len()
        );
        // The next sample after landing picks the settled interval.
        harness.controller.step().unwrap();
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
            lux: ConfigurableResponses::from([vec![DARK; 2], vec![BRIGHT; 30]].concat()),
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
    fn the_outage_counts_measured_time_not_requested_sleeps() {
        let mut harness = start(Setup {
            lux: [
                DARK,
                NulledReading::Unreadable,
                NulledReading::Unreadable,
                NulledReading::Unreadable,
            ]
            .into(),
            brightness: MIN_RAW.into(),
            ..Setup::default()
        })
        .unwrap();
        // Each step hangs for 4 s before it samples. The next sample is due
        // 1 s after the previous one, so samples land 4 s, 9 s and 14 s after
        // the startup reading, worked out by hand.
        let hang = Duration::from_secs(4);
        for sample in 1..=2 {
            harness.clock.advance(hang);
            harness
                .controller
                .step()
                .unwrap_or_else(|e| panic!("sample {sample} failed early: {e:?}"));
        }
        harness.clock.advance(hang);
        let err = harness.controller.step().unwrap_err();
        assert!(
            matches!(&err, Error::SensorUnavailable { outage, .. }
                if *outage == Duration::from_secs(14)),
            "{err:?}"
        );
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
        assert!(matches!(err, Error::NotPermitted { .. }), "{err:?}");
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
