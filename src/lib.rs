pub mod args;
mod backlight;
mod clock;
mod controller;
mod curve;
pub mod error;
mod logind;
mod nullable;
mod sensor;
mod sysfs;

use std::convert::Infallible;

use tracing::info;

use crate::{
    args::Args,
    backlight::Backlight,
    clock::Clock,
    controller::{Controller, Fatal, Input, Output},
    curve::{Curve, Limits},
    error::{Error, Result},
    sensor::Sensor,
};

/// Adjusts the backlight until something goes wrong.
///
/// # Errors
/// Invalid limits (checked before touching any hardware), no usable sensor
/// or panel, or a failure the control loop cannot ride out.
pub fn run(args: &Args) -> Result<()> {
    let limits = Limits::new(args.min, args.max)?;
    let backlight = Backlight::detect()?;
    let sensor = Sensor::detect()?;
    match drive(&sensor, &backlight, &Clock::create(), limits)? {}
}

/// Runs the [`Controller`] against the hardware: does what it asks for and
/// passes the result back in.
fn drive(
    sensor: &Sensor,
    backlight: &Backlight,
    clock: &Clock,
    limits: Limits,
) -> Result<Infallible> {
    info!(
        sensor = %sensor.device().display(),
        backlight = backlight.name(),
        max_brightness = backlight.max(),
        "starting",
    );
    let mut controller = Controller::new(Curve::new(limits, backlight.max()));
    let mut input = Input::Time;
    loop {
        input = match controller.step(clock.now(), input)? {
            Output::Sample => Input::Sample(sensor.lux()),
            Output::ReadPanel => Input::Panel(backlight.brightness()?),
            Output::Write(raw) => {
                backlight.set(raw)?;
                Input::Time
            }
            Output::SleepUntil(deadline) => {
                clock.sleep_until(deadline);
                Input::Time
            }
        };
    }
}

impl From<Fatal<Error>> for Error {
    fn from(fatal: Fatal<Error>) -> Self {
        match fatal {
            Fatal::Startup(err) => err,
            Fatal::Outage { outage, source } => Self::SensorUnavailable {
                outage,
                source: Box::new(source),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::{
        backlight::{Backlight, NulledBacklight},
        clock::Clock,
        controller::{MAX_SENSOR_OUTAGE, SETTLED_SAMPLE_INTERVAL},
        curve::Limits,
        drive,
        error::Error,
        nullable::{ConfigurableResponses, OutputTracker},
        sensor::{NulledReading, Sensor},
    };

    /// A Surface Go 3 panel.
    const MAX_BRIGHTNESS: u32 = 7500;
    /// 2% of [`MAX_BRIGHTNESS`], worked out by hand.
    const MIN_RAW: u32 = 150;
    /// Below the curve's dark end, so the target is exactly the minimum.
    const DARK: NulledReading = NulledReading::Lux(1.0);
    /// Above the curve's bright end, so the target is exactly the maximum.
    const BRIGHT: NulledReading = NulledReading::Lux(2_000.0);
    /// How many unreadable samples in a row make an outage: a settled panel
    /// samples once per [`SETTLED_SAMPLE_INTERVAL`].
    fn failures_to_outage() -> usize {
        usize::try_from(MAX_SENSOR_OUTAGE.as_nanos() / SETTLED_SAMPLE_INTERVAL.as_nanos()).unwrap()
    }

    struct Setup {
        lux: ConfigurableResponses<NulledReading>,
        brightness: ConfigurableResponses<u32>,
        refusal: Option<&'static str>,
    }

    struct Ran {
        err: Error,
        writes: OutputTracker<u32>,
        sleeps: OutputTracker<Duration>,
    }

    /// Drives nulled hardware until the loop fails.
    fn run(setup: Setup) -> Ran {
        let sensor = Sensor::create_null_with(setup.lux);
        let backlight = Backlight::create_null_with(NulledBacklight {
            max_brightness: MAX_BRIGHTNESS,
            brightness: setup.brightness,
            refusal: setup.refusal.map(str::to_owned),
        });
        let writes = backlight.track_writes();
        let clock = Clock::create_null();
        let sleeps = clock.track_sleeps();
        let Err(err) = drive(&sensor, &backlight, &clock, Limits::new(2, 75).unwrap());
        Ran {
            err,
            writes,
            sleeps,
        }
    }

    /// `readable` samples, then a sensor that stays unreadable.
    fn outage_after(readable: usize) -> ConfigurableResponses<NulledReading> {
        [
            vec![DARK; readable],
            vec![NulledReading::Unreadable; failures_to_outage()],
        ]
        .concat()
        .into()
    }

    #[test]
    fn an_unreadable_sensor_fails_startup() {
        let ran = run(Setup {
            lux: NulledReading::Unreadable.into(),
            brightness: MIN_RAW.into(),
            refusal: None,
        });
        assert!(matches!(ran.err, Error::Read { .. }), "{:?}", ran.err);
    }

    #[test]
    fn a_refused_write_stops_the_loop() {
        let ran = run(Setup {
            lux: BRIGHT.into(),
            brightness: MIN_RAW.into(),
            refusal: Some("Your session has no seat, refusing."),
        });
        assert!(
            matches!(ran.err, Error::NotPermitted { .. }),
            "{:?}",
            ran.err
        );
        assert_eq!(
            ran.writes.data().len(),
            1,
            "should stop at the first refusal"
        );
    }

    #[test]
    fn an_outage_becomes_sensor_unavailable() {
        let ran = run(Setup {
            lux: outage_after(1),
            brightness: MIN_RAW.into(),
            refusal: None,
        });
        assert!(
            matches!(&ran.err, Error::SensorUnavailable { outage, source }
                if *outage == MAX_SENSOR_OUTAGE
                    && matches!(**source, Error::Read { .. })),
            "{:?}",
            ran.err
        );
    }

    #[test]
    fn a_settled_panel_is_left_alone_and_checked_every_sample() {
        let readable = 6;
        let ran = run(Setup {
            lux: outage_after(readable),
            // One read, at startup: re-reading while settled would exhaust it.
            brightness: [MIN_RAW].into(),
            refusal: None,
        });
        assert!(
            matches!(ran.err, Error::SensorUnavailable { .. }),
            "{:?}",
            ran.err
        );
        assert_eq!(ran.writes.data(), [], "should not write");
        // One settled sleep comes before each sample but the first.
        assert_eq!(
            ran.sleeps.data(),
            vec![SETTLED_SAMPLE_INTERVAL; readable - 1 + failures_to_outage()]
        );
    }
}
