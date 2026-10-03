# Bake Cargo

`bake-cargo` provides reusable tasks for Cargo projects. Cargo operations register
beneath `cargo`, and GitHub release creation registers beneath
`releases:github`. Add the package to an unpublished `bake/` task binary and link
it once:

```toml
[dependencies]
bake = "0.17"
bake-cargo = { version = "0.2" }
```

```rust,ignore
use bake_cargo as _;
```

This crate links `bake-releases` and `bake-license`, so release-document tasks
and `license:update` are also available without extra imports. The task
executable links `socketry-project`, which provides the standard project tasks
and hooks. Run `cargo bake agent:context:install` to install context from
dependencies such as `bake`; generated context and skills are excluded locally
through Git's `info/exclude` file and do not add rules to `.gitignore`.
Shared Rust guidance lives in
[Bake Agent Context](https://github.com/socketry/bake-agent-context-rust/blob/main/context/rust.md).
This crate also publishes guidance in
[`context/publishing.md`](context/publishing.md). Link Bake Agent Context in
your private `bake/` package, directly or through `socketry-project`, then run
`cargo bake --regenerate` to link its tasks and
`cargo bake agent:context:install` to install guidance from dependencies.

## Inspect and package

```sh
cargo bake cargo:packages
cargo bake cargo:package bake
```

Workspace discovery uses `cargo metadata --no-deps` and ignores packages marked
`publish = false` or configured for no registry. Package names and versions are
read from Cargo metadata.

## Versioning and releases

The `cargo:version:patch`, `cargo:version:minor`, and `cargo:version:major`
tasks update the shared stable version of all publishable packages in the
workspace. `cargo:version:bump --version X.Y.Z` sets an explicit higher version.
These tasks preserve TOML formatting, update version requirements for
in-workspace dependencies, and refresh `Cargo.lock`.
After the update, they optionally call a project task named
`cargo:after_version_bump`, passing the new version. If that task is absent,
the version bump completes without a hook. A project can use the hook to run
`license:update`, update `releases.md`, or perform other project-specific work:

```rust,ignore
#[bake::task(name = "cargo:after_version_bump")]
fn after_version_bump(context: &mut bake::Context, version: String) -> bake::Result<()> {
    context.call("license:update", &[])?;
    let heading = format!("v{version}");
    context.call("releases:update", &[&heading])?;
    Ok(())
}
```

Projects using `socketry-project` get a standard hook that updates the license,
release notes, and generated Readme sections.

Review the changes made by the hook before committing the release.

After updating the shared version and adding the matching release-notes heading,
run `cargo:release` to validate and package the release candidate. It
does not create a tag or publish anything. Commit the version, release notes,
and any other release changes, then open a pull request for review. The merge to
the configured branch is the release request.

## GitHub workflow and repository settings

Generate `.github/workflows/publish.yml` after reviewing the existing workflow:

```sh
cargo bake cargo:setup:workflow
```

It refuses to replace a different existing file. Use `--force true` only after
reviewing the generated output. The workflow runs workspace checks on pull
requests and branch updates. It compares the workspace version with the pull
request base or previous branch commit, and requires a matching `## vVERSION`
heading in `releases.md` for a version change. Ordinary commits do not publish.
For a release change, it waits for approval from the `crates-io` environment
reviewers, checks which workspace packages still need that version, exchanges a
GitHub OIDC token for a short-lived crates.io token, and publishes them. It
creates and pushes the annotated `vVERSION` tag only after every package upload
succeeds. The workflow uses the GitHub environment `crates-io` and workflow file
`publish.yml` by default.

Plan repository protections before applying them. Store the desired reviewers
in the project's Cargo metadata so the setup task can apply them:

```toml
[workspace.metadata.bake.release]
reviewers = ["socketry/managers"]
```

For a single-package project without a workspace, use
`[package.metadata.bake.release]` instead. List GitHub user logins or
organization/team slugs; the setup task resolves them to GitHub IDs using the
authenticated `gh` CLI. Cargo ignores this tool-specific metadata, and
`cargo metadata` exposes it to Bake. An explicit `--reviewers` argument
overrides the manifest value. The branch ruleset permits repository admins to
bypass branch requirements for pull request merges while still blocking direct
pushes. Environment reviewer bypass is configured separately. Review the plan
before applying it:

```sh
cargo bake cargo:setup:github:plan \
  --branch main \
  --approvals 1 \
  --wait-timer 0
cargo bake cargo:setup:github:apply \
  --branch main \
  --approvals 1 \
  --wait-timer 0
```

If `--repository owner/name` is omitted, the GitHub repository is inferred from
the `origin` Git remote. `--checks` is repeatable and should match status check
names already reported by GitHub. When `--wait-timer` is omitted, the existing
environment wait timer is left unchanged. At least one reviewer is required,
and the selected list replaces the environment's current reviewer list. Both
tasks use the authenticated `gh` CLI to resolve reviewer names. The apply task
also needs permission to manage repository rulesets and environments.
It creates or updates the two rulesets managed by this package and configures
the named environment's wait timer and reviewer list. Existing managed rulesets
are replaced with the reviewed desired settings; unrelated rulesets are left
alone.
When `--checks` is omitted, the ruleset requires the `check` job generated by
the workflow above. Configure at least one required reviewer on the `crates-io`
environment to require approval before publishing. The setup allows the
workflow initiator to approve their own deployment. GitHub administrators can
bypass environment rules by default. To make approval mandatory for
administrators too, disable that bypass in the environment's GitHub settings.
Tag pushes do not trigger publication.

## Trusted publishing

After a package has been published once, crates.io allows its owners to register
a GitHub Actions trusted publisher. Preview and apply the configuration with:

```sh
cargo bake cargo:trusted-publishing:plan bake
export CARGO_REGISTRY_TOKEN=...
cargo bake cargo:trusted-publishing:configure bake
```

The token must have the crates.io **Trusted Publishing** endpoint scope. For
`bootstrap`, it also needs permission to publish the selected crate because the
task passes the same `CARGO_REGISTRY_TOKEN` to Cargo and the crates.io API. The
request uses Cargo's raw token value in the `Authorization` header. It is read
from the environment, never placed in command arguments or printed. The desired
configuration is inferred from `origin` and includes `publish.yml` and
`crates-io` by default. Re-running configuration is idempotent.

For a crate that has not yet been published, `cargo:bootstrap PACKAGE`
performs the initial `cargo publish --locked --package PACKAGE`, then registers
the trusted publisher. This is an explicit publication task: review package
contents and release state first. If the upload succeeds but publisher setup
fails, the error identifies the follow-up configure task. It does not enable
trusted-publishing-only mode automatically. Once the GitHub workflow succeeds,
you can enable that registry requirement:

```sh
cargo bake cargo:trusted-publishing:require bake --required true
```

Keep that setting disabled until the configured workflow has successfully
published. The registry setting is reversible with `--required false`.

## Scope

Create a GitHub Release from the notes under its matching heading after the
release tag exists:

```sh
cargo bake releases:github:release vX.Y.Z --draft true
```

The task uses the authenticated `gh` CLI. Omit `--draft true` to publish the
release immediately.

The Cargo release integration supports crates.io and GitHub Actions. It edits
Cargo package version fields and local dependency requirements, but does not
commit changes or publish crates itself. The GitHub release task publishes a
GitHub Release explicitly. The setup task only manages its named repository
rulesets, and GitHub may require repository or organization plan features for
some settings.

## Contributing

Please open an issue or pull request on [GitHub](https://github.com/socketry/bake-cargo-rust).

### Agent Context

Before contributing, read `agents.md` and the relevant context files it links. If `agents.md` is missing or out of date, run `cargo bake agent:context:install` to install context from dependencies and update the index.

## Releasing

Prepare a release with `cargo bake cargo:version:patch` (or `minor`, `major`,
or `bump --version X.Y.Z`), then run `cargo bake cargo:release` and open a
pull request. After review and merge, GitHub Actions publishes the release
when the configured `crates-io` environment approves it, then creates or updates
the matching GitHub Release from `releases.md`. See the
[Cargo publishing guide](https://github.com/socketry/bake-cargo-rust/blob/main/context/publishing.md).
