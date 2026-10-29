# Changelog

## 0.1.2

### Patch Changes

- [`9fe85ae`](https://github.com/lukehsiao/autolux/commit/9fe85aecfd4a4ef81d2d430835469935e607e9e6) - **fix**: tell logind refusals apart from bus failures.
  
  When logind refuses a write because the session is not seated or not in the foreground, autolux still shows the help about running inside the graphical session. When the system bus is unreachable, it now says so and asks whether the bus is running, instead of showing the seated-session help. Any other logind error shows logind's error name and detail.

- [`7719399`](https://github.com/lukehsiao/autolux/commit/77193996744ba68b3a8e0e985c24d5ae1b14b5e4) - **fix**: a bad `max_brightness` now says what the file holds.
  
  When a backlight's `max_brightness` file cannot be parsed, the error now quotes the file's real contents and says it expected a positive whole number.

- [`a1b682e`](https://github.com/lukehsiao/autolux/commit/a1b682e74309da43567a0c480f4cafee60bf1be1) - **fix**: fades now take their intended time.
  
  autolux used to add up the sleeps it asked for, so the time each brightness write took made every frame longer. On a Surface Go 3 this stretched fades by about 15%. Frames and light samples now run on fixed deadlines, and time is measured with the monotonic clock. The ten-second sensor outage also counts measured time, which excludes suspend.

- [`ffb291d`](https://github.com/lukehsiao/autolux/commit/ffb291dd3126c35bed151961b49d1fa5fd3dcdcb) - **perf**: fades write to the panel far less often.
  
  Every brightness write is a D-Bus call that logind handles, so a fade that wrote on almost every frame kept logind busy. autolux now writes only when the panel has moved at least 0.2% of the lightness scale since the last write, which is still far below a visible step, and sleeps through the frames in between. The write that ends a fade still lands exactly on the target.

<pre>
$ git-stats v0.1.1..v0.1.2
Author      Commits  Changed Files  Insertions  Deletions  Net Δ
Luke Hsiao       11             53       +3092      -2041  +1051
Total            11             53       +3092      -2041  +1051
</pre>

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
