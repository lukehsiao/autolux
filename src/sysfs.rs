//! Read-only view of sysfs, where the kernel exposes sensors and panels. This
//! is the lowest layer: [`Sysfs::create`] forwards to `std::fs`, and the
//! nulled sysfs serves configured contents without touching the disk. It
//! owns reading attributes as typed values and mapping failures to
//! [`Error::Read`] and [`Error::Parse`]. Everything above it (detection,
//! control) runs for real in both modes.

use std::{
    io,
    path::{Path, PathBuf},
    str::FromStr,
};

use crate::error::{Error, Result};
#[cfg(test)]
use crate::nullable::ConfigurableResponses;

pub struct Sysfs {
    disk: Disk,
}

impl Sysfs {
    #[must_use]
    pub fn create() -> Self {
        Self { disk: Disk::Real }
    }

    /// The attribute at `path` as a `T`, ignoring surrounding whitespace such
    /// as the trailing newline sysfs adds. `expected` describes a valid value
    /// for the error message, e.g. "a whole number".
    ///
    /// # Errors
    /// [`Error::Read`] when the file is unreadable; [`Error::Parse`], with
    /// the file's real contents, when it does not hold a `T`.
    pub fn read<T: FromStr>(&self, path: &Path, expected: &'static str) -> Result<T> {
        let contents = self
            .disk
            .read_to_string(path)
            .map_err(|source| Error::Read {
                path: path.to_owned(),
                source,
            })?;
        contents.trim().parse().map_err(|_| Error::Parse {
            path: path.to_owned(),
            contents,
            expected,
        })
    }

    /// The devices under a class directory such as `/sys/class/backlight`,
    /// as full paths in name order. A missing directory means the kernel
    /// has no driver for the class loaded, so it holds no devices.
    ///
    /// # Errors
    /// [`Error::Read`] for any I/O error other than `NotFound`.
    pub fn devices(&self, class: &Path) -> Result<Vec<PathBuf>> {
        let mut devices = match self.disk.read_dir(class) {
            Ok(devices) => devices,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(source) => {
                return Err(Error::Read {
                    path: class.to_owned(),
                    source,
                });
            }
        };
        devices.sort();
        Ok(devices)
    }

    /// Full paths of the direct children of `path`, in no particular order.
    ///
    /// # Errors
    /// Any I/O error, including `NotFound` when the directory does not exist.
    pub fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        self.disk.read_dir(path)
    }
}

#[cfg(test)]
impl Sysfs {
    /// A filesystem containing exactly `files`, plus the directories above
    /// them. Reading anything else fails with [`io::ErrorKind::NotFound`],
    /// as the real filesystem does for a missing sysfs attribute.
    pub fn create_null_with<P: Into<PathBuf>>(
        files: impl IntoIterator<Item = (P, NulledFile)>,
    ) -> Self {
        Self {
            disk: Disk::Stubbed(stub::Files::new(
                files
                    .into_iter()
                    .map(|(path, contents)| (path.into(), contents.0)),
            )),
        }
    }
}

/// Contents of one file in a nulled [`Sysfs`]. A plain string is
/// returned on every read; an array is returned one element per read.
#[cfg(test)]
pub struct NulledFile(ConfigurableResponses<Result<String, io::ErrorKind>>);

#[cfg(test)]
impl From<&str> for NulledFile {
    fn from(contents: &str) -> Self {
        Self(ConfigurableResponses::Repeat(Ok(contents.to_owned())))
    }
}

#[cfg(test)]
impl<const N: usize> From<[&str; N]> for NulledFile {
    fn from(contents: [&str; N]) -> Self {
        Self(ConfigurableResponses::<&str>::from(contents).map(|s| Ok(s.to_owned())))
    }
}

/// Per-read results, so a stub can fail some reads with the given error kind.
#[cfg(test)]
impl From<ConfigurableResponses<Result<String, io::ErrorKind>>> for NulledFile {
    fn from(responses: ConfigurableResponses<Result<String, io::ErrorKind>>) -> Self {
        Self(responses)
    }
}

