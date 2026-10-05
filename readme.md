# `bake-cargo`

`bake-cargo` provides reusable tasks for Cargo projects. Cargo operations register beneath `cargo`, including GitHub release creation at `cargo:releases:github:release`. Add the package to an unpublished `bake/` task binary and regenerate its task links:

```sh
cargo bake --regenerate
cargo add --manifest-path bake/Cargo.toml bake-cargo
cargo bake --regenerate
```

This crate links `bake-releases` and `bake-license`, so release-document tasks and `license:update` are also available without extra imports. The task executable links `socketry-project`, which provides the standard project tasks and hooks. Run `cargo bake agent:context:install` to install context from dependencies such as `bake`; generated context and skills are excluded locally through Git's `info/exclude` file and do not add rules to `.gitignore`. Shared Rust guidance lives in [Bake Agent Context](https://github.com/socketry/bake-agent-context-rust/blob/main/context/rust.md). For the standard Socketry release process, use the [Releasing skill](https://github.com/socketry/socketry-project-rust/blob/main/context/releasing.md). This Readme and each task's `--help` output document the Bake Cargo commands. Link Bake Agent Context in your private `bake/` package, directly or through `socketry-project`, then run `cargo bake --regenerate` to link its tasks and `cargo bake agent:context:install` to install guidance from dependencies.

## Inspect and package

```sh
cargo bake cargo:packages
cargo bake cargo:package bake
```

Workspace discovery uses `cargo metadata --no-deps` and ignores packages marked `publish = false` or configured for no registry. Package names and versions are read from Cargo metadata.

## Versioning and releases

The `cargo:version:patch`, `cargo:version:minor`, and `cargo:version:major` tasks update the shared stable version of all publishable packages in the workspace. `cargo:version:bump --version X.Y.Z` sets an explicit higher version. These tasks preserve TOML formatting, update version requirements for in-workspace dependencies, and refresh `Cargo.lock`. After the update, they optionally call a project task named `cargo:after_version_bump`, passing the new version. If that task is absent, the version bump completes without a hook. A project can use the hook to run `license:update`, update `releases.md`, or perform other project-specific work:

```rust,ignore
#[bake::task(name = "cargo:after_version_bump")]
fn after_version_bump(context: &mut bake::Context, version: String) -> bake::Result<()> {
    context.call("license:update", &[])?;
    let heading = format!("v{version}");
    context.call("releases:update", &[&heading])?;
    Ok(())
}
```

Projects using `socketry-project` get a standard hook that updates the license, release notes, and generated Readme sections.

Review the changes made by the hook before committing the release.

After updating the shared version and adding the matching release-notes heading, commit the version, release notes, and any other release changes. Then run `cargo:release` to validate and package the committed release candidate. It does not create a tag or publish anything. Open a pull request for review; the merge to the configured branch is the release request.

## GitHub workflow and repository settings

Generate `.github/workflows/publish.yml` after reviewing the existing workflow:

```sh
cargo bake cargo:setup:workflow
```

It refuses to replace a different existing file. Use `--force true` only after reviewing the generated output.

The standard workflows have separate responsibilities. `test.yml` runs the project's tests and coverage checks on pull requests and pushes. `publish.yml` runs release validation, formatting, and Clippy on pull requests, but skips its ordinary test command because `test.yml` already tests the proposed changes. On pushes to `main`, `publish.yml` runs ordinary workspace tests in its `check` job before the `publish` job can start. `test.yml` also runs on pushes, so the branch gets both coverage results and the test run required before publishing. The `publish` job only runs after a successful check when the push contains a versioned release.

`cargo:release:detect` compares the workspace version with the pull request base or previous branch commit, validates the release heading, and reports whether a versioned release is ready. The `cargo:release` task packages the candidate on release changes. Ordinary commits do not publish. After merge, `cargo:publish:pending` checks which workspace packages still need that version; the workflow exchanges a GitHub OIDC token for a short-lived crates.io token only when uploads remain. `cargo:release:publish` publishes the remaining packages, creates and pushes the annotated `vVERSION` tag, and creates or updates the matching GitHub Release through `cargo:releases:github:release`. The workflow uses the GitHub environment `crates-io` and workflow file `publish.yml` by default.

Plan repository protections before applying them. Store the desired reviewers in the project's Cargo metadata so the setup task can apply them:

```toml
[workspace.metadata.bake.release]
reviewers = ["socketry/managers"]
```

For a single-package project without a workspace, use `[package.metadata.bake.release]` instead. List GitHub user logins or organization/team slugs; the setup task resolves them to GitHub IDs using the authenticated `gh` CLI. Cargo ignores this tool-specific metadata, and `cargo metadata` exposes it to Bake. An explicit `--reviewers` argument overrides the manifest value. The branch ruleset permits repository admins to bypass branch requirements for pull request merges while still blocking direct pushes. Environment reviewer bypass is configured separately. Review the plan before applying it:

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

