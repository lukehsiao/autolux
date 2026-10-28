---
"autolux": patch
---

**fix**: tell logind refusals apart from bus failures.

When logind refuses a write because the session is not seated or not in the foreground, autolux still shows the help about running inside the graphical session. When the system bus is unreachable, it now says so and asks whether the bus is running, instead of showing the seated-session help. Any other logind error shows logind's error name and detail.
