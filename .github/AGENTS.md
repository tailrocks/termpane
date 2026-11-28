# Generated GitHub Workflows and Configurations

`velnor-gen` from [tailrocks/velnor-new](https://github.com/tailrocks/velnor-new) owns the workflow files that it generates under `.github/workflows/` (`ci.yml`, `release.yml` and `rust-cache-trim.yml`). Successful generation replaces emitted owned paths and retires owned paths no longer emitted. Do not hand-edit these managed paths; change their inputs and run `velnor-gen --root . generate`. The inputs are `.velnor/rust-ci.toml`, `.velnor/release.toml` and `release-plz.toml`.

Other existing files and directories under `.github/` are repository-owned and preserved by generation, including empty directories. Put custom files outside `.github/workflows/`, which is reserved for generated workflows.

## Generation Inputs

- **Keep generation inputs outside `.github/`**: Repository configuration belongs in `.velnor/rust-ci.toml`, `.velnor/release.toml`, workspace manifests, and toolchain settings outside `.github/`.
- **Regenerate via CLI**: Apply configuration changes by running `velnor-gen --root . generate`, then `velnor-gen --root . check`. The `ci-generated` mise task runs the check.

## Workflow Issues and Enhancements

When issues, bugs, limitations, or enhancements are observed in generated GitHub Actions workflows:

1. **Do NOT patch generator-owned outputs or create repo-specific workflow workarounds**:
   - Avoid local patches, overrides, or shims to managed workflow, config, or action files.
2. **Research generic solutions in `velnor-new`**:
   - Investigate solutions in the upstream generator: [tailrocks/velnor-new](https://github.com/tailrocks/velnor-new).
   - Verify that potential changes align with the vision and architectural invariants of Velnor (generic workflow generator, strict safety invariants, zero legacy, fast CI).
3. **Submit PR to upstream `velnor-new`**:
   - Only after thorough verification and review, submit a PR to `velnor-new` (https://github.com/tailrocks/velnor-new).
4. **Regenerate workflows**:
   - After the fix is merged and released, bump the `velnor-gen` pin in `mise.toml` and `velnor-gen --root . pin-helper <version>`, then regenerate.

## Root Repository Rules

- If a root `AGENTS.md` is present in this repository, all of its rules, invariants, and guidelines apply here as well.
- In case of conflict, stricter safety, correctness, and verification standards take precedence.
