---
"autolux": patch
---

**chore**: publish releases to crates.io through trusted publishing.

A dedicated release job now trades the workflow's OIDC identity for a crates.io token that expires after 30 minutes, instead of reading a long-lived API token from the repository's secrets. The code is unchanged from 0.1.0.
