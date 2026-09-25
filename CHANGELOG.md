# Changelog

## 0.1.1

### Patch Changes

- [`c8be829`](https://github.com/lukehsiao/autolux/commit/c8be8298edfbeafa58f2e9d6b9dd19c6c81e83a2) - **chore**: publish releases to crates.io through trusted publishing.
  
  A dedicated release job now trades the workflow's OIDC identity for a crates.io token that expires after 30 minutes, instead of reading a long-lived API token from the repository's secrets. The code is unchanged from 0.1.0.

<pre>
$ git-stats v0.1.0..v0.1.1
Author      Commits  Changed Files  Insertions  Deletions  Net Δ
Luke Hsiao        2              3         +36        -20    +16
Total             2              3         +36        -20    +16
</pre>

## 0.1.0

### Minor Changes

- [`be8aedc`](https://github.com/lukehsiao/autolux/commit/be8aedcca81be99969c76c9057830104d6bb98d7) - **feat**: initial release of an adaptive backlight daemon. autolux reads an IIO ambient light sensor, maps illuminance to brightness along a logarithmic curve bounded by `--min` and `--max` percentages, and fades the panel continuously at 60 Hz rather than stepping. It writes brightness through systemd-logind, so it runs unprivileged as a systemd user service.
