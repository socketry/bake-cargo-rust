# Releases

## Unreleased

## v0.1.0

- Rename the Cargo task library to `bake-cargo` and give its tasks a Cargo namespace.
- Run `license:update` automatically after Cargo version changes.
- Keep `bake-releases` focused on release-document tasks.
- Publish Cargo workspaces after reviewed release changes merge to the configured branch.
- Require environment reviewers before publishing and create version tags after successful uploads.
- Turn `cargo:release` into a release-candidate check instead of creating tags.
- Generate workflows that discover workspace packages dynamically.
- Move the Cargo, GitHub, and crates.io release tasks into their own repository.
- Support Cargo workspace discovery, publishing workflow setup, and trusted publishing.
- Add shared workspace version bumps and a tag-based release task.
- Skip package versions already published when running generated release workflows.
