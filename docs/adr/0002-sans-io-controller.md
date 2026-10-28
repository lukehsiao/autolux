# The Controller is sans-IO and accepts any input at any time

Every control decision lives in a sans-IO `Controller`: sampling, waking, fading, frame pacing, which frames to write, and the outage policy. It takes monotonic time and I/O results as input and returns the next thing to do. It accepts any input at any time: a sample or a panel reading that it did not ask for is still valid. As a result, the caller has no ordering rule it could break. Property tests drive the Controller with a pure in-memory world. The nullable infrastructure tests only the thin I/O loop around it. This is a deliberate exception to the crate's nullables style. The bugs live in the control decisions and in the order of calls, and a pure core with no ordering rule exposes all of them to property tests.

## Considered Options

- **Deepen the existing Controller and test it over nullables.** The nulled panel replays scripted reads instead of reading back writes, and it cannot model a timed manual adjustment or write latency. Making it able to would grow test machinery inside production adapters.
- **A generic loop over one `Io` or `Hardware` world trait.** Production and tests would share one loop, but the trait has only one production adapter, so it is a hypothetical seam. Its test implementation runs none of the real adapter code.
- **Typestate tokens that encode wake, then panel read, then resume.** These make misuse a compile error. But once the Controller accepts any input at any time, there is no order left to enforce, and the tokens only add plumbing.
