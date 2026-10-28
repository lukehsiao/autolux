---
"autolux": patch
---

**fix**: a bad `max_brightness` now says what the file holds.

When a backlight's `max_brightness` file cannot be parsed, the error now quotes the file's real contents and says it expected a positive whole number.
