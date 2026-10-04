// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Cargo project and release tasks for Bake.
//!
//! The crate name supplies the `cargo` task namespace. Nested modules group
//! setup, trusted-publishing, and version operations; the crate also provides a
//! GitHub release task using `releases.md`.
#[path = "cargo.rs"]
mod cargo_support;
mod crates_io;
mod github;
mod release;
#[path = "version.rs"]
mod version_support;

#[cfg(test)]
mod test_support;

use bake_license as _;
use bake_releases as _;

use bake::{Context, Error, Result, Value};
use serde_json::json;
use std::fs;
use std::path::PathBuf;

use crate::{cargo_support as cargo_helpers, github as github_helpers};

/// Validate and package a release candidate before opening a reviewed pull request.
#[bake::task]
pub fn release(context: &mut Context) -> Result<Value> {
    let version = crate::version_support::workspace_version(context)?;
    crate::release::prepare(context, &version)
}

/// List publishable packages in the current Cargo workspace.
#[bake::task]
pub fn packages(context: &mut Context) -> Result<Value> {
    Ok(json!(cargo_helpers::workspace_packages(context)?))
}

/// Build and validate the package archive for one workspace package.
#[bake::task(name = "cargo:package")]
pub fn create_package_archive(context: &mut Context, package: String) -> Result<String> {
    cargo_helpers::run_cargo(context, ["package", "--locked", "--package", &package])?;
    Ok(format!("Packaged {package}"))
}

/// Publish one workspace package to crates.io using the configured Cargo credentials.
#[bake::task]
pub fn publish(context: &mut Context, package: String) -> Result<String> {
    cargo_helpers::run_cargo(context, ["publish", "--locked", "--package", &package])?;
    Ok(format!("Published {package}"))
}

/// Publish a package once, then register this repository's trusted publisher.
///
/// If publisher registration fails after Cargo accepts the upload, the package
/// remains published; rerun `cargo:trusted-publishing:configure` to finish setup.
#[bake::task]
pub fn bootstrap(
    context: &mut Context,
    package: String,
    #[bake(default = "publish.yml")] workflow: String,
    #[bake(default = "crates-io")] environment: String,
) -> Result<Value> {
    cargo_helpers::package_by_name(context, &package)?;
    let repository = github_helpers::Repository::from_origin(context)?;
    crates_io::validate_trusted_publisher_inputs(&package, &workflow, &environment)?;
    cargo_helpers::run_cargo(context, ["publish", "--locked", "--package", &package])
        .map_err(|error| Error::new(format!("initial crates.io publication failed: {error}")))?;

    let configuration = crates_io::configure_trusted_publisher(
            &package,
            &repository,
            &workflow,
            &environment,
        )
        .map_err(|error| {
            Error::new(format!(
                "{package} was published, but trusted publisher setup failed: {error}; rerun `cargo:trusted-publishing:configure {package}`"
            ))
        })?;

    Ok(json!({
        "package": package,
        "initial_publish": "complete",
        "trusted_publisher": configuration,
        "trusted_publishing_only": false,
        "next": format!("Review the workflow and publisher configuration, then run cargo:trusted-publishing:require {package} --required true when ready."),
    }))
}

/// Generate or update the GitHub Actions workflow for workspace package publication.
/// Existing files are preserved unless `--force true` is supplied.
pub mod setup {
    use super::*;

