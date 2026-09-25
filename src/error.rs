use std::{path::PathBuf, result};

use miette::Diagnostic;
use thiserror::Error;

pub(crate) type Result<T, E = Error> = result::Result<T, E>;

#[derive(Error, Debug, Diagnostic)]
pub enum Error {
    #[error("{percent}% is not a valid brightness; use 0 through 100")]
    #[diagnostic(code(autolux::invalid_percent))]
    InvalidPercent { percent: u8 },
    #[error("--min ({min}%) must not exceed --max ({max}%)")]
    #[diagnostic(code(autolux::invalid_range))]
    InvalidRange { min: u8, max: u8 },
    #[error("no ambient light sensor found under {}", dir.display())]
    #[diagnostic(
        code(autolux::no_sensor),
        help(
            "autolux looks for an IIO device with an in_illuminance_input or in_illuminance_raw \
             channel. On laptops and tablets the hid_sensor_als or acpi_als module provides one."
        )
    )]
    NoSensor { dir: PathBuf },
    #[error("no backlight found under {}", dir.display())]
    #[diagnostic(
        code(autolux::no_backlight),
        help("only panels with a kernel-controllable backlight can be adjusted")
    )]
    NoBacklight { dir: PathBuf },
    #[error("failed to read {}", path.display())]
    #[diagnostic(code(autolux::read_error))]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} contains {contents:?}, expected {expected}", path.display())]
    #[diagnostic(code(autolux::parse_error))]
    Parse {
        path: PathBuf,
        contents: String,
        expected: &'static str,
    },
    #[error("logind refused to set the backlight: {reason}")]
    #[diagnostic(
        code(autolux::logind_error),
        help(
            "SetBrightness only works for the owner of a seated session. Run autolux as a systemd \
             user service or from a terminal inside the graphical session, not over SSH."
        )
    )]
    Logind { reason: String },
    #[error("the ambient light sensor has been unreadable for {outage:?}")]
    #[diagnostic(
        code(autolux::sensor_unavailable),
        help("exiting so the service manager restarts autolux, which finds the sensor afresh")
    )]
    SensorUnavailable {
        outage: std::time::Duration,
        #[source]
        source: Box<Error>,
    },
}
