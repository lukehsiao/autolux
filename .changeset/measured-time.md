---
"autolux": patch
---

**fix**: fades now take their intended time.

autolux used to add up the sleeps it asked for, so the time each brightness write took made every frame longer. On a Surface Go 3 this stretched fades by about 15%. Frames and light samples now run on fixed deadlines, and time is measured with the monotonic clock. The ten-second sensor outage also counts measured time, which excludes suspend.
