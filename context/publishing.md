# Cargo Publishing

Use `bake-cargo` to prepare and publish Cargo projects through reviewed GitHub
Actions releases. This guide is the shared release procedure for Rust projects
using Bake Cargo tasks.

## Scope

Keep packages in one Cargo workspace when they should share release timing.
Publishable workspace packages use one version, one root `releases.md`, and one
`vVERSION` tag. The workflow publishes the packages together in dependency
order. Put packages that need independent versions, release notes, or tags in
separate repositories.

## Prepare a release

Use `cargo:version:patch`, `cargo:version:minor`, `cargo:version:major`, or
`cargo:version:bump --version X.Y.Z` to update the workspace version. These
tasks update local dependency requirements and `Cargo.lock`, then call the
optional `cargo:after_version_bump` task with the new version. Use that hook to
update the license, release notes, Readme, or other project files;
`socketry-project` provides the standard hook.

Review the version and generated files, then run `cargo:release` to validate and
package the release candidate. Commit the changes and open a pull request. The
workflow checks formatting, Clippy, tests, package versions, and the matching
`## vVERSION` heading in `releases.md`.

## Publish

Merging a reviewed version change to the configured branch requests publication.
The workflow waits for approval from the `crates-io` environment reviewers,
checks which packages at that version still need publishing, obtains a short-lived
crates.io token through GitHub OIDC, and publishes the remaining packages. It
creates and pushes the annotated `vVERSION` tag only after every upload succeeds.
If a multi-package upload fails partway through, rerunning the workflow skips
packages already published at that version. Tag pushes do not trigger another
publication.

After every package is published and the `vVERSION` tag exists, the workflow
creates or updates a GitHub Release from the matching heading in `releases.md`.
It leaves an identical release alone and synchronizes the title, notes, and
draft state when they differ. The task can also be run manually with
`cargo bake releases:github:release vVERSION`; add `--draft true` to create or
update a draft.

## Initial publication

Link `bake-cargo` from the private `bake/` task package with `use bake_cargo as _;`,
or use `socketry-project` to link the standard project tasks. Generate
`.github/workflows/publish.yml` with `cargo bake cargo:setup:workflow`. Review
the generated file before replacing an existing workflow.

Set required environment reviewers in Cargo metadata:

```toml
[workspace.metadata.bake.release]
reviewers = ["socketry/managers"]
```

For a single-package project without a workspace, use
`[package.metadata.bake.release]`. Preview repository and environment settings
with `cargo bake cargo:setup:github:plan`, then apply them with
`cargo bake cargo:setup:github:apply`. The tasks resolve reviewer names through
the authenticated `gh` CLI. Applying settings requires permission to manage
repository rulesets and environments. The setup permits the workflow initiator
to approve their own deployment; GitHub administrators may bypass environment
reviewers unless administrator bypass is disabled in GitHub settings.

For a package's first upload, `cargo bake cargo:bootstrap PACKAGE` publishes it
with a crates.io owner token and registers its GitHub Actions trusted publisher.
The token needs both publish and trusted-publishing endpoint scopes. Afterward,
the workflow uses OIDC. Configure the publisher for the correct repository,
`publish.yml`, and `crates-io` environment. Keep trusted-publishing-only mode
disabled until the workflow has published successfully; it can be enabled later
with `cargo bake cargo:trusted-publishing:require PACKAGE --required true`.
