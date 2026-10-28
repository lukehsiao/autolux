//! The panel's backlight: read through sysfs, written through logind.

use std::{
    num::NonZeroU32,
    path::{Path, PathBuf},
};

use crate::{
    error::{Error, Result},
    logind::Logind,
    nullable::OutputListener,
    sysfs::Sysfs,
};
#[cfg(test)]
use crate::{
    nullable::{ConfigurableResponses, OutputTracker},
    sysfs::NulledFile,
};

const BACKLIGHTS: &str = "/sys/class/backlight";

pub struct Backlight {
    fs: Sysfs,
    logind: Logind,
    dir: PathBuf,
    name: String,
    max: NonZeroU32,
    writes: OutputListener<u32>,
}

/// The panel a nulled [`Backlight`] presents. The defaults are deliberately
/// odd numbers, so a test that leans on one by accident stands out.
#[cfg(test)]
#[derive(Debug)]
pub struct NulledBacklight {
    pub max_brightness: u32,
    /// What successive reads of `brightness` return.
    pub brightness: ConfigurableResponses<u32>,
    /// When set, logind refuses every write with this reason.
    pub refusal: Option<String>,
}

#[cfg(test)]
impl Default for NulledBacklight {
    fn default() -> Self {
        Self {
            max_brightness: 4321,
            brightness: ConfigurableResponses::Repeat(1234),
            refusal: None,
        }
    }
}

impl Backlight {
    /// Picks the panel under `/sys/class/backlight` and connects to logind,
    /// so a missing panel or an unreachable bus fails at startup.
    ///
    /// With several panels, prefers firmware over platform over raw
    /// interfaces, as the kernel's sysfs ABI for backlights recommends, then
    /// name order.
    ///
    /// # Errors
    /// [`Error::NoBacklight`] when there is none; [`Error::Read`] or
    /// [`Error::Parse`] when the chosen panel's `max_brightness` is unusable;
    /// [`Error::Logind`] when the system bus is unreachable.
    pub fn detect() -> Result<Self> {
        Self::detect_in(Sysfs::create(), Logind::create())
    }

    fn detect_in(fs: Sysfs, logind: Logind) -> Result<Self> {
        let root = Path::new(BACKLIGHTS);
        let preference = |dir: &PathBuf| {
            // A missing or odd `type` only lowers the panel's rank.
            let kind: String = fs
                .read(&dir.join("type"), "a backlight type")
                .unwrap_or_default();
            match kind.as_str() {
                "firmware" => 0,
                "platform" => 1,
                "raw" => 2,
                _ => 3,
            }
        };
        let Some(dir) = fs
            .devices(root)?
            .into_iter()
            .min_by_key(|dir| (preference(dir), dir.clone()))
        else {
            return Err(Error::NoBacklight {
                dir: root.to_owned(),
            });
        };
        let max = fs.read(&dir.join("max_brightness"), "a positive whole number")?;
        let name = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        logind.connect()?;
        Ok(Self {
            fs,
            logind,
            dir,
            name,
            max,
            writes: OutputListener::default(),
        })
    }

    /// The device name, as logind and logs know it.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The panel's `max_brightness`, in raw driver units.
    #[must_use]
    pub fn max(&self) -> NonZeroU32 {
        self.max
    }

    /// The panel's current raw brightness.
    ///
    /// # Errors
    /// [`Error::Read`] or [`Error::Parse`].
    pub fn brightness(&self) -> Result<u32> {
        self.fs.read(&self.dir.join("brightness"), "a whole number")
    }

    /// Sets the panel to raw brightness `raw`.
    ///
    /// # Errors
    /// [`Error::Logind`] when logind refuses.
    pub fn set(&self, raw: u32) -> Result<()> {
        self.writes.emit(|| raw);
        self.logind.set_brightness("backlight", &self.name, raw)
    }
}

#[cfg(test)]
impl Backlight {
    #[must_use]
    pub fn create_null() -> Self {
        Self::create_null_with(NulledBacklight::default())
    }

    /// An `intel_backlight` panel as `panel` describes it.
    ///
    /// # Panics
    /// Never: the layout always has a backlight.
    #[must_use]
    pub fn create_null_with(panel: NulledBacklight) -> Self {
        let NulledBacklight {
            max_brightness,
            brightness,
            refusal,
        } = panel;
        let dir = format!("{BACKLIGHTS}/intel_backlight");
        let fs = Sysfs::create_null_with([
            (format!("{dir}/type"), NulledFile::from("raw\n")),
            (
                format!("{dir}/max_brightness"),
                NulledFile::from(format!("{max_brightness}\n").as_str()),
            ),
            (
                format!("{dir}/brightness"),
                NulledFile::from(brightness.map(|raw| Ok(format!("{raw}\n")))),
            ),
        ]);
        let logind = refusal.map_or_else(Logind::create_null, |reason| {
            Logind::create_null_refusing(&reason)
        });
        Self::detect_in(fs, logind).expect("the nulled layout has a backlight")
    }

