//! Setting a backlight through systemd-logind.
//!
//! sysfs brightness files are root-only, but logind's `SetBrightness` lets
//! the owner of a seated session change them, so autolux can run as a plain
//! user service without a udev rule. [`Logind::create`] talks to the real
//! system bus; the nulled logind answers without I/O.

use std::cell::RefCell;

use crate::error::{Error, Result};

/// logind's reply when the caller's session has no seat, is not in the
/// foreground, or does not own the device.
const NOT_YOUR_DEVICE: &str = "org.freedesktop.login1.NotYourDevice";
/// logind's reply when the caller does not own the session.
const ACCESS_DENIED: &str = "org.freedesktop.DBus.Error.AccessDenied";

pub struct Logind {
    bus: Bus,
    /// The session, connected on first use.
    session: RefCell<Option<Session>>,
}

impl Logind {
    /// Does no I/O; the connection is made by [`Logind::connect`] or the
    /// first request.
    #[must_use]
    pub fn create() -> Self {
        Self::with_bus(Bus::Real)
    }

    fn with_bus(bus: Bus) -> Self {
        Self {
            bus,
            session: RefCell::new(None),
        }
    }

    /// Connects to the system bus now rather than at the first request, so
    /// an unreachable bus fails at startup. Permission is still checked per
    /// request, by logind.
    ///
    /// # Errors
    /// [`Error::Bus`] when the system bus is unreachable.
    pub fn connect(&self) -> Result<()> {
        self.with_session(|_| Ok(()))
    }

    /// Sets the backlight `name` to the raw value `brightness`.
    ///
    /// # Errors
    /// [`Error::NotPermitted`] when logind refuses this session,
    /// [`Error::Logind`] when logind fails otherwise, and [`Error::Bus`]
    /// when the bus fails.
    pub fn set_brightness(&self, name: &str, brightness: u32) -> Result<()> {
        self.with_session(|session| session.set_brightness(name, brightness))
    }

    /// Runs `f` on the session, connecting first if this is the first use.
    fn with_session(&self, f: impl FnOnce(&Session) -> zbus::Result<()>) -> Result<()> {
        let mut slot = self.session.borrow_mut();
        let session = match slot.take() {
            Some(session) => session,
            None => self.bus.connect().map_err(classify)?,
        };
        f(slot.insert(session)).map_err(classify)
    }
}

#[cfg(test)]
impl Logind {
    /// Accepts every request.
    #[must_use]
    pub fn create_null() -> Self {
        Self::create_null_with(NulledLogind::default())
    }

    /// Refuses every request with `detail`, the way logind refuses callers
    /// outside a seated session.
    #[must_use]
    pub fn create_null_refusing(detail: &str) -> Self {
        Self::create_null_with(NulledLogind {
            reply: Some(method_error(NOT_YOUR_DEVICE, detail)),
            ..NulledLogind::default()
        })
    }

    #[must_use]
    pub fn create_null_with(logind: NulledLogind) -> Self {
        Self::with_bus(Bus::Stubbed(Box::new(logind)))
    }
}

/// How the nulled logind fails. `None` succeeds.
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub struct NulledLogind {
    pub connect: Option<zbus::Error>,
    pub reply: Option<zbus::Error>,
    /// The panels logind can find. A write to any other name fails as the
    /// real logind fails it. `None` finds every name.
    pub panels: Option<Vec<String>>,
}

/// The error zbus returns when logind answers with the D-Bus error `name`.
#[cfg(test)]
pub fn method_error(name: &str, detail: &str) -> zbus::Error {
    let call = zbus::Message::method_call("/org/freedesktop/login1/session/auto", "SetBrightness")
        .expect("the path and member are valid")
        .build(&())
        .expect("an empty body is valid");
    zbus::Message::error(&call.header(), name)
        .expect("the error name is valid")
        .build(&detail)
        .expect("a string body is valid")
        .into()
}

// logind's own name for the error, or the bus failure behind it. Only the
// D-Bus error name tells a refusal apart from other failures; the detail is
// free text.
fn classify(error: zbus::Error) -> Error {
    match error {
        zbus::Error::MethodError(name, detail, _) => {
            let detail = detail.unwrap_or_default();
            match name.as_str() {
                NOT_YOUR_DEVICE | ACCESS_DENIED => Error::NotPermitted { detail },
                _ => Error::Logind {
                    name: name.to_string(),
                    detail,
                },
            }
        }
        source @ (zbus::Error::Connection(..)
        | zbus::Error::InputOutput(_)
        | zbus::Error::Handshake(_)) => Error::Bus { source },
        // The bus works but the exchange went wrong, as with a reply that
        // does not decode. The bus help would mislead here.
        other => Error::Logind {
            name: "zbus".to_owned(),
            detail: other.to_string(),
        },
    }
}

// The seams. Each arm mirrors the bus connection or the one
// `org.freedesktop.login1.Session` method autolux calls.
enum Bus {
    Real,
    #[cfg(test)]
    Stubbed(Box<NulledLogind>),
}

