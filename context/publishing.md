# Cargo Publishing

Use `bake-cargo` to prepare and publish Cargo projects through reviewed GitHub
Actions releases.

## Workspace release boundaries

Keep packages in one Cargo workspace when they are intended to share release
timing. Publishable workspace packages should use one version and one root
`releases.md`; the workflow creates one `vVERSION` tag after all packages at
that version have been published. Put packages that need independent versions,
release notes, or tags in separate repositories.

## Prepare a release

Use `cargo:version:patch`, `cargo:version:minor`, `cargo:version:major`, or
`cargo:version:bump --version X.Y.Z` to update the shared workspace version.
These tasks call an optional project task named
`cargo:after_version_bump`. Use that hook to update the license, release notes,
readme, or other project files. `socketry-project` provides the standard hook.

Review the version and generated files, then run `cargo:release` to validate
the release candidate. Commit the changes and open a pull request. The
publishing workflow checks formatting, Clippy, tests, package versions, and the
matching `## vVERSION` heading in `releases.md`. Merging the reviewed version
change to the configured branch requests publication.

## Configure GitHub and crates.io

Generate `.github/workflows/publish.yml` with `cargo:setup:workflow`. Put the
required GitHub reviewers in Cargo metadata:

```toml
[workspace.metadata.bake.release]
reviewers = ["socketry/managers"]
```

Use `cargo:setup:github:plan` to inspect the desired branch and tag rulesets and
the `crates-io` environment before applying them with
`cargo:setup:github:apply`. The setup permits the workflow initiator to approve
their own deployment; the configured reviewer list still gates publication.
GitHub administrators may bypass environment reviewers unless administrator
bypass is disabled in the environment settings.

For a package's first release, `cargo:bootstrap` publishes
with a crates.io owner token and registers the GitHub Actions trusted publisher.
Afterward, the workflow uses GitHub OIDC to obtain a short-lived publishing
token. Configure the crates.io publisher with the correct GitHub repository,
workflow filename, and `crates-io` environment. Do not enable trusted-publishing-
only mode until the workflow has published successfully.

The workflow skips workspace packages whose current version is already on
crates.io, so a failed multi-package publication can be retried. It creates and
pushes the annotated `vVERSION` tag only after every remaining package upload
succeeds. Tags do not trigger another publication.
