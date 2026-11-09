## Summary

<!-- Explain the user-visible or developer-facing change and why it is needed. -->

## Changes

<!-- List the important implementation details. Keep this focused on what reviewers need to understand. -->

- 

## Safety and compatibility

<!-- For ECU, tune, project format, configuration, or dependency changes, describe the impact. Otherwise write "No impact." -->

- ECU write/burn behavior:
- Existing projects or user configuration:
- Dependencies and licensing:

## Verification

<!-- List commands actually run and their outcomes. Mark anything not run as not run, with a short reason. Include relevant manual or hardware checks. -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cargo deny check`
- [ ] `pnpm fmt:check`
- Manual or hardware checks:

## Performance

<!-- Required when changing a hot path or adding per-frame/project-open work. Include same-machine before/after numbers and benchmark names. Otherwise write "Not applicable." -->

Not applicable.

## Screenshots or recordings

<!-- For UI changes, add before/after screenshots or a short recording. Otherwise write "Not applicable." -->

Not applicable.

## Scope

<!-- Note meaningful exclusions or follow-up work. If none, write "None." -->

- Not included:

## Checklist

- [ ] The PR title follows Conventional Commits.
- [ ] Tests cover behavior changed by this PR.
- [ ] Public API and user-facing behavior are documented where appropriate.
- [ ] No real vendor tune, INI, or log fixtures were added.
- [ ] No new code path bypasses ECU write safeguards.
- [ ] I reviewed the diff and removed unrelated changes.
