<!--
GraftX pull request template.
Keep the description focused. Squash-merge to main uses the PR title as the
final commit subject, so the title MUST follow Conventional Commits.
-->

## Description

<!-- What does this PR change, and why? Keep it short and concrete. -->

## Type of change

<!-- Check all that apply. The PR title must use the matching prefix. -->

- [ ] `feat:` new functionality (e.g. additional API coverage, transport, shim)
- [ ] `fix:` bug fix
- [ ] `docs:` documentation only
- [ ] `refactor:` code change that neither fixes a bug nor adds a feature
- [ ] `chore:` build, tooling, CI, or maintenance
- [ ] `test:` tests only

## Related issues

<!-- e.g. Closes #123, Refs #456. Use "N/A" if none. -->

Closes #

## Checklist

- [ ] PR title follows Conventional Commits (`feat:` / `fix:` / `docs:` / `refactor:` / `chore:` / `test:`)
- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` is clean
- [ ] `cargo test --workspace` passes
- [ ] Docs and/or `CHANGELOG.md` updated if this change is user-facing
- [ ] All `unsafe`/FFI is isolated in dedicated modules with `// SAFETY:` comments; no `unwrap()`/`expect()` on library paths
- [ ] Untrusted command-stream inputs are validated/bounds-checked where this change touches server replay or the wire protocol
- [ ] This PR contains NO AI attribution (no `Co-Authored-By` / `Generated-with` trailers or footers in commits or this description)
