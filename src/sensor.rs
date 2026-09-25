//! The ambient light sensor, found through the kernel's IIO subsystem.

use std::{
    io,
    path::{Path, PathBuf},
};

use crate::{
    error::{Error, Result},
    filesystem::{FileSystem, NulledFile},
    nullable::Responses,
};

const IIO_DEVICES: &str = "/sys/bus/iio/devices";

/// Illuminance channel prefixes, in the order they are tried. Most drivers
/// expose one unnumbered channel; a few number it.
const CHANNELS: [&str; 2] = ["in_illuminance_", "in_illuminance0_"];

/// The scale the nulled sensor reports raw counts at, the same as the HID
/// sensor in a Surface Go 3.
const NULLED_SCALE: f64 = 0.001;

pub struct Sensor {
    fs: FileSystem,
    device: PathBuf,
    reading: Reading,
}

enum Reading {
    /// The driver reports lux directly.
    Processed(PathBuf),
    /// Lux is `(raw + offset) * scale`. Offset and scale are fixed for a
    /// device, so they are read once.
    Raw {
        path: PathBuf,
        offset: f64,
        scale: f64,
    },
}

/// What one read of a nulled sensor returns.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NulledReading {
    Lux(f64),
    /// The read fails, as it can while a sensor hub resumes from suspend.
    Unreadable,
}

impl Sensor {
    /// Finds the first IIO device, in name order, with an illuminance channel.
    ///
    /// # Errors
    /// [`Error::NoSensor`] when no device has one; [`Error::Read`] or
    /// [`Error::Parse`] when the chosen device's scale or offset is unusable.
    pub fn detect() -> Result<Self> {
        Self::detect_in(FileSystem::create())
    }

    fn detect_in(fs: FileSystem) -> Result<Self> {
        let root = Path::new(IIO_DEVICES);
        let mut devices = match fs.read_dir(root) {
            Ok(devices) => devices,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(source) => {
                return Err(Error::Read {
                    path: root.to_owned(),
                    source,
                });
            }
        };
        devices.sort();
        for device in devices {
            // Triggers and anything that vanishes mid-scan are not sensors.
            let Ok(attributes) = fs.read_dir(&device) else {
                continue;
            };
            let attribute = |prefix: &str, suffix: &str| {
                let path = device.join(format!("{prefix}{suffix}"));
                attributes.contains(&path).then_some(path)
            };
            for prefix in CHANNELS {
                if let Some(path) = attribute(prefix, "input") {
                    return Ok(Self {
                        fs,
                        device,
                        reading: Reading::Processed(path),
                    });
                }
                if let Some(path) = attribute(prefix, "raw") {
                    let constant = |suffix, default| {
                        attribute(prefix, suffix)
                            .map_or(Ok(default), |path| read_number(&fs, &path))
                    };
                    let reading = Reading::Raw {
                        path,
                        offset: constant("offset", 0.0)?,
                        scale: constant("scale", 1.0)?,
                    };
                    return Ok(Self {
                        fs,
                        device,
                        reading,
                    });
                }
            }
        }
        Err(Error::NoSensor {
            dir: root.to_owned(),
        })
    }

    /// A sensor in a room at a steady, oddly specific 1234.5 lux, so a test
    /// that leans on it by accident stands out.
    #[must_use]
    pub fn create_null() -> Self {
        Self::create_null_with(NulledReading::Lux(1234.5))
    }

    /// A sensor laid out like a Surface Go 3's (raw counts at a fixed scale)
    /// that reports `readings`: one value repeats, an array is consumed.
    ///
    /// # Panics
    /// Never: the layout always has an illuminance channel.
    pub fn create_null_with(readings: impl Into<Responses<NulledReading>>) -> Self {
        let dir = format!("{IIO_DEVICES}/iio:device0");
        let raw = readings.into().map(|reading| match reading {
            NulledReading::Lux(lux) => Ok(format!("{}\n", (lux / NULLED_SCALE).round())),
            NulledReading::Unreadable => Err(io::ErrorKind::Other),
        });
        let fs = FileSystem::create_null_with([
            (format!("{dir}/name"), NulledFile::from("als\n")),
            (format!("{dir}/in_illuminance_raw"), NulledFile::from(raw)),
            (
                format!("{dir}/in_illuminance_scale"),
                NulledFile::from("0.001000000\n"),
            ),
            (
                format!("{dir}/in_illuminance_offset"),
                NulledFile::from("0\n"),
            ),
        ]);
        Self::detect_in(fs).expect("the nulled layout has an illuminance channel")
    }