    #[must_use]
    pub fn track_writes(&self) -> OutputTracker<u32> {
        self.writes.track()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Backlight, NulledBacklight};
    use crate::{
        error::Error,
        logind::{BrightnessRequest, Logind},
        sysfs::{NulledFile, Sysfs},
    };

    fn detect(files: &[(&str, &str)], logind: Logind) -> crate::error::Result<Backlight> {
        Backlight::detect_in(
            Sysfs::create_null_with(
                files
                    .iter()
                    .map(|(path, contents)| (*path, NulledFile::from(*contents))),
            ),
            logind,
        )
    }

    #[test]
    fn prefers_firmware_then_platform_then_raw_interfaces() {
        let panels = [
            ("/sys/class/backlight/a_raw/type", "raw\n"),
            ("/sys/class/backlight/a_raw/max_brightness", "10\n"),
            ("/sys/class/backlight/b_platform/type", "platform\n"),
            ("/sys/class/backlight/b_platform/max_brightness", "20\n"),
            ("/sys/class/backlight/c_firmware/type", "firmware\n"),
            ("/sys/class/backlight/c_firmware/max_brightness", "30\n"),
        ];
        let chosen = detect(&panels, Logind::create_null()).unwrap();
        assert_eq!((chosen.name(), chosen.max().get()), ("c_firmware", 30));
        let chosen = detect(&panels[..4], Logind::create_null()).unwrap();
        assert_eq!((chosen.name(), chosen.max().get()), ("b_platform", 20));
    }

    #[test]
    fn breaks_ties_by_name() {
        let chosen = detect(
            &[
                ("/sys/class/backlight/zeta/type", "raw\n"),
                ("/sys/class/backlight/zeta/max_brightness", "1\n"),
                ("/sys/class/backlight/alpha/type", "raw\n"),
                ("/sys/class/backlight/alpha/max_brightness", "2\n"),
            ],
            Logind::create_null(),
        )
        .unwrap();
        assert_eq!(chosen.name(), "alpha");
    }

    #[test]
    fn no_panel_is_no_backlight() {
        let err = detect(
            &[("/sys/bus/iio/devices/iio:device7/name", "als\n")],
            Logind::create_null(),
        )
        .err()
        .unwrap();
        assert!(matches!(err, Error::NoBacklight { .. }), "{err:?}");
    }

    #[test]
    fn zero_max_brightness_is_rejected() {
        let err = detect(
            &[("/sys/class/backlight/broken/max_brightness", "0\n")],
            Logind::create_null(),
        )
        .err()
        .unwrap();
        assert!(
            matches!(&err, Error::Parse { path, contents, expected: "a positive whole number" }
                if path == Path::new("/sys/class/backlight/broken/max_brightness")
                    && contents == "0\n"),
            "{err:?}"
        );
    }

    #[test]
    fn reads_the_current_brightness() {
        let backlight = detect(
            &[
                (
                    "/sys/class/backlight/intel_backlight/max_brightness",
                    "7500\n",
                ),
                ("/sys/class/backlight/intel_backlight/brightness", "290\n"),
            ],
            Logind::create_null(),
        )
        .unwrap();
        assert_eq!(backlight.brightness().unwrap(), 290);
    }

    #[test]
    fn writes_go_to_logind_for_the_chosen_device() {
        let logind = Logind::create_null();
        let requests = logind.track_requests();
        let backlight = detect(
            &[(
                "/sys/class/backlight/intel_backlight/max_brightness",
                "7500\n",
            )],
            logind,
        )
        .unwrap();
        let writes = backlight.track_writes();
        backlight.set(4321).unwrap();
        assert_eq!(writes.data(), [4321]);
        assert_eq!(
            requests.data(),
            [BrightnessRequest {
                subsystem: "backlight".to_owned(),
                name: "intel_backlight".to_owned(),
                brightness: 4321,
            }]
        );
    }

    #[test]
    fn refused_writes_are_logind_errors() {
        let backlight = Backlight::create_null_with(NulledBacklight {
            refusal: Some("no seat".to_owned()),
            ..NulledBacklight::default()
        });
        let err = backlight.set(1).unwrap_err();
        assert!(
            matches!(&err, Error::Logind { reason } if reason == "no seat"),
            "{err:?}"
        );
    }

    #[test]
    fn nulled_backlight_reads_configured_values_in_order() {
        let backlight = Backlight::create_null_with(NulledBacklight {
            max_brightness: 7500,
            brightness: [290, 4000].into(),
            refusal: None,
        });
        assert_eq!(backlight.max().get(), 7500);
        assert_eq!(backlight.brightness().unwrap(), 290);
        assert_eq!(backlight.brightness().unwrap(), 4000);
    }

    #[test]
    fn bare_nulled_backlight_works_with_loud_defaults() {
        let backlight = Backlight::create_null();
        assert_eq!(backlight.max().get(), 4321);
        assert_eq!(backlight.brightness().unwrap(), 1234);
        backlight.set(1).unwrap();
    }
}
