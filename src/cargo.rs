// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use bake::{Context, Error, Result};
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct WorkspacePackage {
    pub name: String,
    pub version: String,
    pub manifest_path: String,
}

/// Read the GitHub environment reviewers configured in Cargo metadata.
/// Workspace metadata takes precedence, with package metadata as a fallback
/// for single-package projects.
pub(crate) fn release_reviewers(context: &Context) -> Result<Vec<String>> {
    let output = command(context, ["metadata", "--format-version", "1", "--no-deps"])?;
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| Error::new(format!("could not parse Cargo metadata: {error}")))?;

    reviewers_from_metadata(&metadata)
}

fn reviewers_from_metadata(metadata: &Value) -> Result<Vec<String>> {
    if let Some(reviewers) = metadata
        .get("metadata")
        .and_then(|value| value.get("bake"))
        .and_then(|value| value.get("release"))
        .and_then(|value| value.get("reviewers"))
    {
        return parse_reviewers(reviewers);
    }

    let workspace_root = metadata
        .get("workspace_root")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new("Cargo metadata did not contain a workspace root"))?;
    let root_manifest = std::path::Path::new(workspace_root).join("Cargo.toml");
    let root_package_reviewers = metadata
        .get("packages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|package| {
            package
                .get("manifest_path")
                .and_then(Value::as_str)
                .is_some_and(|manifest| std::path::Path::new(manifest) == root_manifest)
        })
        .and_then(|package| package.get("metadata"))
        .and_then(|value| value.get("bake"))
        .and_then(|value| value.get("release"))
        .and_then(|value| value.get("reviewers"));

    match root_package_reviewers {
        Some(reviewers) => parse_reviewers(reviewers),
        None => Ok(Vec::new()),
    }
}

fn parse_reviewers(value: &Value) -> Result<Vec<String>> {
    let reviewers = value
        .as_array()
        .ok_or_else(|| Error::new("Cargo metadata bake.release.reviewers must be an array"))?;
    reviewers
        .iter()
        .map(|reviewer| {
            reviewer.as_str().map(str::to_owned).ok_or_else(|| {
                Error::new("Cargo metadata bake.release.reviewers must contain strings")
            })
        })
        .collect()
}

pub(crate) fn workspace_packages(context: &Context) -> Result<Vec<WorkspacePackage>> {
    let output = command(context, ["metadata", "--format-version", "1", "--no-deps"])?;
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| Error::new(format!("could not parse Cargo metadata: {error}")))?;
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::new("Cargo metadata did not contain a packages array"))?;

    let mut result = Vec::new();
    for package in packages {
        let publishable = match package.get("publish") {
            None | Some(Value::Null) | Some(Value::Bool(true)) => true,
            Some(Value::Bool(false)) => false,
            Some(Value::Array(registries)) => registries
                .iter()
                .any(|registry| registry.as_str() == Some("crates-io")),
            Some(_) => true,
        };
        if !publishable {
            continue;
        }
        let name = package
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new("Cargo metadata package has no name"))?;
        let version = package
            .get("version")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new(format!("Cargo metadata package {name:?} has no version")))?;
        let manifest_path = package
            .get("manifest_path")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::new(format!(
                    "Cargo metadata package {name:?} has no manifest path"
                ))
            })?;
        result.push(WorkspacePackage {
            name: name.to_owned(),
            version: version.to_owned(),
            manifest_path: manifest_path.to_owned(),
        });
    }

    result.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(result)
}

pub(crate) fn package_by_name(context: &Context, name: &str) -> Result<WorkspacePackage> {
    workspace_packages(context)?
        .into_iter()
        .find(|package| package.name == name)
        .ok_or_else(|| {
            Error::new(format!(
                "publishable package {name:?} was not found in the workspace"
            ))
        })
}

pub(crate) fn run_cargo<const COUNT: usize>(
    context: &Context,
    arguments: [&str; COUNT],
) -> Result<()> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = context.command(cargo);
    command.args(arguments);
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(format!(
            "cargo {} failed: {status}",
            arguments.join(" ")
        )))
    }
}

pub(crate) fn run_cargo_arguments(context: &Context, arguments: &[String]) -> Result<()> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = context.command(cargo).args(arguments).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(format!(
            "cargo {} failed: {status}",
            arguments.join(" ")
        )))
    }
}

