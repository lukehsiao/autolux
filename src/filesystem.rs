//! Read-only view of the filesystem, which is how sysfs exposes sensors and
//! panels. This is the lowest layer: [`FileSystem::create`] forwards to
//! `std::fs`, [`FileSystem::create_null`] serves configured contents without
//! touching the disk. Everything above it (detection, parsing, control) runs
//! for real in both modes.

use std::{
    cell::RefCell,
    collections::{BTreeSet, HashMap},
    io,
    path::{Path, PathBuf},
};

use crate::nullable::Responses;

pub struct FileSystem {
    fs: Box<dyn Fs>,
}

impl FileSystem {
    #[must_use]
    pub fn create() -> Self {
        Self {
            fs: Box::new(RealFs),
        }
    }

    /// An empty filesystem: every read fails with [`io::ErrorKind::NotFound`].
    #[must_use]
    pub fn create_null() -> Self {
        Self::create_null_with(std::iter::empty::<(PathBuf, NulledFile)>())
    }

    /// A filesystem containing exactly `files`, plus the directories above
    /// them. Reading anything else fails with [`io::ErrorKind::NotFound`],
    /// as the real filesystem does for a missing sysfs attribute.
    pub fn create_null_with<P: Into<PathBuf>>(
        files: impl IntoIterator<Item = (P, NulledFile)>,
    ) -> Self {
        let files = files
            .into_iter()
            .map(|(path, contents)| (path.into(), contents.0))
            .collect();
        Self {
            fs: Box::new(StubbedFs {
                files: RefCell::new(files),
            }),
        }
    }

    /// The whole file, exactly as stored; sysfs attributes end in a newline.
    ///
    /// # Errors
    /// Any I/O error, including `NotFound` for a missing attribute.
    pub fn read_to_string(&self, path: &Path) -> io::Result<String> {
        self.fs.read_to_string(path)
    }

    /// Full paths of the direct children of `path`, in no particular order.
    ///
    /// # Errors
    /// Any I/O error, including `NotFound` when the directory does not exist.
    pub fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        self.fs.read_dir(path)
    }
}

/// Contents of one file in a nulled [`FileSystem`]. A plain string is
/// returned on every read; an array is returned one element per read.
pub struct NulledFile(Responses<Result<String, io::ErrorKind>>);

impl From<&str> for NulledFile {
    fn from(contents: &str) -> Self {
        Self(Responses::Always(Ok(contents.to_owned())))
    }
}

impl<const N: usize> From<[&str; N]> for NulledFile {
    fn from(contents: [&str; N]) -> Self {
        Self(Responses::<&str>::from(contents).map(|s| Ok(s.to_owned())))
    }
}

/// Per-read results, so a stub can fail some reads with the given error kind.
impl From<Responses<Result<String, io::ErrorKind>>> for NulledFile {
    fn from(responses: Responses<Result<String, io::ErrorKind>>) -> Self {
        Self(responses)
    }
}

/// Mirrors the `std::fs` calls the wrapper makes, so the real implementation
/// is pure forwarding.
trait Fs {
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
}

struct RealFs;

impl Fs for RealFs {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        std::fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect()
    }
}

struct StubbedFs {
    files: RefCell<HashMap<PathBuf, Responses<Result<String, io::ErrorKind>>>>,
}

impl Fs for StubbedFs {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        let mut files = self.files.borrow_mut();
        let Some(responses) = files.get_mut(path) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Nulled FileSystem: {} is not configured", path.display()),
            ));
        };
        responses
            .next(&format!("Nulled FileSystem {}", path.display()))
            .map_err(|kind| {
                io::Error::new(
                    kind,
                    format!(
                        "Nulled FileSystem: configured failure reading {}",
                        path.display()
                    ),
                )
            })
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        // A configured file implies every directory above it exists, so the
        // children of `path` are the ancestors of configured files whose
        // parent is `path`. The set dedupes siblings sharing a directory.
        let entries: BTreeSet<PathBuf> = self
            .files
            .borrow()
            .keys()
            .filter_map(|file| file.ancestors().find(|a| a.parent() == Some(path)))
            .map(Path::to_path_buf)
            .collect();
        if entries.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Nulled FileSystem: no directory {}", path.display()),
            ));
        }
        Ok(entries.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, io::ErrorKind, path::Path};

    use super::{FileSystem, NulledFile};
    use crate::nullable::Responses;

    // Narrow integration tests: these run against a real temporary directory
    // and document the std::fs behaviour the stub must match.
    mod real {
        use super::*;

        #[test]
        fn returns_contents_verbatim_including_trailing_newline() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("brightness"), "290\n").unwrap();
            let contents = FileSystem::create()
                .read_to_string(&dir.path().join("brightness"))
                .unwrap();
            assert_eq!(contents, "290\n");
        }

        #[test]
        fn missing_file_is_not_found() {
            let dir = tempfile::tempdir().unwrap();
            let err = FileSystem::create()
                .read_to_string(&dir.path().join("absent"))
                .unwrap_err();
            assert_eq!(err.kind(), ErrorKind::NotFound);
        }

        #[test]
        fn lists_direct_children_as_full_paths() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("iio:device7")).unwrap();
            std::fs::write(dir.path().join("iio:device7/name"), "als\n").unwrap();
            std::fs::write(dir.path().join("trigger0"), "").unwrap();
            let entries: BTreeSet<_> = FileSystem::create()
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
            let err = FileSystem::create()
                .read_dir(&dir.path().join("absent"))
                .unwrap_err();
            assert_eq!(err.kind(), ErrorKind::NotFound);
        }
    }

    mod nulled {
        use super::*;

        #[test]
        fn unconfigured_paths_are_not_found_like_the_real_filesystem() {
            let fs = FileSystem::create_null_with([("/sys/a", NulledFile::from("1"))]);
            let err = fs.read_to_string(Path::new("/sys/b")).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::NotFound);
            let err = fs.read_dir(Path::new("/proc")).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::NotFound);
        }

        #[test]
        fn configured_failures_surface_with_their_kind() {
            let fs = FileSystem::create_null_with([(
                "/sys/a",
                NulledFile::from(Responses::from([
                    Ok("1".to_owned()),
                    Err(ErrorKind::TimedOut),
                ])),
            )]);
            assert_eq!(fs.read_to_string(Path::new("/sys/a")).unwrap(), "1");
            let err = fs.read_to_string(Path::new("/sys/a")).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::TimedOut);
        }

        #[test]
        fn read_dir_lists_files_and_intermediate_directories() {
            let fs = FileSystem::create_null_with([
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
