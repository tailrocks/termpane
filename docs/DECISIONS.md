# Decisions

Record new decisions here as numbered entries. Do not rewrite an earlier entry; add a new one that supersedes it.

## 1. Releases paused until the velnor-gen release flow is hardened

The migration to velnor-gen 0.1.116 ships CI only. `.velnor/release.toml` and `release-plz.toml` are removed, so `velnor-gen` generates no `release.yml`, and the `release-plz` pin is dropped from `mise.toml` and `mise.lock`. No automated release runs: a merge to `main` neither opens a release pull request nor tags nor publishes.

Reason: the generated release flow had open P1 and P2 findings at head `76da6af` and could not run anyway. The `release` environment has no release App secrets, the crate has no crates.io trusted publisher for `release.yml`, and the tag ruleset `protect-tags` lacks the `creation` rule the helper requires. The generator findings are tracked upstream in [#3121](https://github.com/tailrocks/velnor-new/issues/3121), [#3122](https://github.com/tailrocks/velnor-new/issues/3122), [#3123](https://github.com/tailrocks/velnor-new/issues/3123), [#3124](https://github.com/tailrocks/velnor-new/issues/3124), [#3125](https://github.com/tailrocks/velnor-new/issues/3125), [#3126](https://github.com/tailrocks/velnor-new/issues/3126) and [#3128](https://github.com/tailrocks/velnor-new/issues/3128).

Lift this decision, with a new entry, only when those issues are closed, the owner has provisioned the release App and the crates.io trusted publisher, and the tag ruleset carries the rules the helper requires. Before the first automated release, also settle the version: the registry is at `0.1.0`, and `CHANGELOG.md` lists a breaking hyperlink API change under Unreleased, which release-plz would publish as `0.1.1` unless the workspace is bumped to `0.2.0` first.