enum Session {
    Real(zbus::blocking::Proxy<'static>),
    #[cfg(test)]
    Stubbed(Box<NulledLogind>),
}

impl Bus {
    fn connect(&self) -> zbus::Result<Session> {
        match self {
            Self::Real => connect().map(Session::Real),
            #[cfg(test)]
            Self::Stubbed(nulled) => match &nulled.connect {
                Some(err) => Err(err.clone()),
                None => Ok(Session::Stubbed(nulled.clone())),
            },
        }
    }
}

impl Session {
    fn set_brightness(&self, name: &str, brightness: u32) -> zbus::Result<()> {
        match self {
            Self::Real(proxy) => {
                proxy.call::<_, _, ()>("SetBrightness", &("backlight", name, brightness))
            }
            #[cfg(test)]
            Self::Stubbed(nulled) => match (&nulled.reply, &nulled.panels) {
                (Some(reply), _) => Err(reply.clone()),
                (None, Some(panels)) if !panels.iter().any(|panel| panel == name) => {
                    Err(method_error(
                        "System.Error.ENODEV",
                        &format!("Failed to open device backlight:{name}: No such device"),
                    ))
                }
                (None, _) => Ok(()),
            },
        }
    }
}

fn connect() -> zbus::Result<zbus::blocking::Proxy<'static>> {
    let connection = zbus::blocking::Connection::system()?;
    zbus::blocking::proxy::Builder::new(&connection)
        .destination("org.freedesktop.login1")?
        // `auto` is the caller's own session, or for a systemd user
        // service (which runs outside any session) the user's graphical
        // one.
        .path("/org/freedesktop/login1/session/auto")?
        .interface("org.freedesktop.login1.Session")?
        // Caching would subscribe to PropertiesChanged and wake autolux
        // for every idle-hint flip, for properties it never reads.
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
}

#[cfg(test)]
mod tests {
    use std::{io, sync::Arc};

    use super::{Logind, NulledLogind, method_error};
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
        logind.set_brightness(&name, current).unwrap();
    }

    fn replying(reply: zbus::Error) -> Logind {
        Logind::create_null_with(NulledLogind {
            reply: Some(reply),
            ..NulledLogind::default()
        })
    }

    #[test]
    fn nulled_logind_accepts_by_default() {
        let logind = Logind::create_null();
        logind.connect().unwrap();
        logind.set_brightness("intel_backlight", 290).unwrap();
    }

    #[test]
    fn a_panel_logind_cannot_find_is_no_such_device() {
        let logind = Logind::create_null_with(NulledLogind {
            panels: Some(vec!["intel_backlight".to_owned()]),
            ..NulledLogind::default()
        });
        logind.set_brightness("intel_backlight", 1).unwrap();
        let err = logind.set_brightness("acpi_video0", 1).unwrap_err();
        assert!(
            matches!(&err, Error::Logind { name, detail }
                if name == "System.Error.ENODEV"
                    && detail == "Failed to open device backlight:acpi_video0: No such device"),
            "{err:?}"
        );
    }

    #[test]
    fn an_unreachable_bus_fails_at_connect() {
        let address = "unix:path=/run/dbus/system_bus_socket".parse().unwrap();
        let unreachable =
            zbus::Error::Connection(Arc::new(io::Error::from(io::ErrorKind::NotFound)), address);
        let logind = Logind::create_null_with(NulledLogind {
            connect: Some(unreachable),
            ..NulledLogind::default()
        });
        let err = logind.connect().unwrap_err();
        assert!(matches!(err, Error::Bus { .. }), "{err:?}");
        let err = logind.set_brightness("intel_backlight", 1).unwrap_err();
        assert!(matches!(err, Error::Bus { .. }), "{err:?}");
    }

    #[test]
    fn a_lost_bus_is_a_bus_error() {
        let err = replying(zbus::Error::InputOutput(Arc::new(io::Error::from(
            io::ErrorKind::BrokenPipe,
        ))))
        .set_brightness("intel_backlight", 1)
        .unwrap_err();
        assert!(matches!(err, Error::Bus { .. }), "{err:?}");
    }

    #[test]
    fn foreign_sessions_are_not_permitted() {
        for name in [
            "org.freedesktop.login1.NotYourDevice",
            "org.freedesktop.DBus.Error.AccessDenied",
        ] {
            let err = replying(method_error(name, "refusing."))
                .set_brightness("intel_backlight", 1)
                .unwrap_err();
            assert!(
                matches!(&err, Error::NotPermitted { detail } if detail == "refusing."),
                "{name}: {err:?}"
            );
        }
    }

    #[test]
    fn other_logind_errors_keep_their_name_and_detail() {
        let err = replying(method_error(
            "System.Error.ENODEV",
            "Failed to open device backlight:nope: No such device",
        ))
        .set_brightness("nope", 1)
        .unwrap_err();
        assert!(
            matches!(&err, Error::Logind { name, detail }
                if name == "System.Error.ENODEV"
                    && detail == "Failed to open device backlight:nope: No such device"),
            "{err:?}"
        );
    }

    #[test]
    fn other_zbus_errors_are_not_bus_errors() {
        for reply in [
            zbus::Error::InvalidReply,
            zbus::Error::Failure("odd".into()),
        ] {
            let detail = reply.to_string();
            let err = replying(reply)
                .set_brightness("intel_backlight", 1)
                .unwrap_err();
            assert!(
                matches!(&err, Error::Logind { detail: shown, .. } if *shown == detail),
                "{err:?}"
            );
        }
    }
}
