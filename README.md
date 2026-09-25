<h1 align="center">
    🔆<br>
    autolux
</h1>
<div align="center">
    <strong>Adaptive backlight daemon driven by an ambient light sensor.</strong>
</div>
<br>
<div align="center">
  <a href="https://github.com/lukehsiao/autolux/actions/workflows/general.yml">
    <img src="https://img.shields.io/github/actions/workflow/status/lukehsiao/autolux/general.yml" alt="Build Status">
  </a>
  <a href="https://crates.io/crates/autolux">
    <img src="https://img.shields.io/crates/v/autolux" alt="Version">
  </a>
  <a href="https://github.com/lukehsiao/autolux/blob/main/LICENSE.md">
    <img src="https://img.shields.io/crates/l/autolux" alt="License">
  </a>
</div>
<br>

`autolux` sets a Linux laptop or tablet's screen brightness from its ambient light sensor.
It follows a logarithmic curve between a minimum and a maximum you choose, and fades between levels continuously rather than in visible steps.
It finds the sensor and the panel on its own, needs no root, and sleeps almost all the time.

## Install
### Cargo
```
cargo install --locked autolux
```

Or, if you use [`cargo-binstall`](https://github.com/cargo-bins/cargo-binstall):

```
cargo binstall autolux
```

### mise
[mise](https://mise.jdx.dev/) can install a prebuilt binary directly from the GitHub release:

```
mise use -g github:lukehsiao/autolux
```

This puts the `autolux` binary on your `PATH`.

## Usage
```
Adaptive backlight daemon driven by an ambient light sensor

Usage: autolux [OPTIONS]

Options:
      --min <PERCENT>  Brightness in a dark room, as a percentage of the panel's maximum [env:
                       AUTOLUX_MIN=] [default: 5]
      --max <PERCENT>  Brightness in a bright room, as a percentage of the panel's maximum [env:
                       AUTOLUX_MAX=] [default: 100]
  -v, --verbose...     Increase logging verbosity
  -q, --quiet...       Decrease logging verbosity
  -h, --help           Print help (see more with '--help')
  -V, --version        Print version
```

Percentages are of the panel's raw `max_brightness`, the same scale `brightnessctl` uses.

## Running as a systemd user service
`autolux` sets the backlight through systemd-logind's `SetBrightness`, which only the owner of a seated session may call.
Running it as a user service tied to the graphical session satisfies that without root or a udev rule.
Started over SSH instead, it fails with `Your session has no seat, refusing.`

Create `~/.config/systemd/user/autolux.service`:

```ini
[Unit]
Description=Adaptive backlight from the ambient light sensor
After=graphical-session.target
PartOf=graphical-session.target
ConditionPathExistsGlob=/sys/class/backlight/*
StartLimitIntervalSec=0

[Service]
Type=simple
Slice=background-graphical.slice
EnvironmentFile=-%h/.config/autolux.env
ExecStart=%h/.cargo/bin/autolux
Restart=always
RestartSec=5

[Install]
WantedBy=graphical-session.target
```

systemd does not search your `PATH`, so point `ExecStart` at wherever `autolux` lives: `%h/.cargo/bin/autolux` for Cargo, `%h/.local/share/mise/shims/autolux` for mise.

Put the limits in `~/.config/autolux.env`, so they can be changed without editing the unit:

```ini
AUTOLUX_MIN=2
AUTOLUX_MAX=75
```

Then start it, and watch what it does:

```
systemctl --user daemon-reload
systemctl --user enable --now autolux.service
journalctl --user -fu autolux.service
```

## How it works
### The curve
Perceived brightness is roughly logarithmic in illuminance, so the target brightness is linear in the logarithm of the room's light: every doubling of the light raises the panel by the same amount.
The panel sits at `--min` at or below 3 lux (a dark room) and reaches `--max` at 500 lux (office lighting, per EN 12464-1); daylight keeps it at `--max`.
Halfway, about 39 lux, is halfway between the limits.

### Fading
Two first-order filters run in series.
The first smooths the light level with a 2 s time constant, so a passing shadow barely registers but a light switch shows up within a fraction of a second.
The second eases the panel toward the curve's target with a 1 s time constant, on a perceptual scale (CIE lightness goes roughly as the cube root of luminance), so equal steps look equally large at any brightness.
Chained, they change brightness with continuous speed: a fade starts gently, runs, and slows to a stop.

While fading, the panel is updated at 60 Hz, and no single frame moves it by more than about 1.7% of the perceptual scale.
A fade across the whole range takes about five seconds.

### Staying out of the way
Once the panel reaches its target, `autolux` reads the sensor once a second and goes back to sleep; only while a fade runs does it sample four times a second.
Reading a HID ambient light sensor is a synchronous round trip to the sensor hub (about 10 ms and 200 µs of CPU on a Surface Go 3), so that is nearly all an idle `autolux` costs: 31 ms of CPU over two settled minutes there, about 0.03% of one core.
It starts a new fade only when the target drifts about one just-noticeable difference away, so sensor jitter causes no writes.
The limits are hard bounds, though: a panel it finds outside them, at startup or when a fade begins, is always brought back, even by an invisible amount.
If you change the brightness yourself, `autolux` leaves it until the room's light changes enough to start a new fade, which then begins from wherever you left the panel.
If the sensor stays unreadable for ten seconds, `autolux` exits so the service manager restarts it, which finds the sensor afresh.

### Finding the hardware
The sensor is the first IIO device under `/sys/bus/iio/devices` with an `in_illuminance_input` channel, or an `in_illuminance_raw` one scaled by its `_offset` and `_scale`.
The panel is the device under `/sys/class/backlight`, preferring firmware over platform over raw interfaces when there are several, as the kernel's sysfs ABI recommends.
