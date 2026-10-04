// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use bake::{Context, Error, Result, Value};
use serde_json::json;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item};

use crate::cargo_support::{WorkspacePackage, run_cargo_arguments, workspace_packages};
use crate::version_support::{Version, workspace_version};

/// Detect a version change suitable for the standard publishing workflow.
#[bake::task(name = "cargo:release:detect")]
pub fn detect_release(context: &mut Context, base: String, sha: String) -> Result<Value> {
    if sha.is_empty() || sha.chars().any(char::is_control) {
        return Err(Error::new("release commit must be a nonempty Git revision"));
    }

    let version = workspace_version(context)?;
    let current = Version::parse(&version)?;
    let previous = previous_versions(context, &base)?;
    let changed = match previous.as_deref() {
        Some(previous) => {
            let previous = Version::parse(previous)?;
            if current < previous {
                return Err(Error::new(format!(
                    "workspace version {current} is lower than previous version {previous}"
                )));
            }
            current > previous
        }
        None => true,
    };

    if changed {
        if !crate::release::contains_release_heading(
            &std::fs::read_to_string(context.root().join("releases.md"))
                .map_err(|error| Error::new(format!("could not read releases.md: {error}")))?,
            &version,
        ) {
            return Err(Error::new(format!(
                "releases.md must contain a '## v{version}' heading"
            )));
        }

        let tag = format!("v{version}");
        let reference = format!("refs/tags/{tag}");
        let output = context
            .command("git")
            .args(["rev-list", "-n", "1", reference.as_str()])
            .output()?;
        if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() != sha {
            return Err(Error::new(format!(
                "tag {tag} already exists at a different commit"
            )));
        }
    }

    let version = if changed { version } else { String::new() };
    append_github_output("release", if changed { "true" } else { "false" })?;
    append_github_output("version", &version)?;

    Ok(json!({"release": changed, "version": version}))
}

/// Check whether any publishable workspace package still needs uploading.
#[bake::task(name = "cargo:publish:pending")]
pub fn pending_packages(context: &mut Context, version: String) -> Result<Value> {
    let (pending, _) = publication_state(context, &version)?;
    let names: Vec<_> = pending
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    let has_packages = !pending.is_empty();
    append_github_output("has_packages", if has_packages { "true" } else { "false" })?;

    Ok(json!({"has_packages": has_packages, "packages": names}))
}

/// Publish remaining workspace packages, create the release tag, and sync the GitHub Release.
#[bake::task(name = "cargo:release:publish")]
pub fn publish_release(context: &mut Context, version: String, sha: String) -> Result<Value> {
    if sha.is_empty() || sha.chars().any(char::is_control) {
        return Err(Error::new("release commit must be a nonempty Git revision"));
    }

    let (pending, published) = publication_state(context, &version)?;
    let mut published_packages = Vec::new();
    if !pending.is_empty() {
        let mut arguments = vec![
            "publish".to_owned(),
            "--workspace".to_owned(),
            "--locked".to_owned(),
        ];
        for package in &published {
            arguments.extend(["--exclude".to_owned(), package.name.clone()]);
        }
        run_cargo_arguments(context, &arguments)?;
        published_packages.extend(pending.iter().map(|package| package.name.as_str()));
    }

    let tag = format!("v{version}");
    create_release_tag(context, &tag, &sha)?;
    let url = crate::releases::github::release(
        context,
        tag.clone(),
        PathBuf::from("releases.md"),
        false,
    )?;

    Ok(json!({
        "tag": tag,
        "published": published_packages,
        "already_published": published.iter().map(|package| package.name.as_str()).collect::<Vec<_>>(),
        "github_release": url,
    }))
}

fn publication_state(
    context: &Context,
    version: &str,
) -> Result<(Vec<WorkspacePackage>, Vec<WorkspacePackage>)> {
    let expected = Version::parse(version)?;
    let packages = workspace_packages(context)?;
    if packages.is_empty() {
        return Err(Error::new("the workspace has no publishable packages"));
    }

    for package in &packages {
        let package_version = Version::parse(&package.version)?;
        if package_version != expected {
            return Err(Error::new(format!(
                "{} has version {}, expected {version}",
                package.name, package.version
            )));
        }
    }

    let mut pending = Vec::new();
    let mut published = Vec::new();
    for package in packages {
        if crate::crates_io::version_is_published(&package.name, version)? {
            published.push(package);
        } else {
            pending.push(package);
        }
    }

    Ok((pending, published))
}

