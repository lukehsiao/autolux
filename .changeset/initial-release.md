---
"autolux": minor
---

**feat**: initial release of an adaptive backlight daemon. autolux reads an IIO ambient light sensor, maps illuminance to brightness along a logarithmic curve bounded by `--min` and `--max` percentages, and fades the panel continuously at 60 Hz rather than stepping. It writes brightness through systemd-logind, so it runs unprivileged as a systemd user service.