fn command<const COUNT: usize>(
    context: &Context,
    arguments: [&str; COUNT],
) -> Result<std::process::Output> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = context.command(cargo);
    let output = command.args(arguments).output()?;
    if !output.status.success() {
        return Err(Error::new(format!(
            "cargo {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output)
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn workspace_release_metadata_supplies_reviewers() {
        let metadata = json!({
            "workspace_root": "/project",
            "metadata": {"bake": {"release": {"reviewers": ["Team:123"]}}},
            "packages": []
        });

        assert_eq!(reviewers_from_metadata(&metadata).unwrap(), ["Team:123"]);
    }

    #[test]
    fn root_package_metadata_is_used_when_workspace_metadata_is_absent() {
        let metadata = json!({
            "workspace_root": "/project",
            "metadata": null,
            "packages": [{
                "manifest_path": "/project/Cargo.toml",
                "metadata": {"bake": {"release": {"reviewers": ["User:456"]}}}
            }]
        });

        assert_eq!(reviewers_from_metadata(&metadata).unwrap(), ["User:456"]);
    }

    #[test]
    fn workspace_metadata_takes_precedence_over_package_metadata() {
        let metadata = json!({
            "workspace_root": "/project",
            "metadata": {"bake": {"release": {"reviewers": ["Team:123"]}}},
            "packages": [{
                "manifest_path": "/project/Cargo.toml",
                "metadata": {"bake": {"release": {"reviewers": ["User:456"]}}}
            }]
        });

        assert_eq!(reviewers_from_metadata(&metadata).unwrap(), ["Team:123"]);
    }

    #[test]
    fn malformed_reviewer_metadata_is_rejected() {
        let metadata = json!({
            "workspace_root": "/project",
            "metadata": {"bake": {"release": {"reviewers": "Team:123"}}},
            "packages": []
        });

        assert!(reviewers_from_metadata(&metadata).is_err());
    }
}

pub(crate) fn publish_workflow(packages: &[WorkspacePackage], branch: &str) -> Result<String> {
    let first = packages
        .first()
        .ok_or_else(|| Error::new("the workspace has no publishable packages"))?;
    let version = first.version.as_str();
    if packages.iter().any(|package| package.version != version) {
        return Err(Error::new(
            "workspace release publishing requires every publishable package to share one version",
        ));
    }
    crate::github::validate_branch(branch)?;

    Ok(include_str!("publish.yml").replace("__BRANCH__", branch))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(name: &str, version: &str) -> WorkspacePackage {
        WorkspacePackage {
            name: name.to_owned(),
            version: version.to_owned(),
            manifest_path: format!("{name}/Cargo.toml"),
        }
    }

    #[test]
    fn generated_workflow_requires_a_reviewed_main_branch_release_change() {
        let workflow = publish_workflow(
            &[package("first", "1.2.3"), package("second", "1.2.3")],
            "main",
        )
        .unwrap();

        assert!(workflow.contains("cargo test --workspace --locked"));
        assert!(workflow.contains("id: detect"));
        assert!(workflow.contains("github.event.before || github.event.pull_request.base.sha"));
        assert!(workflow.contains("releases.md must contain a '## v{version}' heading"));
        assert!(workflow.contains("environment: crates-io"));
        assert!(workflow.contains("contents: write"));
        assert!(workflow.contains("Create release tag after successful publication"));
        assert!(workflow.contains("[\"git\", \"rev-list\", \"-n\", \"1\", f\"refs/tags/{tag}\"]"));
        assert!(!workflow.contains("tags:\n      - \"v*\""));
        assert!(workflow.contains("if any(item.get(\"num\") == os.environ[\"BAKE_VERSION\"]"));
        assert!(workflow.contains("existing.append(name)"));
        assert!(workflow.contains("if: steps.packages.outputs.has_packages == 'true'"));
        assert!(workflow.contains("[\"cargo\", \"publish\", \"--workspace\", \"--locked\"]"));
        assert!(workflow.contains("--exclude"));
        assert!(workflow.contains(
            "BAKE_BEFORE: ${{ github.event.before || github.event.pull_request.base.sha }}"
        ));
        assert!(!workflow.contains("__BRANCH__"));
        assert!(workflow.contains("      - main\n"));
    }

    #[test]
    fn generated_workflow_requires_a_shared_workspace_version() {
        assert!(
            publish_workflow(
                &[package("first", "1.2.3"), package("second", "1.2.4")],
                "main",
            )
            .is_err()
        );
    }
}