    /// The IIO device in use, for logs.
    #[must_use]
    pub fn device(&self) -> &Path {
        &self.device
    }

    /// Current illuminance in lux.
    ///
    /// # Errors
    /// [`Error::Read`] or [`Error::Parse`] when the channel is unreadable.
    pub fn lux(&self) -> Result<f64> {
        match &self.reading {
            Reading::Processed(path) => read_number(&self.fs, path),
            Reading::Raw {
                path,
                offset,
                scale,
            } => Ok((read_number(&self.fs, path)? + offset) * scale),
        }
    }
}

fn read_number(fs: &FileSystem, path: &Path) -> Result<f64> {
    let contents = fs.read_to_string(path).map_err(|source| Error::Read {
        path: path.to_owned(),
        source,
    })?;
    contents.trim().parse().map_err(|_| Error::Parse {
        path: path.to_owned(),
        contents,
        expected: "a number",
    })
}

#[cfg(test)]
#[expect(
    clippy::float_cmp,
    reason = "the expected values are exact in binary floating point"
)]
mod tests {
    use std::path::Path;

    use hegel::{TestCase, generators};

    use super::{NulledReading, Sensor};
    use crate::{
        error::Error,
        filesystem::{FileSystem, NulledFile},
    };

    const ALS: &str = "/sys/bus/iio/devices/iio:device7";

    fn detect(files: &[(&str, &str)]) -> crate::error::Result<Sensor> {
        Sensor::detect_in(FileSystem::create_null_with(
            files
                .iter()
                .map(|(path, contents)| (*path, NulledFile::from(*contents))),
        ))
    }

    #[test]
    fn uses_the_device_with_an_illuminance_channel() {
        let sensor = detect(&[
            ("/sys/bus/iio/devices/iio:device0/in_accel_x_raw", "12\n"),
            ("/sys/bus/iio/devices/trigger0/name", "als-dev7\n"),
            (
                "/sys/bus/iio/devices/iio:device7/in_illuminance_raw",
                "2410\n",
            ),
            (
                "/sys/bus/iio/devices/iio:device7/in_illuminance_scale",
                "0.001000000\n",
            ),
        ])
        .unwrap();
        assert_eq!(sensor.device(), Path::new(ALS));
        assert!((sensor.lux().unwrap() - 2.41).abs() < 1e-9);
    }

    #[test]
    fn applies_offset_before_scale() {
        let sensor = detect(&[
            (
                "/sys/bus/iio/devices/iio:device7/in_illuminance_raw",
                "2410\n",
            ),
            (
                "/sys/bus/iio/devices/iio:device7/in_illuminance_offset",
                "-10\n",
            ),
            (
                "/sys/bus/iio/devices/iio:device7/in_illuminance_scale",
                "0.5\n",
            ),
        ])
        .unwrap();
        assert_eq!(sensor.lux().unwrap(), 1200.0);
    }

    #[test]
    fn raw_without_scale_or_offset_is_already_lux() {
        let sensor =
            detect(&[("/sys/bus/iio/devices/iio:device7/in_illuminance_raw", "5\n")]).unwrap();
        assert_eq!(sensor.lux().unwrap(), 5.0);
    }

    #[test]
    fn prefers_the_processed_value() {
        let sensor = detect(&[
            (
                "/sys/bus/iio/devices/iio:device7/in_illuminance_input",
                "123.4\n",
            ),
            ("/sys/bus/iio/devices/iio:device7/in_illuminance_raw", "1\n"),
        ])
        .unwrap();
        assert_eq!(sensor.lux().unwrap(), 123.4);
    }