    /// Write `.github/workflows/publish.yml` for the current workspace.
    #[bake::task]
    pub fn workflow(
        context: &mut Context,
        #[bake(default = "publish.yml")] filename: String,
        #[bake(default = "main")] branch: String,
        #[bake(default = false)] force: bool,
    ) -> Result<String> {
        let packages = cargo_helpers::workspace_packages(context)?;
        if packages.is_empty() {
            return Err(Error::new("the workspace has no publishable packages"));
        }
        github_helpers::validate_branch(&branch)?;
        if PathBuf::from(&filename).components().count() != 1
            || filename.is_empty()
            || !filename.ends_with(".yml") && !filename.ends_with(".yaml")
        {
            return Err(Error::new(
                "workflow filename must be a single .yml or .yaml filename",
            ));
        }

        let contents = cargo_helpers::publish_workflow(&packages, &branch)?;
        let workflow_directory = context.root().join(".github").join("workflows");
        fs::create_dir_all(&workflow_directory)?;
        let path = workflow_directory.join(filename);

        if path.exists() {
            let existing = fs::read_to_string(&path)?;
            if existing == contents {
                return Ok(format!(
                    "{} already matches the generated workflow",
                    path.display()
                ));
            }
            if !force {
                return Err(Error::new(format!(
                    "{} already exists and differs; review it, or pass --force true to replace it",
                    path.display()
                )));
            }
        }

        fs::write(&path, contents)?;
        Ok(format!(
            "Generated {} for {} package(s)",
            path.display(),
            packages.len()
        ))
    }

    /// GitHub repository ruleset and environment setup tasks.
    pub mod github {
        use super::super::*;

        /// Show the desired repository rulesets and publishing environment.
        #[bake::task]
        #[allow(clippy::too_many_arguments)]
        pub fn plan(
            context: &mut Context,
            #[bake(default = "")] repository: String,
            #[bake(default = "main")] branch: String,
            #[bake(default = 1)] approvals: u32,
            checks: Vec<String>,
            reviewers: Vec<String>,
            wait_timer: Option<u32>,
            #[bake(default = "crates-io")] environment: String,
        ) -> Result<Value> {
            let repository = github_helpers::Repository::resolve(context, &repository)?;
            let reviewers = if reviewers.is_empty() {
                cargo_helpers::release_reviewers(context)?
            } else {
                reviewers
            };
            github_helpers::validate_setup(
                &branch,
                approvals,
                &checks,
                &reviewers,
                wait_timer,
                &environment,
            )?;
            let packages = cargo_helpers::workspace_packages(context)?;
            if packages.is_empty() {
                return Err(Error::new("the workspace has no publishable packages"));
            }
            let checks = github_helpers::effective_checks(&checks);
            let reviewer_set = github_helpers::resolve_reviewers(context, &repository, &reviewers)?;
            Ok(github_helpers::setup_plan(
                &repository,
                &branch,
                approvals,
                &checks,
                &reviewer_set,
                wait_timer,
                &environment,
            ))
        }

        /// Apply the managed rulesets and create/update the publishing environment.
        /// Run `cargo:setup:github:plan` first and review its output.
        #[bake::task]
        #[allow(clippy::too_many_arguments)]
        pub fn apply(
            context: &mut Context,
            #[bake(default = "")] repository: String,
            #[bake(default = "main")] branch: String,
            #[bake(default = 1)] approvals: u32,
            checks: Vec<String>,
            reviewers: Vec<String>,
            wait_timer: Option<u32>,
            #[bake(default = "crates-io")] environment: String,
        ) -> Result<Value> {
            let repository = github_helpers::Repository::resolve(context, &repository)?;
            let reviewers = if reviewers.is_empty() {
                cargo_helpers::release_reviewers(context)?
            } else {
                reviewers
            };
            github_helpers::validate_setup(
                &branch,
                approvals,
                &checks,
                &reviewers,
                wait_timer,
                &environment,
            )?;
            let checks = github_helpers::effective_checks(&checks);
            let reviewer_set = github_helpers::resolve_reviewers(context, &repository, &reviewers)?;
            github_helpers::apply_setup(
                context,
                &repository,
                &branch,
                approvals,
                &checks,
                &reviewer_set.resolved,
                wait_timer,
                &environment,
            )
        }
    }
}

/// Configure crates.io's GitHub Actions trusted publisher for one package.
pub mod trusted_publishing {
    use super::*;

    /// Show the trusted publisher configuration that would be registered.
    #[bake::task]
    pub fn plan(
        context: &mut Context,
        package: String,
        #[bake(default = "publish.yml")] workflow: String,
        #[bake(default = "crates-io")] environment: String,
    ) -> Result<Value> {
        cargo_helpers::package_by_name(context, &package)?;
        let repository = github_helpers::Repository::from_origin(context)?;
        crates_io::trusted_publisher_plan(&package, &repository, &workflow, &environment)
    }

