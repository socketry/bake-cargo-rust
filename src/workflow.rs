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

    let current = workspace_version(context)?;
    let version = current.to_string();
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
    append_release_outputs(changed, &version, append_github_output)
}

fn append_release_outputs(
    changed: bool,
    version: &str,
    mut append: impl FnMut(&str, &str) -> Result<()>,
) -> Result<Value> {
    append("release", if changed { "true" } else { "false" })?;
    append("version", version)?;

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

    let root = context
        .root()
        .canonicalize()
        .map_err(|error| Error::new(format!("could not resolve project root: {error}")))?;
    let Some(root_source) = revision_file(context, base, Path::new("Cargo.toml"))? else {
        return Ok(None);
    };
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
        let relative = manifest.strip_prefix(&root).map_err(|_| {
            Error::new(format!(
                "workspace package manifest {} is outside the project root",
                manifest.display()
            ))
        })?;
        let Some(source) = revision_file(context, base, relative)? else {
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

fn revision_file(context: &Context, revision: &str, path: &Path) -> Result<Option<String>> {
    let object = format!("{revision}:{}", path.display());
    let output = context.command("git").args(["show", &object]).output()?;
    if !output.status.success() {
        return Ok(None);
    }

    match String::from_utf8(output.stdout) {
        Ok(source) => Ok(Some(source)),
        Err(error) => Err(Error::new(format!("{object} is not UTF-8: {error}"))),
    }
}

fn parse_manifest(source: &str, description: &str) -> Result<DocumentMut> {
    match source.parse::<DocumentMut>() {
        Ok(document) => Ok(document),
        Err(error) => Err(Error::new(format!(
            "could not parse {description}: {error}"
        ))),
    }
}

fn package_version(document: &DocumentMut, workspace_version: Option<&str>) -> Option<String> {
    let version = document
        .get("package")
        .and_then(Item::as_table_like)?
        .get("version")?;
    if let Some(version) = version.as_str() {
        return Some(version.to_owned());
    }

    let inherits_workspace_version = match version.as_table_like() {
        Some(table) => table.get("workspace").and_then(Item::as_bool) == Some(true),
        None => false,
    };
    if inherits_workspace_version {
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
    write_github_output(&mut output, name, value)
}

fn write_github_output(output: &mut impl Write, name: &str, value: &str) -> Result<()> {
    writeln!(output, "{name}={value}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Environment, Project, shell_quote};
    use bake::{Context, Registry};
    use std::fs;
    use std::process::{Command, Output};

    fn release_project(version: &str) -> Project {
        let project = Project::new();
        project.single_package("fixture", version);
        project.write(
            "releases.md",
            &format!("# Releases\n\n## v{version}\n\nRelease notes.\n"),
        );
        project
    }

    fn git(project: &Project, arguments: &[&str]) -> Output {
        Command::new("git")
            .current_dir(project.root())
            .args(arguments)
            .output()
            .unwrap()
    }

    fn git_ok(project: &Project, arguments: &[&str]) -> String {
        let output = git(project, arguments);
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn initialize_git(project: &Project, remote: bool) -> String {
        git_ok(project, &["init", "--quiet"]);
        git_ok(project, &["config", "user.name", "Test User"]);
        git_ok(project, &["config", "user.email", "test@example.com"]);
        git_ok(project, &["add", "-A"]);
        git_ok(project, &["commit", "--quiet", "-m", "fixture"]);

        if remote {
            let origin = project.root().join("origin.git");
            let output = Command::new("git")
                .current_dir(project.root())
                .args(["init", "--bare", "--quiet"])
                .arg(&origin)
                .output()
                .unwrap();
            assert!(output.status.success());
            let output = Command::new("git")
                .current_dir(project.root())
                .args(["remote", "add", "origin"])
                .arg(origin)
                .output()
                .unwrap();
            assert!(output.status.success());
        }

        git_ok(project, &["rev-parse", "HEAD"])
    }

    fn commit(project: &Project) -> String {
        git_ok(project, &["add", "-A"]);
        git_ok(project, &["commit", "--quiet", "-m", "update"]);
        git_ok(project, &["rev-parse", "HEAD"])
    }

    fn install_gh(project: &Project, environment: &mut Environment) {
        let notes = shell_quote(&project.root().join("gh-release-notes"));
        project.executable(
            "gh",
            &format!(
                "#!/bin/sh\nif [ \"$1 $2\" = 'release view' ]; then echo 'release not found' >&2; exit 1; fi\nif [ \"$1 $2\" = 'release create' ]; then cat > {notes}; echo 'https://github.com/socketry/fixture/releases/tag/v1.2.3'; exit 0; fi\necho unexpected-gh-command >&2; exit 1\n"
            ),
        );
        environment.prepend_path(&project.root().join("bin"));
    }

    fn set_release_output(environment: &mut Environment, project: &Project) -> PathBuf {
        let path = project.root().join("github-output");
        environment.set("GITHUB_OUTPUT", path.as_os_str());
        path
    }

    fn prepare_workspace(project: &Project, environment: &mut Environment) {
        project.cargo_proxy(environment, None);
    }

    fn context(project: &Project) -> Context {
        project.context()
    }

    fn workflow_registry() -> Registry {
        let mut registry = Registry::new();
        for registration in bake::__private::TASK_REGISTRATIONS {
            let task = (registration.factory)();
            if [
                "cargo:release:detect",
                "cargo:publish:pending",
                "cargo:release:publish",
            ]
            .contains(&task.name())
            {
                registry.register(task).unwrap();
            }
        }
        registry
    }

    #[test]
    fn registered_publish_tasks_dispatch_to_their_handlers() {
        let project = Project::new();

        let error = workflow_registry()
            .run_arguments(
                project.root(),
                &[
                    "cargo:release:detect".to_owned(),
                    "--base".to_owned(),
                    "000000".to_owned(),
                    "--sha".to_owned(),
                    String::new(),
                ],
            )
            .unwrap_err();
        assert!(error.to_string().contains("release commit"));

        let error = workflow_registry()
            .run_arguments(
                project.root(),
                &[
                    "cargo:publish:pending".to_owned(),
                    "--version".to_owned(),
                    "invalid".to_owned(),
                ],
            )
            .unwrap_err();
        assert!(error.to_string().contains("stable MAJOR.MINOR.PATCH"));

        let error = workflow_registry()
            .run_arguments(
                project.root(),
                &[
                    "cargo:release:publish".to_owned(),
                    "--version".to_owned(),
                    "0.1.0".to_owned(),
                    "--sha".to_owned(),
                    String::new(),
                ],
            )
            .unwrap_err();
        assert!(error.to_string().contains("release commit"));
    }

    fn set_crates_io_api(environment: &mut Environment, api: &str) {
        environment.set("BAKE_TEST_CRATES_IO_API", api);
    }

    #[test]
    fn propagates_release_output_failures() {
        for fail_on in ["release", "version"] {
            assert!(
                append_release_outputs(true, "1.2.3", |name, _| {
                    if name == fail_on {
                        Err(Error::new(format!("failed to append {name}")))
                    } else {
                        Ok(())
                    }
                })
                .unwrap_err()
                .to_string()
                .contains(fail_on)
            );
        }

        struct RejectWrite;

        impl Write for RejectWrite {
            fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("output is unavailable"))
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        assert!(
            write_github_output(&mut RejectWrite, "release", "true")
                .unwrap_err()
                .to_string()
                .contains("output is unavailable")
        );
        let mut writer = RejectWrite;
        assert!(writer.flush().is_ok());

        let mut environment = Environment::new();
        let project = release_project("1.2.3");
        prepare_workspace(&project, &mut environment);
        let output_directory = project.root().join("github-output-directory");
        fs::create_dir(&output_directory).unwrap();
        environment.set("GITHUB_OUTPUT", output_directory.as_os_str());
        let (api, server) = crate::crates_io::tests::mock_server(vec![(404, "crate not found")]);
        set_crates_io_api(&mut environment, &api);

        assert!(pending_packages(&mut context(&project), "1.2.3".to_owned()).is_err());
        server.join().unwrap();
    }

    #[test]
    fn detects_a_new_release_and_appends_github_outputs() {
        let mut environment = Environment::new();
        let project = release_project("1.2.2");
        prepare_workspace(&project, &mut environment);
        let base = initialize_git(&project, false);

        project.write(
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"1.2.3\"\nedition = \"2024\"\n",
        );
        project.write("releases.md", "# Releases\n\n## v1.2.3\n\nRelease notes.\n");
        let sha = commit(&project);
        let output = set_release_output(&mut environment, &project);

        let result = detect_release(&mut context(&project), base, sha).unwrap();

        assert_eq!(result, json!({"release": true, "version": "1.2.3"}));
        assert_eq!(
            fs::read_to_string(output).unwrap(),
            "release=true\nversion=1.2.3\n"
        );
    }

    #[test]
    fn detects_an_initial_release_when_the_base_has_no_cargo_manifest() {
        let mut environment = Environment::new();
        let project = Project::new();
        project.write("README.md", "Initial project overview.\n");
        prepare_workspace(&project, &mut environment);
        let base = initialize_git(&project, false);

        project.single_package("fixture", "1.2.3");
        project.write("releases.md", "# Releases\n\n## v1.2.3\n\nRelease notes.\n");
        let sha = commit(&project);
        let output = set_release_output(&mut environment, &project);

        let result = detect_release(&mut context(&project), base, sha).unwrap();

        assert_eq!(result, json!({"release": true, "version": "1.2.3"}));
        assert_eq!(
            fs::read_to_string(output).unwrap(),
            "release=true\nversion=1.2.3\n"
        );
    }

    #[test]
    fn reports_detect_release_metadata_previous_version_and_git_errors() {
        let mut environment = Environment::new();
        let metadata_failure = release_project("1.2.3");
        environment.set(
            "CARGO",
            metadata_failure.root().join("missing-cargo").as_os_str(),
        );
        assert!(
            detect_release(
                &mut context(&metadata_failure),
                String::new(),
                "deadbeef".to_owned(),
            )
            .is_err()
        );

        let invalid_previous = release_project("not-a-version");
        let base = initialize_git(&invalid_previous, false);
        invalid_previous.write(
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"1.2.3\"\nedition = \"2024\"\n",
        );
        invalid_previous.write("releases.md", "# Releases\n\n## v1.2.3\n\nNotes.\n");
        invalid_previous.cargo_proxy(&mut environment, None);
        assert!(
            detect_release(&mut context(&invalid_previous), base, "deadbeef".to_owned(),)
                .unwrap_err()
                .to_string()
                .contains("stable MAJOR.MINOR.PATCH")
        );

        let missing_git = release_project("1.2.3");
        missing_git.cargo_proxy(&mut environment, None);
        environment.set("PATH", missing_git.root().as_os_str());
        assert!(
            detect_release(
                &mut context(&missing_git),
                "base-revision".to_owned(),
                "deadbeef".to_owned(),
            )
            .is_err()
        );
        assert!(
            detect_release(
                &mut context(&missing_git),
                "000000".to_owned(),
                "deadbeef".to_owned(),
            )
            .is_err()
        );
    }

    #[test]
    fn skips_an_unchanged_release_and_allows_missing_github_output() {
        let mut environment = Environment::new();
        environment.remove("GITHUB_OUTPUT");
        let project = release_project("1.2.3");
        prepare_workspace(&project, &mut environment);
        let base = initialize_git(&project, false);

        let result = detect_release(&mut context(&project), base.clone(), base).unwrap();

        assert_eq!(result, json!({"release": false, "version": ""}));
    }

    #[test]
    fn rejects_invalid_revisions_lower_versions_missing_notes_and_conflicting_tags() {
        let mut environment = Environment::new();
        let project = release_project("1.2.3");
        prepare_workspace(&project, &mut environment);
        let base = initialize_git(&project, false);
        let mut context = context(&project);

        assert!(detect_release(&mut context, base.clone(), String::new()).is_err());
        assert!(detect_release(&mut context, base.clone(), "bad\nsha".to_owned()).is_err());

        project.write(
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"1.2.2\"\nedition = \"2024\"\n",
        );
        assert!(
            detect_release(&mut context, base.clone(), "new-sha".to_owned())
                .unwrap_err()
                .to_string()
                .contains("lower than previous version")
        );

        project.write(
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"1.2.4\"\nedition = \"2024\"\n",
        );
        fs::remove_file(project.root().join("releases.md")).unwrap();
        assert!(
            detect_release(&mut context, String::new(), "new-sha".to_owned())
                .unwrap_err()
                .to_string()
                .contains("could not read releases.md")
        );
        project.write("releases.md", "# Releases\n\n## v1.2.3\n\nOld notes.\n");
        assert!(
            detect_release(&mut context, String::new(), "new-sha".to_owned())
                .unwrap_err()
                .to_string()
                .contains("must contain a '## v1.2.4' heading")
        );

        project.write("releases.md", "# Releases\n\n## v1.2.4\n\nNew notes.\n");
        git_ok(&project, &["tag", "v1.2.4", &base]);
        assert!(
            detect_release(&mut context, String::new(), "new-sha".to_owned())
                .unwrap_err()
                .to_string()
                .contains("already exists at a different commit")
        );
    }

    #[test]
    fn reads_previous_workspace_versions_and_handles_missing_package_manifests() {
        let mut environment = Environment::new();
        let project = Project::new();
        project.write(
            "Cargo.toml",
            "[workspace]\nmembers = []\nresolver = \"3\"\n\n[workspace.package]\nversion = \"1.2.2\"\nedition = \"2024\"\n",
        );
        prepare_workspace(&project, &mut environment);
        let base = initialize_git(&project, false);
        project.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/fixture\"]\nresolver = \"3\"\n\n[workspace.package]\nversion = \"1.2.3\"\nedition = \"2024\"\n",
        );
        project.write(
            "crates/fixture/Cargo.toml",
            "[package]\nname = \"fixture\"\nversion.workspace = true\nedition.workspace = true\n",
        );
        project.write("crates/fixture/src/lib.rs", "// fixture\n");

        assert_eq!(previous_versions(&context(&project), &base).unwrap(), None);
        assert_eq!(
            previous_versions(&context(&project), "000000").unwrap(),
            None
        );
    }

    #[test]
    fn reports_previous_workspace_metadata_git_and_manifest_errors() {
        let mut environment = Environment::new();
        let failed_metadata = release_project("1.2.3");
        let base = initialize_git(&failed_metadata, false);
        let cargo = failed_metadata.executable(
            "cargo-failed",
            "#!/bin/sh\necho metadata denied >&2; exit 1\n",
        );
        environment.set("CARGO", cargo.as_os_str());
        assert!(
            previous_versions(&context(&failed_metadata), &base)
                .unwrap_err()
                .to_string()
                .contains("metadata denied")
        );

        for (previous_manifest, expected_error) in [
            ("not valid toml = [\n", Some("could not parse")),
            (
                "[package]\nname = \"fixture\"\nversion.workspace = false\n",
                None,
            ),
        ] {
            let project = Project::new();
            project.write(
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/fixture\"]\nresolver = \"3\"\n",
            );
            project.write("crates/fixture/Cargo.toml", previous_manifest);
            let base = initialize_git(&project, false);
            project.write(
                "crates/fixture/Cargo.toml",
                "[package]\nname = \"fixture\"\nversion = \"1.2.3\"\nedition = \"2024\"\n",
            );
            project.write("crates/fixture/src/lib.rs", "// fixture\n");
            prepare_workspace(&project, &mut environment);
            let result = previous_versions(&context(&project), &base);
            match expected_error {
                Some(expected) => {
                    let error = result.unwrap_err();
                    assert!(error.to_string().contains(expected), "{error}");
                }
                None => assert_eq!(result.unwrap(), None),
            }
        }

        let project = Project::new();
        project.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/fixture\"]\nresolver = \"3\"\n",
        );
        project.write(
            "crates/fixture/Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"1.2.3\"\nedition = \"2024\"\n",
        );
        project.write("crates/fixture/src/lib.rs", "// fixture\n");
        prepare_workspace(&project, &mut environment);
        let base = initialize_git(&project, false);
        let fake_git = project.executable(
            "git",
            &format!(
                "#!/bin/sh\n/bin/cat {}\n/bin/rm \"$0\"\n",
                shell_quote(&project.root().join("Cargo.toml"))
            ),
        );
        let fake_bin = fake_git.parent().unwrap();
        environment.set("PATH", fake_bin.as_os_str());
        assert!(previous_versions(&context(&project), &base).is_err());
    }

    #[test]
    fn reads_explicit_and_inherited_versions_from_the_base_revision() {
        let mut environment = Environment::new();
        let project = Project::new();
        project.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/fixture\"]\nresolver = \"3\"\n\n[workspace.package]\nversion = \"1.2.2\"\nedition = \"2024\"\n",
        );
        project.write(
            "crates/fixture/Cargo.toml",
            "[package]\nname = \"fixture\"\nversion.workspace = true\nedition.workspace = true\n",
        );
        project.write("crates/fixture/src/lib.rs", "// fixture\n");
        prepare_workspace(&project, &mut environment);
        let base = initialize_git(&project, false);
        project.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/fixture\"]\nresolver = \"3\"\n\n[workspace.package]\nversion = \"1.2.3\"\nedition = \"2024\"\n",
        );

        assert_eq!(
            previous_versions(&context(&project), &base).unwrap(),
            Some("1.2.2".to_owned())
        );
    }

    #[test]
    fn reports_invalid_previous_manifests_and_mismatched_package_versions() {
        let mut environment = Environment::new();

        let malformed = release_project("1.2.3");
        prepare_workspace(&malformed, &mut environment);
        malformed.write("Cargo.toml", "not valid toml");
        let base = initialize_git(&malformed, false);
        malformed.write(
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"1.2.3\"\nedition = \"2024\"\n",
        );
        assert!(
            previous_versions(&context(&malformed), &base)
                .unwrap_err()
                .to_string()
                .contains("could not parse previous workspace Cargo.toml")
        );

        let project = Project::new();
        project.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"first\", \"second\"]\nresolver = \"3\"\n\n[workspace.package]\nversion = \"1.2.2\"\nedition = \"2024\"\n",
        );
        project.write(
            "first/Cargo.toml",
            "[package]\nname = \"first\"\nversion = \"1.2.2\"\nedition = \"2024\"\n",
        );
        project.write("first/src/lib.rs", "// first\n");
        project.write(
            "second/Cargo.toml",
            "[package]\nname = \"second\"\nversion = \"1.2.3\"\nedition = \"2024\"\n",
        );
        project.write("second/src/lib.rs", "// second\n");
        prepare_workspace(&project, &mut environment);
        let base = initialize_git(&project, false);
        project.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"first\", \"second\"]\nresolver = \"3\"\n\n[workspace.package]\nversion = \"1.2.4\"\nedition = \"2024\"\n",
        );
        project.write(
            "first/Cargo.toml",
            "[package]\nname = \"first\"\nversion.workspace = true\nedition.workspace = true\n",
        );
        project.write(
            "second/Cargo.toml",
            "[package]\nname = \"second\"\nversion.workspace = true\nedition.workspace = true\n",
        );
        assert!(
            previous_versions(&context(&project), &base)
                .unwrap_err()
                .to_string()
                .contains("did not share one version")
        );
    }

    #[test]
    fn parses_manifest_versions_and_reports_missing_or_invalid_base_files() {
        let explicit = parse_manifest(
            "[package]\nname = \"fixture\"\nversion = \"1.2.3\"\n",
            "fixture manifest",
        )
        .unwrap();
        assert_eq!(package_version(&explicit, None), Some("1.2.3".to_owned()));

        let inherited = parse_manifest(
            "[package]\nname = \"fixture\"\nversion.workspace = true\n",
            "fixture manifest",
        )
        .unwrap();
        assert_eq!(
            package_version(&inherited, Some("1.2.4")),
            Some("1.2.4".to_owned())
        );
        assert_eq!(package_version(&inherited, None), None);
        let inherited_false = parse_manifest(
            "[package]\nname = \"fixture\"\nversion.workspace = false\n",
            "fixture manifest",
        )
        .unwrap();
        assert_eq!(package_version(&inherited_false, Some("1.2.4")), None);
        let invalid_version = parse_manifest(
            "[package]\nname = \"fixture\"\nversion = 3\n",
            "fixture manifest",
        )
        .unwrap();
        assert_eq!(package_version(&invalid_version, None), None);
        let missing_version = parse_manifest("[package]\nname = \"fixture\"\n", "fixture").unwrap();
        assert_eq!(package_version(&missing_version, Some("1.2.4")), None);
        assert!(parse_manifest("not toml", "fixture manifest").is_err());
        assert_eq!(package_version(&DocumentMut::new(), None), None);

        let mut environment = Environment::new();
        let project = Project::new();
        project.write("README.md", "fixture\n");
        prepare_workspace(&project, &mut environment);
        let base = initialize_git(&project, false);
        project.single_package("fixture", "1.2.3");
        assert_eq!(previous_versions(&context(&project), &base).unwrap(), None);

        let missing_project = Project::new();
        fs::remove_dir_all(missing_project.root()).unwrap();
        assert!(
            previous_versions(&context(&missing_project), "HEAD")
                .unwrap_err()
                .to_string()
                .contains("could not resolve project root")
        );

        let invalid_utf8 = release_project("1.2.3");
        prepare_workspace(&invalid_utf8, &mut environment);
        fs::write(invalid_utf8.root().join("Cargo.toml"), [0xff]).unwrap();
        let base = initialize_git(&invalid_utf8, false);
        invalid_utf8.write(
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"1.2.3\"\nedition = \"2024\"\n",
        );
        assert!(
            previous_versions(&context(&invalid_utf8), &base)
                .unwrap_err()
                .to_string()
                .contains("is not UTF-8")
        );
    }

    #[test]
    fn rejects_a_package_manifest_outside_the_project_root() {
        let mut environment = Environment::new();
        let project = release_project("1.2.3");
        prepare_workspace(&project, &mut environment);
        let base = initialize_git(&project, false);
        let metadata = project.write(
            "metadata.json",
            r#"{"packages":[{"name":"external","version":"1.2.3","manifest_path":"/outside/Cargo.toml"}]}"#,
        );
        let cargo = project.executable(
            "cargo-metadata",
            &format!("#!/bin/sh\ncat {}\n", shell_quote(&metadata)),
        );
        environment.set("CARGO", cargo.as_os_str());

        assert!(
            previous_versions(&context(&project), &base)
                .unwrap_err()
                .to_string()
                .contains("is outside the project root")
        );
    }

    #[test]
    fn returns_unpublished_packages_and_writes_the_pending_output() {
        let mut environment = Environment::new();
        let project = release_project("1.2.3");
        prepare_workspace(&project, &mut environment);
        let output = set_release_output(&mut environment, &project);
        let (api, server) = crate::crates_io::tests::mock_server(vec![(404, "crate not found")]);
        set_crates_io_api(&mut environment, &api);

        let result = pending_packages(&mut context(&project), "1.2.3".to_owned()).unwrap();

        assert_eq!(
            result,
            json!({"has_packages": true, "packages": ["fixture"]})
        );
        assert_eq!(fs::read_to_string(output).unwrap(), "has_packages=true\n");
        assert_eq!(server.join().unwrap().len(), 1);
    }

    #[test]
    fn validates_publication_state_before_checking_crates_io() {
        let mut environment = Environment::new();
        let empty = Project::new();
        empty.write(
            "Cargo.toml",
            "[workspace]\nmembers = []\nresolver = \"3\"\n",
        );
        prepare_workspace(&empty, &mut environment);
        assert!(
            publication_state(&context(&empty), "1.2.3")
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
        prepare_workspace(&mixed, &mut environment);
        assert!(
            publication_state(&context(&mixed), "1.2.3")
                .unwrap_err()
                .to_string()
                .contains("second has version 1.2.4, expected 1.2.3")
        );
        assert!(publication_state(&context(&mixed), "invalid").is_err());

        let failed_metadata = Project::new();
        let failed_cargo = failed_metadata.executable(
            "cargo-failed",
            "#!/bin/sh\necho metadata denied >&2; exit 1\n",
        );
        environment.set("CARGO", failed_cargo.as_os_str());
        assert!(publication_state(&context(&failed_metadata), "1.2.3").is_err());

        let invalid_version = Project::new();
        let metadata = serde_json::json!({
            "packages": [{
                "name": "fixture",
                "version": "not-a-version",
                "manifest_path": invalid_version.root().join("Cargo.toml")
            }]
        });
        let cargo = invalid_version.executable(
            "cargo-metadata",
            &format!("#!/bin/sh\nprintf '%s' '{}'\n", metadata),
        );
        environment.set("CARGO", cargo.as_os_str());
        assert!(publication_state(&context(&invalid_version), "1.2.3").is_err());
    }

    #[test]
    fn reports_crates_io_errors_while_checking_publication_state() {
        let mut environment = Environment::new();
        let project = release_project("1.2.3");
        prepare_workspace(&project, &mut environment);
        let (api, server) = crate::crates_io::tests::mock_server(vec![(403, "registry denied")]);
        set_crates_io_api(&mut environment, &api);

        assert!(
            publication_state(&context(&project), "1.2.3")
                .unwrap_err()
                .to_string()
                .contains("registry denied")
        );
        server.join().unwrap();
    }

    #[test]
    fn publishes_remaining_packages_tags_the_commit_and_creates_a_release() {
        let mut environment = Environment::new();
        let project = Project::new();
        project.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"alpha\", \"beta\"]\nresolver = \"3\"\n\n[workspace.package]\nversion = \"1.2.3\"\nedition = \"2024\"\n",
        );
        for name in ["alpha", "beta"] {
            project.write(
                format!("{name}/Cargo.toml"),
                &format!("[package]\nname = \"{name}\"\nversion.workspace = true\nedition.workspace = true\n"),
            );
            project.write(format!("{name}/src/lib.rs"), "// fixture\n");
        }
        project.write("releases.md", "# Releases\n\n## v1.2.3\n\nRelease notes.\n");
        prepare_workspace(&project, &mut environment);
        let _base = initialize_git(&project, true);
        let sha = git_ok(&project, &["rev-parse", "HEAD"]);
        install_gh(&project, &mut environment);
        let (api, server) = crate::crates_io::tests::mock_server(vec![
            (200, r#"{"versions":[{"num":"1.2.3"}]}"#),
            (404, "crate not found"),
        ]);
        set_crates_io_api(&mut environment, &api);

        let result = publish_release(&mut context(&project), "1.2.3".to_owned(), sha).unwrap();

        assert_eq!(result["tag"], "v1.2.3");
        assert_eq!(result["published"], json!(["beta"]));
        assert_eq!(result["already_published"], json!(["alpha"]));
        assert_eq!(
            result["github_release"],
            "https://github.com/socketry/fixture/releases/tag/v1.2.3"
        );
        assert!(
            project
                .cargo_arguments()
                .contains("publish --workspace --locked --exclude alpha")
        );
        assert!(
            fs::read_to_string(project.root().join("gh-release-notes"))
                .unwrap()
                .contains("Release notes.")
        );
        assert_eq!(server.join().unwrap().len(), 2);
        assert!(
            git(&project, &["rev-parse", "refs/tags/v1.2.3"])
                .status
                .success()
        );
    }

    #[test]
    fn skips_cargo_publish_when_all_packages_are_already_published() {
        let mut environment = Environment::new();
        let project = release_project("1.2.3");
        prepare_workspace(&project, &mut environment);
        let sha = initialize_git(&project, true);
        install_gh(&project, &mut environment);
        let output = set_release_output(&mut environment, &project);
        let (api, server) = crate::crates_io::tests::mock_server(vec![
            (200, r#"{"versions":[{"num":"1.2.3"}]}"#),
            (200, r#"{"versions":[{"num":"1.2.3"}]}"#),
        ]);
        set_crates_io_api(&mut environment, &api);

        let pending = pending_packages(&mut context(&project), "1.2.3".to_owned()).unwrap();
        assert_eq!(pending, json!({"has_packages": false, "packages": []}));
        assert_eq!(fs::read_to_string(output).unwrap(), "has_packages=false\n");

        let result =
            publish_release(&mut context(&project), "1.2.3".to_owned(), sha.clone()).unwrap();

        assert_eq!(result["published"], json!([]));
        assert_eq!(result["already_published"], json!(["fixture"]));
        assert!(!project.cargo_arguments().contains("publish --workspace"));
        create_release_tag(&context(&project), "v1.2.3", &sha).unwrap();
        assert!(
            create_release_tag(&context(&project), "v1.2.3", "different-sha")
                .unwrap_err()
                .to_string()
                .contains("already exists at a different commit")
        );
        assert_eq!(server.join().unwrap().len(), 2);
    }

    #[test]
    fn reports_release_publish_and_git_failures() {
        let mut environment = Environment::new();
        let project = release_project("1.2.3");
        prepare_workspace(&project, &mut environment);
        let sha = initialize_git(&project, false);
        assert!(
            publish_release(&mut context(&project), "invalid".to_owned(), sha.clone()).is_err()
        );
        let (api, server) = crate::crates_io::tests::mock_server(vec![(404, "crate not found")]);
        set_crates_io_api(&mut environment, &api);
        environment.set("BAKE_TEST_CARGO_FAILURE", "publish");
        assert!(publish_release(&mut context(&project), "1.2.3".to_owned(), sha.clone()).is_err());
        server.join().unwrap();

        assert!(
            publish_release(&mut context(&project), "1.2.3".to_owned(), "".to_owned()).is_err()
        );
        assert!(run_git(&context(&project), &["not-a-git-command".to_owned()]).is_err());
    }

    #[test]
    fn reports_a_failed_tag_push() {
        let project = release_project("1.2.3");
        let sha = initialize_git(&project, false);

        assert!(
            create_release_tag(&context(&project), "v1.2.3", &sha)
                .unwrap_err()
                .to_string()
                .contains("git push origin refs/tags/v1.2.3 failed")
        );
    }

    #[test]
    fn propagates_tag_creation_errors_from_release_publishing() {
        let mut environment = Environment::new();
        let project = release_project("1.2.3");
        prepare_workspace(&project, &mut environment);
        let sha = initialize_git(&project, false);
        let (api, server) =
            crate::crates_io::tests::mock_server(vec![(200, r#"{"versions":[{"num":"1.2.3"}]}"#)]);
        set_crates_io_api(&mut environment, &api);

        assert!(
            publish_release(&mut context(&project), "1.2.3".to_owned(), sha)
                .unwrap_err()
                .to_string()
                .contains("git push origin refs/tags/v1.2.3 failed")
        );
        server.join().unwrap();
    }

    #[test]
    fn propagates_errors_while_configuring_or_creating_the_tag() {
        let mut environment = Environment::new();
        let project = Project::new();
        project.executable(
            "git",
            "#!/bin/sh\nif [ \"$1\" = rev-list ]; then exit 1; fi\nif [ \"$*\" = \"$BAKE_TEST_GIT_FAILURE\" ]; then echo rejected >&2; exit 7; fi\nexit 0\n",
        );
        environment.prepend_path(&project.root().join("bin"));
        let context = context(&project);

        for command in [
            "config user.name github-actions[bot]",
            "config user.email 41898282+github-actions[bot]@users.noreply.github.com",
            "tag -a v1.2.3 deadbeef -m Release v1.2.3",
        ] {
            environment.set("BAKE_TEST_GIT_FAILURE", command);
            assert!(create_release_tag(&context, "v1.2.3", "deadbeef").is_err());
        }
    }

    #[test]
    fn reports_git_process_launch_failures_during_tagging() {
        let mut environment = Environment::new();
        let missing_git = release_project("1.2.3");
        let sha = initialize_git(&missing_git, false);
        environment.set("PATH", missing_git.root().as_os_str());
        assert!(create_release_tag(&context(&missing_git), "v1.2.3", &sha).is_err());

        let original_path = environment.original("PATH").unwrap();
        environment.set("PATH", original_path);
        let project = release_project("1.2.3");
        let sha = initialize_git(&project, false);
        let fake_git = project.executable(
            "git",
            &format!(
                "#!/bin/sh\nif [ \"$1\" = rev-list ]; then printf '%s' '{}'; /bin/rm \"$0\"; exit 0; fi\nexit 0\n",
                sha
            ),
        );
        environment.set("PATH", fake_git.parent().unwrap().as_os_str());
        assert!(create_release_tag(&context(&project), "v1.2.3", &sha).is_err());
    }

    #[test]
    fn propagates_github_release_creation_failures() {
        let mut environment = Environment::new();
        let project = release_project("1.2.3");
        prepare_workspace(&project, &mut environment);
        let sha = initialize_git(&project, true);
        project.executable(
            "gh",
            "#!/bin/sh\nif [ \"$1 $2\" = 'release view' ]; then echo 'release not found' >&2; exit 1; fi\necho 'release creation denied' >&2; exit 1\n",
        );
        environment.prepend_path(&project.root().join("bin"));
        let (api, server) =
            crate::crates_io::tests::mock_server(vec![(200, r#"{"versions":[{"num":"1.2.3"}]}"#)]);
        set_crates_io_api(&mut environment, &api);

        assert!(
            publish_release(&mut context(&project), "1.2.3".to_owned(), sha)
                .unwrap_err()
                .to_string()
                .contains("GitHub release creation failed")
        );
        server.join().unwrap();
    }
}
