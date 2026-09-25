//! Setting a backlight through systemd-logind.
//!
//! sysfs brightness files are root-only, but logind's `SetBrightness` lets
//! the owner of a seated session change them, so autolux can run as a plain
//! user service without a udev rule. [`Logind::create`] talks to the real
//! system bus; [`Logind::create_null`] accepts every request without I/O.

use std::cell::RefCell;

use crate::{
    error::{Error, Result},
    nullable::{OutputListener, OutputTracker},
};

/// One `SetBrightness` call, as sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrightnessRequest {
    pub subsystem: String,
    pub name: String,
    pub brightness: u32,
}

pub struct Logind {
    session: Box<dyn Session>,
    requests: OutputListener<BrightnessRequest>,
}

impl Logind {
    /// Does no I/O; the connection is made by [`Logind::connect`] or the
    /// first request.
    #[must_use]
    pub fn create() -> Self {
        Self::new(Box::new(RealSession {
            proxy: RefCell::new(None),
        }))
    }

    /// Accepts every request.
    #[must_use]
    pub fn create_null() -> Self {
        Self::new(Box::new(StubbedSession { refusal: None }))
    }

    /// Refuses every request with `reason`, the way logind refuses callers
    /// outside a seated session.
    #[must_use]
    pub fn create_null_refusing(reason: &str) -> Self {
        Self::new(Box::new(StubbedSession {
            refusal: Some(reason.to_owned()),
        }))
    }

    fn new(session: Box<dyn Session>) -> Self {
        Self {
            session,
            requests: OutputListener::new(),
        }
    }

    /// Connects to the system bus now rather than at the first request, so
    /// an unreachable bus fails at startup. Permission is still checked per
    /// request, by logind.
    ///
    /// # Errors
    /// [`Error::Logind`] when the system bus is unreachable.
    pub fn connect(&self) -> Result<()> {
        self.session
            .connect()
            .map_err(|reason| Error::Logind { reason })
    }

    /// Sets the `name` device of `subsystem` (`"backlight"` for panels) to
    /// the raw value `brightness`.
    ///
    /// # Errors
    /// [`Error::Logind`] when logind refuses or the bus fails.
    pub fn set_brightness(&self, subsystem: &str, name: &str, brightness: u32) -> Result<()> {
        self.requests.emit(&BrightnessRequest {
            subsystem: subsystem.to_owned(),
            name: name.to_owned(),
            brightness,
        });
        self.session
            .set_brightness(subsystem, name, brightness)
            .map_err(|reason| Error::Logind { reason })
    }

    #[must_use]
    pub fn track_requests(&self) -> OutputTracker<BrightnessRequest> {
        self.requests.track()
    }
}

/// Mirrors the bus connection and the one `org.freedesktop.login1.Session`
/// method autolux calls.
trait Session {
    fn connect(&self) -> Result<(), String>;
    fn set_brightness(&self, subsystem: &str, name: &str, brightness: u32) -> Result<(), String>;
}

struct RealSession {
    proxy: RefCell<Option<zbus::blocking::Proxy<'static>>>,
}

impl Session for RealSession {
    fn connect(&self) -> Result<(), String> {
        if self.proxy.borrow().is_some() {
            return Ok(());
        }
        let proxy = (|| {
            let connection = zbus::blocking::Connection::system()?;
            zbus::blocking::proxy::Builder::new(&connection)
                .destination("org.freedesktop.login1")?
                // `auto` is the caller's own session, or for a systemd user
                // service (which runs outside any session) the user's
                // graphical one.
                .path("/org/freedesktop/login1/session/auto")?
                .interface("org.freedesktop.login1.Session")?
                // Caching would subscribe to PropertiesChanged and wake
                // autolux for every idle-hint flip, for properties it never
                // reads.
                .cache_properties(zbus::proxy::CacheProperties::No)
                .build()
        })()
        .map_err(|e: zbus::Error| e.to_string())?;
        *self.proxy.borrow_mut() = Some(proxy);
        Ok(())
    }

    fn set_brightness(&self, subsystem: &str, name: &str, brightness: u32) -> Result<(), String> {
        self.connect()?;
        let proxy = self.proxy.borrow();
        let proxy = proxy.as_ref().ok_or("not connected to the system bus")?;
        proxy
            .call::<_, _, ()>("SetBrightness", &(subsystem, name, brightness))
            .map_err(|e| e.to_string())
    }
}

struct StubbedSession {
    refusal: Option<String>,
}

impl Session for StubbedSession {
    fn connect(&self) -> Result<(), String> {
        Ok(())
    }

    fn set_brightness(&self, _: &str, _: &str, _: u32) -> Result<(), String> {
        self.refusal.clone().map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::{BrightnessRequest, Logind};
    use crate::error::Error;

    // Narrow integration test against the real logind. It needs a seated
    // session and it writes the panel's current brightness back, so it is
    // opt-in: run it from a terminal inside the graphical session with
    // `cargo test -- --ignored`.
    #[test]
    #[ignore = "needs a seated graphical session and a backlight"]
    fn real_logind_accepts_a_seated_session() {
        let dir = std::fs::read_dir("/sys/class/backlight")
            .unwrap()
            .next()
            .expect("no backlight to test with")
            .unwrap();
        let name = dir.file_name().into_string().unwrap();
        let current: u32 = std::fs::read_to_string(dir.path().join("brightness"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let logind = Logind::create();
        logind.connect().unwrap();
        logind.set_brightness("backlight", &name, current).unwrap();
    }

    #[test]
    fn nulled_logind_tracks_every_request() {
        let logind = Logind::create_null();
        let requests = logind.track_requests();
        logind.connect().unwrap();
        logind
            .set_brightness("backlight", "intel_backlight", 290)
            .unwrap();
        assert_eq!(
            requests.data(),
            [BrightnessRequest {
                subsystem: "backlight".to_owned(),
                name: "intel_backlight".to_owned(),
                brightness: 290,
            }]
        );
    }

    #[test]
    fn nulled_refusal_surfaces_as_a_logind_error_and_is_still_tracked() {
        let logind = Logind::create_null_refusing("Your session has no seat, refusing.");
        let requests = logind.track_requests();
        let err = logind
            .set_brightness("backlight", "intel_backlight", 1)
            .unwrap_err();
        assert!(
            matches!(&err, Error::Logind { reason } if reason == "Your session has no seat, refusing."),
            "{err:?}"
        );
        assert_eq!(requests.data().len(), 1);
    }
}