    /// Add the GitHub Actions trusted publisher configuration on crates.io.
    #[bake::task]
    pub fn configure(
        context: &mut Context,
        package: String,
        #[bake(default = "publish.yml")] workflow: String,
        #[bake(default = "crates-io")] environment: String,
    ) -> Result<Value> {
        cargo_helpers::package_by_name(context, &package)?;
        let repository = github_helpers::Repository::from_origin(context)?;
        crates_io::configure_trusted_publisher(&package, &repository, &workflow, &environment)
    }

    /// Enable or disable crates.io's trusted-publishing-only requirement.
    #[bake::task]
    pub fn require(
        context: &mut Context,
        package: String,
        #[bake(default = true)] required: bool,
    ) -> Result<Value> {
        cargo_helpers::package_by_name(context, &package)?;
        crates_io::set_trusted_publishing_only(&package, required)
    }
}

/// Change the shared stable version of the publishable Cargo workspace packages.
pub mod version {
    use bake::{Context, Error, Result, Value};

    fn run_after_version_bump(context: &mut Context, result: &Value) -> Result<()> {
        let version = result
            .get("version")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new("version task did not return the new workspace version"))?;

        context.call_if_registered("cargo:after_version_bump", &[version])?;
        Ok(())
    }

    /// Increment the patch component of the workspace version.
    #[bake::task]
    pub fn patch(context: &mut Context) -> Result<Value> {
        let result =
            crate::version_support::increment(context, crate::version_support::Component::Patch)?;
        run_after_version_bump(context, &result)?;
        Ok(result)
    }

    /// Increment the minor component and reset patch to zero.
    #[bake::task]
    pub fn minor(context: &mut Context) -> Result<Value> {
        let result =
            crate::version_support::increment(context, crate::version_support::Component::Minor)?;
        run_after_version_bump(context, &result)?;
        Ok(result)
    }

    /// Increment the major component and reset minor and patch to zero.
    #[bake::task]
    pub fn major(context: &mut Context) -> Result<Value> {
        let result =
            crate::version_support::increment(context, crate::version_support::Component::Major)?;
        run_after_version_bump(context, &result)?;
        Ok(result)
    }

    /// Set the workspace to an explicit stable version greater than its current version.
    #[bake::task]
    pub fn bump(
        context: &mut Context,
        #[bake(
            named,
            help = "New stable workspace version in MAJOR.MINOR.PATCH form."
        )]
        version: String,
    ) -> Result<Value> {
        let result = crate::version_support::set(context, &version)?;
        run_after_version_bump(context, &result)?;
        Ok(result)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use bake::Registry;

        #[bake::task(name = "cargo:after_version_bump")]
        fn capture_version(context: &mut Context, version: String) -> Result<()> {
            context.insert(version);
            Ok(())
        }

        #[bake::task(name = "cargo:after_version_bump")]
        fn fail_version_hook(context: &mut Context, _version: String) -> Result<()> {
            let _ = context;
            Err(Error::new("hook failed"))
        }

        #[test]
        fn hook_is_optional() {
            let mut context = Registry::new().context(".");
            let result = serde_json::json!({"version": "1.2.3"});

            run_after_version_bump(&mut context, &result).unwrap();
            assert!(context.get::<String>().is_none());
        }

        #[test]
        fn hook_receives_the_new_workspace_version() {
            let mut registry = Registry::new();
            registry.register(capture_version_task()).unwrap();
            let mut context = registry.context(".");
            let result = serde_json::json!({"version": "1.2.3"});

            run_after_version_bump(&mut context, &result).unwrap();
            assert_eq!(context.get::<String>().map(String::as_str), Some("1.2.3"));
        }

        #[test]
        fn rejects_missing_bump_versions_and_propagates_hook_errors() {
            let mut context = Registry::new().context(".");
            assert!(run_after_version_bump(&mut context, &serde_json::json!({})).is_err());

            let mut registry = Registry::new();
            registry.register(fail_version_hook_task()).unwrap();
            let mut context = registry.context(".");
            assert!(
                run_after_version_bump(&mut context, &serde_json::json!({"version": "1.2.3"}))
                    .unwrap_err()
                    .to_string()
                    .contains("hook failed")
            );
        }
    }
}

