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

The generated `crate-release.yml` workflow uses release-plz to prepare release pull requests and publish `termpane` to crates.io. The release configuration is in `.velnor/config.toml`. To change it, update the configuration and regenerate the workflow with Velnor Actions. Do not edit generated files directly.

The checked-in workflow is being corrected and must not be used for another publication attempt until the replacement has been reviewed, merged, and qualified. The current configuration refers to a bootstrap token; the intended routine path uses crates.io Trusted Publishing and the protected `release` environment. A maintainer must configure the trusted publisher on crates.io and the required GitHub environment protections before using that path.

The pending `0.1.1` recovery is limited to package `termpane`, version `0.1.1`, and source commit `f1c2dfde45bdbe91425af6bb17d80e606703baac`. After the corrected workflow and its exact recovery configuration are reviewed and merged, use a fresh approved dispatch for that plan. Confirm the source and version in the reviewed configuration and obtain the required `release` environment approval. Do not select a different commit or version to work around a failure.

For subsequent releases, review and merge the release-plz preparation PR, then use the qualified generated workflow and the protected environment approval. When a workflow creates or updates the PR with `GITHUB_TOKEN`, a user with write access must approve its workflow runs before checks start; see [GitHub's `GITHUB_TOKEN` documentation](https://docs.github.com/en/actions/concepts/security/github_token). Confirm the published crate and its contents on crates.io, the `termpane-v<version>` tag points to the approved source commit, and the GitHub Release matches before calling the release complete. A successful workflow step or a dry run alone does not establish publication.

If a run fails or is interrupted, inspect the registry version, tag, GitHub Release, and durable release record before retrying; never blindly republish or overwrite an existing version or tag. The release implementation currently accepts only the first attempt of a run for approval and predecessor verification. Resume an interrupted first attempt through a newly reviewed recovery configuration naming its exact run ID, attempt `1`, and workflow SHA, followed by a fresh approved dispatch. If a later attempt performed an operation, stop and reconcile it separately rather than treating the first attempt as its record.
