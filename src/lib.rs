pub mod args;
pub mod backlight;
pub mod clock;
pub mod controller;
pub mod curve;
pub mod error;
pub mod filesystem;
pub mod logind;
pub mod nullable;
pub mod sensor;
pub mod smoother;

use crate::{
    args::Args, backlight::Backlight, clock::Clock, controller::Controller, curve::Limits,
    error::Result, sensor::Sensor,
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
    let mut controller = Controller::start(sensor, backlight, Clock::create(), limits)?;
    loop {
        controller.step()?;
    }
}
