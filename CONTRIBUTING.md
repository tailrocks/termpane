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

The MSRV floor (1.99, see `rust-version` in `Cargo.toml`) must keep passing:

```sh
rustup run 1.99.0 cargo check --locked
```

## Publishing

**Publishing through Velnor is unavailable until the updated release workflow has
passed its hosted qualification.** The procedure below describes the intended
process; it is not an operational runbook yet.

Velnor generates `.github/workflows/release.yml`. Make workflow changes in the
Velnor configuration and regenerate the workflows; do not edit generated files
by hand.

For a normal release, review and merge the release-plz release PR. The release
workflow must establish that the source is the reviewed release PR commit and
that the required checks passed before publishing. For a recovery release, use
an explicitly reviewed release intent that names the exact approved source
commit; do not represent that commit as a release-PR commit.

The intended publishing job uses the protected GitHub `release` environment and crates.io
trusted publishing via GitHub OIDC. The configured trusted publisher names
`tailrocks/termpane`, `release.yml`, and `release`. The publishing path for an
existing crate is intended not to need a stored `CARGO_REGISTRY_TOKEN`. The environment
and eligible branch or event restrictions must be verified before a release.

Before publication, check crates.io and GitHub for the intended version and
`termpane-v<version>` tag. After the workflow succeeds, verify that the tag
points to the approved source, download the published crate, check its registry
checksum, and compare its contents with the approved source. If a run is
interrupted, reconcile the registry, tag, and release state before retrying;
do not infer publication from a skipped or successful workflow alone.
