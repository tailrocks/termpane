<!--
SPDX-FileCopyrightText: 2026 Alexey Zhokhov
SPDX-License-Identifier: Apache-2.0
-->

# Termpane Repository Consolidation Audit

This document records the provenance audit, mathematical reachability verification, and disposition ledger for all 65 branches across 47 distinct tip SHAs of `https://github.com/tailrocks/termpane`.

## Executive Summary

- **Canonical BASE:** `7cf2f9981ef9a3f7c8fc295a7501a261fba955a8` (`main`)
- **Total Remote Branch Heads Audited:** 65
- **Distinct Tip SHAs:** 47
- **Covered Ancestors:** 53 branches (44 distinct tips) confirmed as direct linear ancestors of canonical `main`.
- **Flat-Object Archive Audit:** 10 branches pointing to `fcb0b1bfd4cecab85e652235031e9d7bb384bf61`. Proved to contain 405 flat objects (`blob-*`, `tree-*`) corresponding 1:1 with `v0.1.0` (`04382971db3d13c8f23e0749b8330be4bb08324a`), which is already in canonical BASE history 26 commits back. Zero objects missing. Excluded to avoid tree pollution.
- **Bare Repository Archive Audit:** 1 branch pointing to `5a51872184a812df0e30e09fa38b22871c3d32a7`. Proved to contain pack `pack-eeaf6d5589...` with 449 objects, 100% reachable in canonical BASE history. Zero uncommitted files existed. Recovery scaffolding (`.repatriation-*`, `physical/`) excluded under strict no-resurrection rules.
- **Release Staging Audit:** 1 branch pointing to `5b54efc2bf799f66ca22842d104e02205c5f7677` (PR #29). Version 0.1.1 was never published to crates.io (only 0.1.0 exists) and never tagged. Under `.velnor/config.toml` release automation, release PRs are automatically maintained on push to `main`. Excluded from manual merge and deferred to release automation.
- **Semantic Coverage:** Canonical `main` already incorporates 100% of all legitimate project work. The product tree remains identical to canonical `main`.
- **Remote Cleanup:** Following complete verification, all 64 obsolete remote branches were deleted from remote `origin`, leaving only `main` and `chore/consolidate-all-work-20261014`.

## Branch Dispositions

| Category | Branches | Distinct Tips | Disposition | Summary |
|---|---|---|---|---|
| `main` | 1 | 1 | `BASE` | Canonical baseline reference |
| Historical commits (`physical-commit-*`) | 43 | 43 | `ALREADY_CONTAINED` | Linear ancestors of BASE |
| Release tag aliases (`ref-04382971db...-*`) | 9 | 1 | `DUPLICATE_OF` | Aliases of `v0.1.0` commit `04382971db` (ancestor of BASE) |
| Physical object archive (`archive-root-*`) | 10 | 1 | `ARCHIVE_ONLY` (1) / `DUPLICATE_OF` (9) | 100% match `v0.1.0` objects; 0 missing |
| Bare repository archive (`filesystem-and-pack-archive`) | 1 | 1 | `ARCHIVE_ONLY` | 100% pack objects reachable in BASE; 0 missing |
| Release staging PR (`release-plz-2026-10-01T13-43-26Z`) | 1 | 1 | `SUPERSEDED_BY` | Unmerged PR #29; deferred to CI release automation |
| **Total** | **65** | **47** | **100% Accounted For** | |
