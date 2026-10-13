# Contributing

## License

Apache-2.0. Contributions are licensed under the same terms (Section 5 of the license). This repository is [REUSE](https://reuse.software/)-compliant: new files need an SPDX header or are covered by the `REUSE.toml` catch-all annotation.

## DCO

All contributions must be signed off under [DCO v1.1](https://developercertificate.org/) — sign every commit with `git commit -s`. The `Signed-off-by` trailer must match the commit author.

Employer contributions: confirm authorization before submitting. Use a personal email in the commit author and sign-off.

## Commit Messages

All commits follow [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/):

```text
<type>[optional scope][!]: <description>
```

Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`. Breaking changes use `!` plus a `BREAKING CHANGE:` footer. PR squash-merge titles become commit subjects, so PR titles follow the same convention.

## How to Submit

1. Fork. Branch off `main` (`feature/`, `fix/`, `refactor/`, or `chore/` prefix).
2. Change. Sign every commit: `git commit -s`.
3. Run the gates below.
4. Open a PR describing the problem solved. CI must pass.

## Gates

The CI gates, runnable locally:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo nextest run --locked --profile ci
cargo nextest run --all-features --locked --profile ci
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
cargo deny check
reuse lint
cargo hack check --feature-powerset --all-targets --locked
cargo build --benches --locked
cargo fuzz run --sanitizer none damage_grid_process -- -max_total_time=30
```

The MSRV floor (1.97, see `rust-version` in `Cargo.toml`) must keep passing:

```sh
rustup run 1.97.0 cargo check --locked
```

## Publishing

Releases are published to crates.io **manually only**, via the `publish.yml` workflow (`workflow_dispatch`, requires typing the crate name as confirmation) or a local `cargo publish` by a maintainer. Tag releases `v*`; the `release.yml` workflow reruns the gates and drafts the GitHub Release from the changelog. No automation publishes to crates.io on tag push.
