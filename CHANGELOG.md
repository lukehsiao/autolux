# Changelog

## 0.1.0

### Minor Changes

- [`be8aedc`](https://github.com/lukehsiao/autolux/commit/be8aedcca81be99969c76c9057830104d6bb98d7) - **feat**: initial release of an adaptive backlight daemon. autolux reads an IIO ambient light sensor, maps illuminance to brightness along a logarithmic curve bounded by `--min` and `--max` percentages, and fades the panel continuously at 60 Hz rather than stepping. It writes brightness through systemd-logind, so it runs unprivileged as a systemd user service.