fn previous_versions(context: &Context, base: &str) -> Result<Option<String>> {
    if base.is_empty() || base.bytes().all(|byte| byte == b'0') {
        return Ok(None);
    }

    let root_manifest = context.root().join("Cargo.toml");
    let root_path = root_manifest
        .strip_prefix(context.root())
        .map_err(|_| Error::new("workspace Cargo.toml is outside the project root"))?;
    let root_source = revision_file(context, base, root_path, true)?
        .ok_or_else(|| Error::new("could not read the previous workspace Cargo.toml"))?;
    let root_document = parse_manifest(&root_source, "previous workspace Cargo.toml")?;
    let previous_workspace_version = root_document
        .get("workspace")
        .and_then(Item::as_table)
        .and_then(|workspace| workspace.get("package"))
        .and_then(Item::as_table)
        .and_then(|package| package.get("version"))
        .and_then(Item::as_str)
        .map(str::to_owned);

    let mut versions = Vec::new();
    for package in workspace_packages(context)? {
        let manifest = Path::new(&package.manifest_path);
        let relative = manifest.strip_prefix(context.root()).map_err(|_| {
            Error::new(format!(
                "workspace package manifest {} is outside the project root",
                manifest.display()
            ))
        })?;
        let Some(source) = revision_file(context, base, relative, false)? else {
            continue;
        };
        let document = parse_manifest(&source, &relative.display().to_string())?;
        if let Some(version) = package_version(&document, previous_workspace_version.as_deref()) {
            versions.push(version);
        }
    }

    let unique: std::collections::BTreeSet<_> = versions.into_iter().collect();
    if unique.len() > 1 {
        return Err(Error::new(format!(
            "previous publishable packages did not share one version: {}",
            unique.into_iter().collect::<Vec<_>>().join(", ")
        )));
    }

    Ok(unique.into_iter().next())
}

fn revision_file(
    context: &Context,
    revision: &str,
    path: &Path,
    required: bool,
) -> Result<Option<String>> {
    let object = format!("{revision}:{}", path.display());
    let output = context.command("git").args(["show", &object]).output()?;
    if !output.status.success() {
        if required {
            return Err(Error::new(format!(
                "could not read {object}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        return Ok(None);
    }

    String::from_utf8(output.stdout)
        .map(Some)
        .map_err(|error| Error::new(format!("{object} is not UTF-8: {error}")))
}

fn parse_manifest(source: &str, description: &str) -> Result<DocumentMut> {
    source
        .parse()
        .map_err(|error| Error::new(format!("could not parse {description}: {error}")))
}

fn package_version(document: &DocumentMut, workspace_version: Option<&str>) -> Option<String> {
    let version = document
        .get("package")
        .and_then(Item::as_table_like)?
        .get("version")?;
    if let Some(version) = version.as_str() {
        return Some(version.to_owned());
    }

    if version
        .as_table_like()
        .and_then(|table| table.get("workspace"))
        .and_then(Item::as_bool)
        == Some(true)
    {
        return workspace_version.map(str::to_owned);
    }

    None
}

fn create_release_tag(context: &Context, tag: &str, sha: &str) -> Result<()> {
    let reference = format!("refs/tags/{tag}");
    let output = context
        .command("git")
        .args(["rev-list", "-n", "1", reference.as_str()])
        .output()?;
    if output.status.success() {
        if String::from_utf8_lossy(&output.stdout).trim() != sha {
            return Err(Error::new(format!(
                "tag {tag} already exists at a different commit"
            )));
        }
    } else {
        run_git(
            context,
            &[
                "config".to_owned(),
                "user.name".to_owned(),
                "github-actions[bot]".to_owned(),
            ],
        )?;
        run_git(
            context,
            &[
                "config".to_owned(),
                "user.email".to_owned(),
                "41898282+github-actions[bot]@users.noreply.github.com".to_owned(),
            ],
        )?;
        run_git(
            context,
            &[
                "tag".to_owned(),
                "-a".to_owned(),
                tag.to_owned(),
                sha.to_owned(),
                "-m".to_owned(),
                format!("Release {tag}"),
            ],
        )?;
    }

    run_git(
        context,
        &["push".to_owned(), "origin".to_owned(), reference],
    )
}

fn run_git(context: &Context, arguments: &[String]) -> Result<()> {
    let output = context.command("git").args(arguments).output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::new(format!(
            "git {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn append_github_output(name: &str, value: &str) -> Result<()> {
    let Some(path) = std::env::var_os("GITHUB_OUTPUT") else {
        return Ok(());
    };

    let mut output = OpenOptions::new().append(true).create(true).open(path)?;
    writeln!(output, "{name}={value}")?;
    Ok(())
}
