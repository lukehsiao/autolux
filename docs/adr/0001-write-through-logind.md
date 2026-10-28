# Write the panel through logind, and manage its cost by writing less

autolux sets the panel through systemd-logind's `SetBrightness` rather than writing `/sys/class/backlight/*/brightness` directly. That keeps the install rootless: no udev rule, no setuid, no group membership. The price is real. logind forks a writer child for every call, which costs about 1.7 ms of logind CPU per write and about 1.9 ms of latency on a Surface Go 3. A 120 s dusk run made 602 writes and charged 1,021 ms of CPU to `systemd-logind.service`, against 148 ms for autolux itself. We manage that cost by making fewer writes per fade, not by writing to sysfs directly.

## Considered Options

- **Direct sysfs writes.** These need write access to a root-owned attribute, which means a udev rule or an extra privileged helper. The README promises an install that needs neither.
- **Fewer writes through logind (chosen).** Writes per fade are the dominant cost, and the controller owns the decision of which frames to write.