/// GitHub release creation using notes from the matching `releases.md` heading.
pub mod releases;

#[cfg(test)]
mod task_tests {
    use super::*;
    use crate::test_support::{Environment, Project};
    use bake::{Registry, Result};
    use std::process::Command;

    fn project() -> Project {
        let project = Project::new();
        project.single_package("fixture", "1.2.3");
        project.write(
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"1.2.3\"\nedition = \"2024\"\n\n[workspace]\nmembers = [\"bake\"]\nresolver = \"3\"\n\n[workspace.metadata.bake.release]\nreviewers = [\"User:123\"]\n",
        );
        project.write(
            "bake/Cargo.toml",
            "[package]\nname = \"fixture-bake\"\nversion = \"0.0.0\"\nedition = \"2024\"\npublish = false\n",
        );
        project.write("bake/src/lib.rs", "// fixture\n");
        project.write(
            "releases.md",
            "# Releases\n\n## v1.2.3\n\nInitial release.\n",
        );
        project
    }

    fn git_origin(project: &Project) {
        assert!(
            Command::new("git")
                .current_dir(project.root())
                .args(["init", "--quiet"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .current_dir(project.root())
                .args([
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/socketry/fixture.git"
                ])
                .status()
                .unwrap()
                .success()
        );
    }

    fn install_gh(project: &Project, environment: &mut Environment, script: &str) {
        project.executable("gh", script);
        environment.prepend_path(&project.root().join("bin"));
    }

    fn test_api(environment: &mut Environment, api: &str) {
        environment.set("CARGO_REGISTRY_TOKEN", "test-token");
        environment.set("BAKE_TEST_CRATES_IO_API", api);
    }

    #[bake::task(name = "cargo:after_version_bump")]
    fn record_version(context: &mut bake::Context, version: String) -> Result<()> {
        context.insert(version);
        Ok(())
    }

    #[test]
    fn exposes_package_release_and_direct_cargo_tasks() {
        let mut environment = Environment::new();
        let project = project();
        project.cargo_proxy(&mut environment, None);
        let mut context = project.context();

        let packages = packages(&mut context).unwrap();
        assert_eq!(packages[0]["name"], "fixture");
        assert_eq!(
            create_package_archive(&mut context, "fixture".to_owned()).unwrap(),
            "Packaged fixture"
        );
        assert_eq!(
            publish(&mut context, "fixture".to_owned()).unwrap(),
            "Published fixture"
        );
        let prepared = release(&mut context).unwrap();
        assert_eq!(prepared["version"], "1.2.3");
        assert!(prepared["packages"][0].as_str() == Some("fixture"));
        assert!(
            project
                .cargo_arguments()
                .contains("package --locked --package fixture")
        );
    }

    #[test]
    fn propagates_cargo_packaging_and_publish_failures() {
        let mut environment = Environment::new();
        let project = project();
        project.cargo_proxy(&mut environment, Some("package"));
        let mut context = project.context();
        assert!(create_package_archive(&mut context, "fixture".to_owned()).is_err());

        environment.set("BAKE_TEST_CARGO_FAILURE", "publish");
        assert!(publish(&mut context, "fixture".to_owned()).is_err());
    }

    #[test]
    fn creates_workflow_then_preserves_or_replaces_existing_content() {
        let mut environment = Environment::new();
        let project = project();
        project.cargo_proxy(&mut environment, None);
        let mut context = project.context();

        let generated = setup::workflow(
            &mut context,
            "publish.yml".to_owned(),
            "main".to_owned(),
            false,
        )
        .unwrap();
        assert!(generated.contains("Generated"));
        let path = project.root().join(".github/workflows/publish.yml");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("      - main\n"));
        assert!(
            setup::workflow(
                &mut context,
                "publish.yml".to_owned(),
                "main".to_owned(),
                false
            )
            .unwrap()
            .contains("already matches")
        );

        std::fs::write(&path, "owner content\n").unwrap();
        assert!(
            setup::workflow(
                &mut context,
                "publish.yml".to_owned(),
                "main".to_owned(),
                false
            )
            .is_err()
        );
        assert!(
            setup::workflow(
                &mut context,
                "publish.yml".to_owned(),
                "main".to_owned(),
                true
            )
            .unwrap()
            .contains("Generated")
        );
        assert!(
            std::fs::read_to_string(path)
                .unwrap()
                .contains("cargo bake --locked")
        );

        assert!(
            setup::workflow(
                &mut context,
                "../publish.yml".to_owned(),
                "main".to_owned(),
                false
            )
            .is_err()
        );
        assert!(
            setup::workflow(
                &mut context,
                "invalid.txt".to_owned(),
                "main".to_owned(),
                false
            )
            .is_err()
        );
        assert!(
            setup::workflow(
                &mut context,
                "other.yml".to_owned(),
                "feature/release".to_owned(),
                false
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_empty_or_inconsistently_versioned_publish_workspaces() {
        let mut environment = Environment::new();
        let empty = Project::new();
        empty.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"private\"]\nresolver = \"3\"\n",
        );
        empty.write("private/Cargo.toml", "[package]\nname = \"private\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n");
        empty.write("private/src/lib.rs", "// private\n");
        empty.cargo_proxy(&mut environment, None);
        let mut empty_context = empty.context();
        assert!(
            setup::workflow(
                &mut empty_context,
                "publish.yml".to_owned(),
                "main".to_owned(),
                false
            )
            .is_err()
        );
        assert!(
            packages(&mut empty_context)
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            setup::github::plan(
                &mut empty_context,
                "socketry/fixture".to_owned(),
                "main".to_owned(),
                1,
                Vec::new(),
                vec!["User:123".to_owned()],
                None,
                "crates-io".to_owned(),
            )
            .unwrap_err()
            .to_string()
            .contains("no publishable packages")
        );

        let mixed = Project::new();
        mixed.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"first\", \"second\"]\nresolver = \"3\"\n",
        );
        mixed.write(
            "first/Cargo.toml",
            "[package]\nname = \"first\"\nversion = \"1.2.3\"\nedition = \"2024\"\n",
        );
        mixed.write("first/src/lib.rs", "// first\n");
        mixed.write(
            "second/Cargo.toml",
            "[package]\nname = \"second\"\nversion = \"1.2.4\"\nedition = \"2024\"\n",
        );
        mixed.write("second/src/lib.rs", "// second\n");
        mixed.cargo_proxy(&mut environment, None);
        let mut mixed_context = mixed.context();
        assert!(
            setup::workflow(
                &mut mixed_context,
                "publish.yml".to_owned(),
                "main".to_owned(),
                false
            )
            .unwrap_err()
            .to_string()
            .contains("share one version")
        );
    }

    #[test]
    fn plans_github_setup_using_workspace_reviewers_and_explicit_repositories() {
        let mut environment = Environment::new();
        let project = project();
        project.cargo_proxy(&mut environment, None);
        let mut context = project.context();

        let plan = setup::github::plan(
            &mut context,
            "socketry/fixture".to_owned(),
            "main".to_owned(),
            1,
            Vec::new(),
            Vec::new(),
            None,
            "crates-io".to_owned(),
        )
        .unwrap();
        assert_eq!(plan["repository"], "socketry/fixture");
        assert_eq!(plan["reviewers"]["resolved"][0], "User:123");
        assert_eq!(
            plan["branch_ruleset"]["rules"][1]["parameters"]["required_status_checks"][0]["context"],
            "check"
        );

        assert!(
            setup::github::plan(
                &mut context,
                "socketry/fixture".to_owned(),
                "main".to_owned(),
                7,
                Vec::new(),
                vec!["User:123".to_owned()],
                None,
                "crates-io".to_owned(),
            )
            .is_err()
        );
    }

    #[test]
    fn applies_github_setup_using_the_generated_workspace_metadata() {
        let mut environment = Environment::new();
        let project = project();
        project.cargo_proxy(&mut environment, None);
        install_gh(
            &project,
            &mut environment,
            "#!/bin/sh\ncase \"$3 $4\" in\n  'GET repos/socketry/fixture/environments?per_page=100') printf '%s' '{\"environments\":[]}' ;;\n  'GET repos/socketry/fixture/rulesets?per_page=100') printf '%s' '[]' ;;\n  'POST repos/socketry/fixture/rulesets') cat >/dev/null; printf '%s' '{}' ;;\n  'PUT repos/socketry/fixture/environments/crates-io') cat >/dev/null; printf '%s' '{}' ;;\n  *) echo unexpected-request >&2; exit 1 ;;\nesac\n",
        );
        let mut context = project.context();
        let result = setup::github::apply(
            &mut context,
            "socketry/fixture".to_owned(),
            "main".to_owned(),
            1,
            Vec::new(),
            Vec::new(),
            None,
            "crates-io".to_owned(),
        )
        .unwrap();
        assert!(result.get("branch_ruleset").is_some());

        let result = setup::github::apply(
            &mut context,
            "socketry/fixture".to_owned(),
            "main".to_owned(),
            1,
            Vec::new(),
            vec!["User:123".to_owned()],
            None,
            "crates-io".to_owned(),
        )
        .unwrap();
        assert!(result.get("environment").is_some());

        assert!(
            setup::github::apply(
                &mut context,
                "socketry/fixture".to_owned(),
                "main".to_owned(),
                7,
                Vec::new(),
                vec!["User:123".to_owned()],
                None,
                "crates-io".to_owned(),
            )
            .unwrap_err()
            .to_string()
            .contains("between zero and six")
        );
    }

    #[test]
    fn plans_configures_and_requires_trusted_publishing_through_local_registry_api() {
        let mut environment = Environment::new();
        let project = project();
        git_origin(&project);
        project.cargo_proxy(&mut environment, None);
        let mut context = project.context();
        let plan = trusted_publishing::plan(
            &mut context,
            "fixture".to_owned(),
            "publish.yml".to_owned(),
            "crates-io".to_owned(),
        )
        .unwrap();
        assert_eq!(plan["github_config"]["repository_owner"], "socketry");

        let (api, server) = crate::crates_io::tests::mock_server(vec![
            (200, r#"{"github_configs":[]}"#),
            (
                201,
                r#"{"github_config":{"id":3,"crate":"fixture","repository_owner":"socketry","repository_name":"fixture","workflow_filename":"publish.yml","environment":"crates-io"}}"#,
            ),
            (200, r#"{"crate":{"trustpub_only":true}}"#),
        ]);
        test_api(&mut environment, &api);
        let configured = trusted_publishing::configure(
            &mut context,
            "fixture".to_owned(),
            "publish.yml".to_owned(),
            "crates-io".to_owned(),
        )
        .unwrap();
        let required =
            trusted_publishing::require(&mut context, "fixture".to_owned(), true).unwrap();
        let requests = server.join().unwrap();
        assert_eq!(configured["status"], "created");
        assert_eq!(required["trustpub_only"], true);
        assert_eq!(requests.len(), 3);
    }

    #[test]
    fn bootstraps_registry_publishing_after_the_initial_upload() {
        let mut environment = Environment::new();
        let project = project();
        git_origin(&project);
        project.cargo_proxy(&mut environment, None);
        let mut context = project.context();
        let (api, server) = crate::crates_io::tests::mock_server(vec![
            (200, r#"{"github_configs":[]}"#),
            (
                201,
                r#"{"github_config":{"id":3,"crate":"fixture","repository_owner":"socketry","repository_name":"fixture","workflow_filename":"publish.yml","environment":"crates-io"}}"#,
            ),
        ]);
        test_api(&mut environment, &api);

        let result = bootstrap(
            &mut context,
            "fixture".to_owned(),
            "publish.yml".to_owned(),
            "crates-io".to_owned(),
        )
        .unwrap();
        assert_eq!(result["initial_publish"], "complete");
        assert_eq!(result["trusted_publisher"]["status"], "created");
        assert!(
            project
                .cargo_arguments()
                .contains("publish --locked --package fixture")
        );
        assert_eq!(server.join().unwrap().len(), 2);
    }

    #[test]
    fn refuses_unsafe_bootstrap_inputs_and_reports_partial_publication() {
        let mut environment = Environment::new();
        environment.remove("CARGO_REGISTRY_TOKEN");
        let project = project();
        git_origin(&project);
        project.cargo_proxy(&mut environment, Some("publish"));
        let mut context = project.context();
        assert!(
            bootstrap(
                &mut context,
                "unknown".to_owned(),
                "publish.yml".to_owned(),
                "crates-io".to_owned(),
            )
            .unwrap_err()
            .to_string()
            .contains("was not found")
        );
        assert!(
            bootstrap(
                &mut context,
                "fixture".to_owned(),
                "../publish.yml".to_owned(),
                "crates-io".to_owned(),
            )
            .is_err()
        );

        environment.set("CARGO_REGISTRY_TOKEN", "test-token");
        let error = bootstrap(
            &mut context,
            "fixture".to_owned(),
            "publish.yml".to_owned(),
            "crates-io".to_owned(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("initial crates.io publication failed")
        );

        environment.set("BAKE_TEST_CARGO_FAILURE", "");
        let (api, server) =
            crate::crates_io::tests::mock_server(vec![(403, "registry denied setup")]);
        test_api(&mut environment, &api);
        let error = bootstrap(
            &mut context,
            "fixture".to_owned(),
            "publish.yml".to_owned(),
            "crates-io".to_owned(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("was published, but trusted publisher setup failed")
        );
        assert!(error.to_string().contains("registry denied setup"));
        server.join().unwrap();
    }

    #[test]
    fn invokes_the_after_version_bump_hook_for_each_version_task() {
        let mut environment = Environment::new();
        let project = project();
        project.cargo_proxy(&mut environment, None);
        let mut registry = Registry::new();
        registry.register(record_version_task()).unwrap();
        let mut context = registry.context(project.root());

        assert_eq!(version::patch(&mut context).unwrap()["version"], "1.2.4");
        assert_eq!(context.get::<String>().map(String::as_str), Some("1.2.4"));
        assert_eq!(version::minor(&mut context).unwrap()["version"], "1.3.0");
        assert_eq!(version::major(&mut context).unwrap()["version"], "2.0.0");
        assert_eq!(
            version::bump(&mut context, "2.3.1".to_owned()).unwrap()["version"],
            "2.3.1"
        );
        assert_eq!(context.get::<String>().map(String::as_str), Some("2.3.1"));
    }

    #[test]
    fn reports_release_preparation_errors_without_running_cargo_package() {
        let mut environment = Environment::new();
        let project = project();
        project.cargo_proxy(&mut environment, None);
        let mut context = project.context();
        std::fs::remove_file(project.root().join("releases.md")).unwrap();
        assert!(
            release(&mut context)
                .unwrap_err()
                .to_string()
                .contains("could not read")
        );

        project.write("releases.md", "# Releases\n\n## v1.2.2\n\nWrong version.\n");
        assert!(
            release(&mut context)
                .unwrap_err()
                .to_string()
                .contains("must contain a")
        );
    }

    #[test]
    fn propagates_all_version_task_errors_for_an_empty_workspace() {
        let mut environment = Environment::new();
        let project = Project::new();
        project.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"private\"]\nresolver = \"3\"\n",
        );
        project.write(
            "private/Cargo.toml",
            "[package]\nname = \"private\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
        );
        project.write("private/src/lib.rs", "// private\n");
        project.cargo_proxy(&mut environment, None);
        let mut context = project.context();

        assert!(version::patch(&mut context).is_err());
        assert!(version::minor(&mut context).is_err());
        assert!(version::major(&mut context).is_err());
        assert!(version::bump(&mut context, "2.0.0".to_owned()).is_err());
    }
}
