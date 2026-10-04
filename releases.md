# Releases

## Unreleased

- Expose `cargo:releases:github:release` as the canonical GitHub release task
  and retain `releases:github:release` as a temporary compatibility alias.
  Both names work with existing Bake versions and crate-derived namespaces.

## v0.2.8

- Move the shared Rust release process to `socketry-project` and remove the
  duplicate publishing context from Bake Cargo.
- Preserve repository-admin bypass for pull request merges when applying branch
  rulesets.

## v0.2.7

- Use the pull request base commit when detecting release changes.

## v0.2.6

- Validate release headings from parsed Markdown in both Bake and the publishing workflow.

## v0.2.5

- Document local path dependencies for private same-repository Bake packages.

## v0.2.4

- Create and synchronize GitHub Releases from `releases.md` after crates.io publication.

## v0.2.3

- Switch the runtime dependency from `socketry-bake` to `bake` 0.17.0.

## v0.2.2

- Correct the bootstrap task name in publishing guidance.

## v0.2.1

- Add Bake Agent Context tasks to the project's development executable.
- Link the shared Rust context guidance from the README.
- Allow the workflow initiator to approve the crates.io deployment.
- Use the generated `check` job as the default required branch check.

## v0.2.0

- Add an optional `cargo:after_version_bump` project hook for version-specific automation.

## v0.1.0

- Rename the Cargo task library to `bake-cargo` and give its tasks a Cargo namespace.
- Let project-local hooks compose license and release-note updates after Cargo version changes.
- Keep `bake-releases` focused on release-document tasks.
- Publish Cargo workspaces after reviewed release changes merge to the configured branch.
- Require environment reviewers before publishing and create version tags after successful uploads.
- Turn `cargo:release` into a release-candidate check instead of creating tags.
- Generate workflows that discover workspace packages dynamically.
- Move the Cargo, GitHub, and crates.io release tasks into their own repository.
- Support Cargo workspace discovery, publishing workflow setup, and trusted publishing.
- Add shared workspace version bumps and a tag-based release task.
- Skip package versions already published when running generated release workflows.