// The seam. Each arm mirrors only the std::fs calls this wrapper makes, and
// the real arm only forwards to std::fs.
enum Disk {
    Real,
    #[cfg(test)]
    Stubbed(stub::Files),
}

impl Disk {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        match self {
            Self::Real => std::fs::read_to_string(path),
            #[cfg(test)]
            Self::Stubbed(files) => files.read_to_string(path),
        }
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        match self {
            Self::Real => std::fs::read_dir(path)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect(),
            #[cfg(test)]
            Self::Stubbed(files) => files.read_dir(path),
        }
    }
}

#[cfg(test)]
mod stub {
    use std::{
        collections::{BTreeSet, HashMap},
        io,
        path::{Path, PathBuf},
        sync::{Arc, Mutex, PoisonError},
    };

    use crate::nullable::ConfigurableResponses;

    type Answers = ConfigurableResponses<Result<String, io::ErrorKind>>;

    pub(super) struct Files(Arc<Mutex<HashMap<PathBuf, Answers>>>);

    impl Files {
        pub(super) fn new(files: impl IntoIterator<Item = (PathBuf, Answers)>) -> Self {
            Self(Arc::new(Mutex::new(files.into_iter().collect())))
        }

        pub(super) fn read_to_string(&self, path: &Path) -> io::Result<String> {
            let mut files = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(responses) = files.get_mut(path) else {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Nulled Sysfs: {} is not configured", path.display()),
                ));
            };
            responses
                .next(&format!("Nulled Sysfs {}", path.display()))
                .map_err(|kind| {
                    io::Error::new(
                        kind,
                        format!(
                            "Nulled Sysfs: configured failure reading {}",
                            path.display()
                        ),
                    )
                })
        }

        pub(super) fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
            // A configured file implies every directory above it exists, so
            // the children of `path` are the ancestors of configured files
            // whose parent is `path`. The set dedupes siblings sharing a
            // directory.
            let entries: BTreeSet<PathBuf> = self
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .keys()
                .filter_map(|file| file.ancestors().find(|a| a.parent() == Some(path)))
                .map(Path::to_path_buf)
                .collect();
            if entries.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Nulled Sysfs: no directory {}", path.display()),
                ));
            }
            Ok(entries.into_iter().collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, io::ErrorKind, num::NonZeroU32, path::Path};

    use super::{NulledFile, Sysfs};
    use crate::{error::Error, nullable::ConfigurableResponses};

    fn read_error_kind(err: &Error) -> Option<ErrorKind> {
        match err {
            Error::Read { source, .. } => Some(source.kind()),
            _ => None,
        }
    }

    // Narrow integration tests: these run against a real temporary directory
    // and document the std::fs behaviour the stub must match.
    mod real {
        use super::*;

        #[test]
        fn reads_an_attribute_ignoring_the_trailing_newline() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("brightness"), "290\n").unwrap();
            let raw: u32 = Sysfs::create()
                .read(&dir.path().join("brightness"), "a whole number")
                .unwrap();
            assert_eq!(raw, 290);
        }

        #[test]
        fn missing_file_is_a_read_error_naming_it() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("absent");
            let err = Sysfs::create()
                .read::<u32>(&path, "a whole number")
                .unwrap_err();
            assert_eq!(read_error_kind(&err), Some(ErrorKind::NotFound), "{err:?}");
            assert!(matches!(&err, Error::Read { path: p, .. } if *p == path));
        }

        #[test]
        fn missing_class_directory_has_no_devices() {
            let dir = tempfile::tempdir().unwrap();
            let devices = Sysfs::create().devices(&dir.path().join("absent")).unwrap();
            assert_eq!(devices, Vec::<std::path::PathBuf>::new());
        }

        #[test]
        fn lists_direct_children_as_full_paths() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("iio:device7")).unwrap();
            std::fs::write(dir.path().join("iio:device7/name"), "als\n").unwrap();
            std::fs::write(dir.path().join("trigger0"), "").unwrap();
            let entries: BTreeSet<_> = Sysfs::create()
                .read_dir(dir.path())
                .unwrap()
                .into_iter()
                .collect();
            let expected: BTreeSet<_> =
                [dir.path().join("iio:device7"), dir.path().join("trigger0")].into();
            assert_eq!(entries, expected);
        }

        #[test]
        fn missing_directory_is_not_found() {
            let dir = tempfile::tempdir().unwrap();
            let err = Sysfs::create()
                .read_dir(&dir.path().join("absent"))
                .unwrap_err();
            assert_eq!(err.kind(), ErrorKind::NotFound);
        }
    }

    mod nulled {
        use super::*;

        #[test]
        fn unconfigured_paths_are_not_found_like_the_real_filesystem() {
            let fs = Sysfs::create_null_with([("/sys/a", NulledFile::from("1"))]);
            let err = fs
                .read::<u32>(Path::new("/sys/b"), "a whole number")
                .unwrap_err();
            assert_eq!(read_error_kind(&err), Some(ErrorKind::NotFound), "{err:?}");
            let err = fs.read_dir(Path::new("/proc")).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::NotFound);
        }

        #[test]
        fn configured_failures_surface_as_read_errors_with_their_kind() {
            let fs = Sysfs::create_null_with([(
                "/sys/a",
                NulledFile::from(ConfigurableResponses::from([
                    Ok("1".to_owned()),
                    Err(ErrorKind::TimedOut),
                ])),
            )]);
            assert_eq!(fs.read::<u32>(Path::new("/sys/a"), "1").unwrap(), 1);
            let err = fs.read::<u32>(Path::new("/sys/a"), "1").unwrap_err();
            assert_eq!(read_error_kind(&err), Some(ErrorKind::TimedOut), "{err:?}");
        }

        #[test]
        fn unparsable_contents_are_a_parse_error_quoting_the_real_file() {
            let fs = Sysfs::create_null_with([("/sys/max_brightness", NulledFile::from("0\n"))]);
            let err = fs
                .read::<NonZeroU32>(Path::new("/sys/max_brightness"), "a positive whole number")
                .unwrap_err();
            assert!(
                matches!(&err, Error::Parse { path, contents, expected: "a positive whole number" }
                    if path == Path::new("/sys/max_brightness") && contents == "0\n"),
                "{err:?}"
            );
        }

        #[test]
        fn devices_are_in_name_order_and_a_missing_class_is_empty() {
            let fs = Sysfs::create_null_with([
                ("/sys/class/backlight/zeta/type", NulledFile::from("raw\n")),
                ("/sys/class/backlight/alpha/type", NulledFile::from("raw\n")),
            ]);
            assert_eq!(
                fs.devices(Path::new("/sys/class/backlight")).unwrap(),
                [
                    Path::new("/sys/class/backlight/alpha"),
                    Path::new("/sys/class/backlight/zeta"),
                ]
            );
            assert_eq!(
                fs.devices(Path::new("/sys/bus/iio/devices")).unwrap(),
                Vec::<std::path::PathBuf>::new()
            );
        }

        #[test]
        fn read_dir_lists_files_and_intermediate_directories() {
            let fs = Sysfs::create_null_with([
                (
                    "/sys/bus/iio/devices/iio:device7/name",
                    NulledFile::from("als"),
                ),
                (
                    "/sys/bus/iio/devices/iio:device7/in_illuminance_raw",
                    NulledFile::from(["1"]),
                ),
                (
                    "/sys/bus/iio/devices/iio:device0/name",
                    NulledFile::from("accel"),
                ),
            ]);
            assert_eq!(
                fs.read_dir(Path::new("/sys/bus/iio/devices")).unwrap(),
                [
                    Path::new("/sys/bus/iio/devices/iio:device0"),
                    Path::new("/sys/bus/iio/devices/iio:device7"),
                ]
            );
            assert_eq!(
                fs.read_dir(Path::new("/sys/bus/iio/devices/iio:device7"))
                    .unwrap(),
                [
                    Path::new("/sys/bus/iio/devices/iio:device7/in_illuminance_raw"),
                    Path::new("/sys/bus/iio/devices/iio:device7/name"),
                ]
            );
        }
    }
}
