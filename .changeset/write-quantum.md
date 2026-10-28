---
"autolux": patch
---

**perf**: fades write to the panel far less often.

Every brightness write is a D-Bus call that logind handles, so a fade that wrote on almost every frame kept logind busy. autolux now writes only when the panel has moved at least 0.2% of the lightness scale since the last write, which is still far below a visible step, and sleeps through the frames in between. The write that ends a fade still lands exactly on the target.