    #[test]
    fn accepts_a_numbered_channel() {
        let sensor = detect(&[(
            "/sys/bus/iio/devices/iio:device2/in_illuminance0_input",
            "88\n",
        )])
        .unwrap();
        assert_eq!(sensor.lux().unwrap(), 88.0);
    }

    #[test]
    fn no_illuminance_channel_is_no_sensor() {
        let err = detect(&[("/sys/bus/iio/devices/iio:device0/in_accel_x_raw", "12\n")])
            .err()
            .unwrap();
        assert!(matches!(err, Error::NoSensor { .. }), "{err:?}");
        let err = detect(&[("/sys/class/backlight/intel_backlight/brightness", "1\n")])
            .err()
            .unwrap();
        assert!(matches!(err, Error::NoSensor { .. }), "{err:?}");
    }

    #[test]
    fn garbage_is_a_parse_error_naming_the_file_and_contents() {
        let sensor = detect(&[(
            "/sys/bus/iio/devices/iio:device7/in_illuminance_raw",
            "\u{0}bogus\n",
        )])
        .unwrap();
        let err = sensor.lux().unwrap_err();
        assert!(
            matches!(
                &err,
                Error::Parse { path, contents, .. }
                    if path == Path::new(ALS).join("in_illuminance_raw").as_path() && contents == "\u{0}bogus\n"
            ),
            "{err:?}"
        );
    }

    #[test]
    fn unusable_scale_fails_detection() {
        let err = detect(&[
            ("/sys/bus/iio/devices/iio:device7/in_illuminance_raw", "1\n"),
            (
                "/sys/bus/iio/devices/iio:device7/in_illuminance_scale",
                "\n",
            ),
        ])
        .err()
        .unwrap();
        assert!(matches!(err, Error::Parse { .. }), "{err:?}");
    }

    #[test]
    fn bare_nulled_sensor_reports_a_loud_default_forever() {
        let sensor = Sensor::create_null();
        for _ in 0..3 {
            assert!((sensor.lux().unwrap() - 1234.5).abs() < 1e-9);
        }
    }

    #[test]
    fn nulled_sensor_reports_configured_lux_and_failures_in_order() {
        let sensor =
            Sensor::create_null_with([NulledReading::Lux(72.3), NulledReading::Unreadable]);
        assert!((sensor.lux().unwrap() - 72.3).abs() < 1e-9);
        assert!(matches!(sensor.lux().unwrap_err(), Error::Read { .. }));
    }

    // Whatever integers and nano-precision scale the kernel prints, lux is
    // (raw + offset) * scale, surrounding whitespace notwithstanding.
    #[hegel::test]
    fn lux_is_raw_plus_offset_times_scale(tc: TestCase) {
        let raw = tc.draw(generators::integers::<i32>());
        let offset = tc.draw(generators::integers::<i32>());
        // IIO prints INT_PLUS_NANO scales as "<int>.<9 digits>".
        let nanos = tc.draw(
            generators::integers::<u64>()
                .min_value(1)
                .max_value(10_000_000_000_000),
        );
        let scale = format!("{}.{:09}\n", nanos / 1_000_000_000, nanos % 1_000_000_000);
        let sensor = detect(&[
            (
                "/sys/bus/iio/devices/iio:device7/in_illuminance_raw",
                &format!("{raw}\n"),
            ),
            (
                "/sys/bus/iio/devices/iio:device7/in_illuminance_offset",
                &format!(" {offset}\n"),
            ),
            (
                "/sys/bus/iio/devices/iio:device7/in_illuminance_scale",
                &scale,
            ),
        ])
        .unwrap();
        #[expect(
            clippy::cast_precision_loss,
            reason = "nanos stays below 2^53, so this is exact"
        )]
        let expected = (f64::from(raw) + f64::from(offset)) * (nanos as f64 / 1e9);
        let lux = sensor.lux().unwrap();
        // Parsing the decimal scale and dividing nanos by 1e9 may round
        // differently in the last binary place. A relative 1e-12 allows that,
        // and is still finer than a wrong last digit for any scale below 1000.
        assert!(
            (lux - expected).abs() <= expected.abs() * 1e-12,
            "{raw} + {offset} at {scale:?} gave {lux}, expected {expected}"
        );
    }
}
