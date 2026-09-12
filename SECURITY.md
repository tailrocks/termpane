# Security Policy

## Reporting a Vulnerability

Please report vulnerabilities through [GitHub private vulnerability reporting](https://github.com/tailrocks/termpane/security/advisories/new) on this repository. Do not open public issues for security reports.

You can expect an acknowledgement within a few days. If the report is accepted, a fix lands on `main` and ships in the next patch release; you will be credited in the release notes unless you prefer otherwise.

## Supported Versions

| Version | Supported |
|---|---|
| latest 0.1.x | yes |
| older | no |

## Attack-Surface Posture

`termpane` is a parser of untrusted input by design: its whole job is consuming arbitrary VT/ANSI byte streams from programs the operator may not control. The crate mitigates that posture structurally:

- **Zero `unsafe`** — `unsafe_code = "forbid"` is enforced workspace-wide; there is no FFI and no non-Rust build dependency.
- **Deterministic and effect-free** — no wall-clock reads, no RNG, no filesystem or network access; a malicious byte stream can mutate grid state but cannot reach the host.
- **Bounded memory** — scrollback is capped by the caller-supplied limit; there are no unbounded growth paths keyed off input bytes.
- **Continuous fuzzing** — the `damage_grid_process` libFuzzer target (no-panic + state-invariant assertions) runs bounded in CI and long-form nightly with AddressSanitizer.

Even so, parser bugs that panic or mis-model state are treated as defects with security relevance; please report them privately as above.