If `--repository owner/name` is omitted, the GitHub repository is inferred from the `origin` Git remote. `--checks` is repeatable and should match status check names already reported by GitHub. When `--wait-timer` is omitted, the existing environment wait timer is left unchanged. At least one reviewer is required, and the selected list replaces the environment's current reviewer list. Both tasks use the authenticated `gh` CLI to resolve reviewer names. The apply task also needs permission to manage repository rulesets and environments. It creates or updates the two rulesets managed by this package and configures the named environment's wait timer and reviewer list. Existing managed rulesets are replaced with the reviewed desired settings; unrelated rulesets are left alone. When `--checks` is omitted, the ruleset requires the publishing workflow's `check` job and the testing workflow's stable `test-result` aggregate job. Configure at least one required reviewer on the `crates-io` environment to require approval before publishing. The setup allows the workflow initiator to approve their own deployment. GitHub administrators can bypass environment rules by default. To make approval mandatory for administrators too, disable that bypass in the environment's GitHub settings. Tag pushes do not trigger publication.

## Trusted publishing

After a package has been published once, crates.io allows its owners to register a GitHub Actions trusted publisher. Preview and apply the configuration with:

```sh
cargo bake cargo:trusted-publishing:plan bake
export CARGO_REGISTRY_TOKEN=...
cargo bake cargo:trusted-publishing:configure bake
```

The token must have the crates.io **Trusted Publishing** endpoint scope. For `bootstrap`, it also needs permission to publish the selected crate because the task passes the same `CARGO_REGISTRY_TOKEN` to Cargo and the crates.io API. The request uses Cargo's raw token value in the `Authorization` header. It is read from the environment, never placed in command arguments or printed. The desired configuration is inferred from `origin` and includes `publish.yml` and `crates-io` by default. Re-running configuration is idempotent.

For a crate that has not yet been published, `cargo:bootstrap PACKAGE` performs the initial `cargo publish --locked --package PACKAGE`, then registers the trusted publisher. This is an explicit publication task: review package contents and release state first. If the upload succeeds but publisher setup fails, the error identifies the follow-up configure task. It does not enable trusted-publishing-only mode automatically. Once the GitHub workflow succeeds, you can enable that registry requirement:

```sh
cargo bake cargo:trusted-publishing:require bake --required true
```

Keep that setting disabled until the configured workflow has successfully published. The registry setting is reversible with `--required false`.

## Scope

Create a GitHub Release from the notes under its matching heading after the release tag exists:

```sh
cargo bake cargo:releases:github:release vX.Y.Z --draft true
```

The task uses the authenticated `gh` CLI. Omit `--draft true` to publish the release immediately.

The Cargo release integration supports crates.io and GitHub Actions. It edits Cargo package version fields and local dependency requirements, but does not commit changes or publish crates itself. The GitHub release task publishes a GitHub Release explicitly. The setup task only manages its named repository rulesets, and GitHub may require repository or organization plan features for some settings.

## Releasing

Prepare a release with `cargo bake cargo:version:patch` (or `minor`, `major`, or `bump --version X.Y.Z`), then run `cargo bake cargo:release` and open a pull request. After review and merge, GitHub Actions publishes the release when the configured `crates-io` environment approves it, then creates or updates the matching GitHub Release from `releases.md`. Follow the shared [Releasing skill](https://github.com/socketry/socketry-project-rust/blob/main/context/releasing.md) for the standard process.

## Releases

<!-- bake-readme:releases:start -->

See [releases.md](releases.md) for the full release history.

### v0.4.2

- Adopt `socketry-project` 0.3.7 for shared project tasks and Markdown normalization.
- Require the aggregate test and coverage result for pull request merges.
- Require publishing checks and test results by default when configuring repository protections.
- Update Bake usage examples and agent context guidance.

### v0.4.1

- Treat a missing base Cargo manifest as the initial package release.

### v0.4.0

- Remove the temporary `releases:github:release` compatibility alias in favor of `cargo:releases:github:release`.
- Replace the inline publish workflow release scripts with standard Bake Cargo tasks for detection, validation, publishing, tagging, and GitHub Release sync.

<!-- bake-readme:releases:end -->

## See Also

- [`bake`](https://github.com/socketry/bake-rust).
- [`bake-releases-cargo`](https://github.com/socketry/bake-releases-cargo-rust).

## Contributing

Please open an issue or pull request on [GitHub](https://github.com/socketry/bake-cargo-rust).

### Agent Context

Run `cargo bake agent:context:install` to install shared context and skills. Read `.agents/context/index.md` to find relevant guides, follow `agents.md` if present, and apply skills under `.agents/skills/`. The installer preserves repository-owned `agents.md`; it does not create or regenerate that file.
