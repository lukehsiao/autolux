# autolux

autolux sets a panel's brightness from the illuminance an ambient light sensor reports, and fades between brightnesses without visible steps.

## Language

### Brightness

**Panel**:
The display whose backlight autolux drives.
_Avoid_: screen, display, backlight (as a noun for the device)

**Raw brightness**:
A brightness in the panel driver's own units, from zero to the panel's maximum.
_Avoid_: level, value

**Percent**:
A raw brightness as a whole percentage of the panel's maximum, the scale users configure.

**Lightness**:
Where a raw brightness sits on a perceptually uniform scale, 0 at off and 1 at the panel's maximum.
_Avoid_: perceived brightness, perceptual scale, level

**Manual adjustment**:
A change to the panel's brightness made outside autolux; the next fade starts from it.
_Avoid_: override, user change

**Limits**:
The configured minimum and maximum percent; hard bounds the panel always ends up inside.
_Avoid_: range, bounds

### Light

**Illuminance**:
The ambient light falling on the sensor, in lux.
_Avoid_: light level, lux reading

**Sample**:
One illuminance read from the sensor.
_Avoid_: reading, measurement

**Outage**:
A continuous stretch with no readable sample.

### Behaviour

**Curve**:
The fixed mapping from illuminance to target.

**Target**:
The raw brightness the curve calls for at the current illuminance.
_Avoid_: goal, setpoint

**Fade**:
Moving the panel toward the target one frame at a time.
_Avoid_: transition, ramp

**Frame**:
One panel update during a fade, at the refresh rate of a 60 Hz display.
_Avoid_: tick

**Settled**:
Not fading: the panel is inside the limits and within the deadband of the target.
_Avoid_: idle

**Wake**:
The change from settled to fading.

**Deadband**:
How far the target may drift from a settled panel before a wake; about one just-noticeable difference in lightness.
_Avoid_: threshold, hysteresis

**Write quantum**:
The smallest change in lightness autolux writes to the panel during a fade; the write that lands on the target is exempt.
_Avoid_: minimum step, write threshold
