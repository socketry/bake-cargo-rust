// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use bake::{Context, Error, Result, Value};
use serde_json::json;
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use toml_edit::{DocumentMut, Item, Table, Value as TomlValue, value};

use crate::cargo_support::{WorkspacePackage, workspace_packages};

#[derive(Clone, Copy)]
pub(crate) enum Component {
    Major,
    Minor,
    Patch,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    fn parse(value: &str) -> Result<Self> {
        let components: Vec<_> = value.split('.').collect();
        if components.len() != 3 {
            return Err(Error::new(format!(
                "version {value:?} must use the stable MAJOR.MINOR.PATCH form"
            )));
        }

        let mut numbers = [0; 3];
        for (index, component) in components.into_iter().enumerate() {
            if component.is_empty()
                || component.len() > 1 && component.starts_with('0')
                || !component.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(Error::new(format!(
                    "version {value:?} must use the stable MAJOR.MINOR.PATCH form"
                )));
            }
            numbers[index] = component
                .parse()
                .map_err(|_| Error::new(format!("version component in {value:?} is too large")))?;
        }

        Ok(Self {
            major: numbers[0],
            minor: numbers[1],
            patch: numbers[2],
        })
    }

    fn increment(self, component: Component) -> Result<Self> {
        match component {
            Component::Major => Ok(Self {
                major: self.major.checked_add(1).ok_or_else(version_overflow)?,
                minor: 0,
                patch: 0,
            }),
            Component::Minor => Ok(Self {
                major: self.major,
                minor: self.minor.checked_add(1).ok_or_else(version_overflow)?,
                patch: 0,
            }),
            Component::Patch => Ok(Self {
                major: self.major,
                minor: self.minor,
                patch: self.patch.checked_add(1).ok_or_else(version_overflow)?,
            }),
        }
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn version_overflow() -> Error {
    Error::new("version component is too large to increment")
}

pub(crate) fn workspace_version(context: &Context) -> Result<String> {
    let packages = workspace_packages(context)?;
    let version = shared_version(&packages)?;
    Ok(version.to_string())
}

pub(crate) fn increment(context: &Context, component: Component) -> Result<Value> {
    let packages = workspace_packages(context)?;
    let current = shared_version(&packages)?;
    let next = current.increment(component)?;
    set_workspace_version(context, &packages, current, next)
}

pub(crate) fn set(context: &Context, target: &str) -> Result<Value> {
    let target = Version::parse(target)?;
    let packages = workspace_packages(context)?;
    let current = shared_version(&packages)?;
    if target <= current {
        return Err(Error::new(format!(
            "new version {target} must be greater than the current workspace version {current}"
        )));
    }
    set_workspace_version(context, &packages, current, target)
}

fn shared_version(packages: &[WorkspacePackage]) -> Result<Version> {
    let first = packages
        .first()
        .ok_or_else(|| Error::new("the workspace has no publishable packages"))?;
    let version = Version::parse(&first.version)?;
    for package in &packages[1..] {
        let package_version = Version::parse(&package.version)?;
        if package_version != version {
            return Err(Error::new(format!(
                "publishable workspace packages must share one version; {} is {}, while {} is {}",
                first.name, first.version, package.name, package.version
            )));
        }
    }
    Ok(version)
}

fn set_workspace_version(
    context: &Context,
    packages: &[WorkspacePackage],
    current: Version,
    target: Version,
) -> Result<Value> {
    if target <= current {
        return Err(Error::new(format!(
            "new version {target} must be greater than the current workspace version {current}"
        )));
    }

    let package_names: HashSet<_> = packages
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    let (workspace_manifest, manifests) = workspace_manifests(context)?;
    let publishable_manifests: HashSet<_> = packages
        .iter()
        .map(|package| PathBuf::from(&package.manifest_path))
        .collect();

    let mut updated = Vec::new();
    for path in manifests {
        let source = fs::read_to_string(&path)
            .map_err(|error| Error::new(format!("{}: {error}", path.display())))?;
        let mut document: DocumentMut = source
            .parse()
            .map_err(|error| Error::new(format!("{}: {error}", path.display())))?;
        if publishable_manifests.contains(&path) {
            update_package_version(&mut document, current, target)?;
        }
        if path == workspace_manifest {
            update_workspace_version(&mut document, current, target)?;
        }
        update_local_dependency_versions(&mut document, target, &package_names);
        updated.push((path, document.to_string()));
    }

    for (path, contents) in &updated {
        replace_file(path, contents)?;
    }

    crate::cargo_support::run_cargo(context, ["update", "--workspace"]).map_err(|error| {
        Error::new(format!(
            "updated Cargo versions, but could not update Cargo.lock: {error}"
        ))
    })?;

    Ok(json!({
        "previous_version": current.to_string(),
        "version": target.to_string(),
        "packages": packages.iter().map(|package| package.name.as_str()).collect::<Vec<_>>(),
        "manifests_updated": updated.iter().map(|(path, _)| path.display().to_string()).collect::<Vec<_>>(),
    }))
}

fn workspace_manifests(context: &Context) -> Result<(PathBuf, BTreeSet<PathBuf>)> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = context
        .command(cargo)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()?;
    if !output.status.success() {
        return Err(Error::new(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| Error::new(format!("could not parse Cargo metadata: {error}")))?;
    let root = metadata
        .get("workspace_root")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| Error::new("Cargo metadata did not contain a workspace root"))?;
    let workspace_manifest = Path::new(root).join("Cargo.toml");
    let packages = metadata
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| Error::new("Cargo metadata did not contain a packages array"))?;
    let mut manifests = BTreeSet::from([workspace_manifest.clone()]);
    for package in packages {
        let manifest = package
            .get("manifest_path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::new("Cargo metadata package has no manifest path"))?;
        manifests.insert(PathBuf::from(manifest));
    }
    Ok((workspace_manifest, manifests))
}

fn update_package_version(
    document: &mut DocumentMut,
    current: Version,
    target: Version,
) -> Result<()> {
    let Some(package) = document.get_mut("package").and_then(Item::as_table_mut) else {
        return Ok(());
    };
    let Some(version) = package.get("version") else {
        return Ok(());
    };
    if version.as_str() == Some(&current.to_string()) {
        document["package"]["version"] = value(target.to_string());
    } else if version
        .as_table_like()
        .and_then(|table| table.get("workspace"))
        .and_then(Item::as_bool)
        == Some(true)
    {
        // This package inherits the version from [workspace.package].
    } else {
        return Err(Error::new(format!(
            "package version is not the expected workspace version {current}"
        )));
    }
    Ok(())
}

fn update_workspace_version(
    document: &mut DocumentMut,
    current: Version,
    target: Version,
) -> Result<()> {
    let Some(version) = document
        .get("workspace")
        .and_then(Item::as_table)
        .and_then(|workspace| workspace.get("package"))
        .and_then(Item::as_table)
        .and_then(|package| package.get("version"))
    else {
        return Ok(());
    };
    if version.as_str() != Some(&current.to_string()) {
        return Err(Error::new(format!(
            "[workspace.package].version is not the expected workspace version {current}"
        )));
    }
    document["workspace"]["package"]["version"] = value(target.to_string());
    Ok(())
}

fn update_local_dependency_versions(
    document: &mut DocumentMut,
    target: Version,
    package_names: &HashSet<&str>,
) {
    update_dependency_groups(document.as_table_mut(), target, package_names);

    if let Some(workspace) = document.get_mut("workspace").and_then(Item::as_table_mut) {
        update_dependency_groups(workspace, target, package_names);
    }

    if let Some(targets) = document.get_mut("target").and_then(Item::as_table_mut) {
        for (_, configuration) in targets.iter_mut() {
            if let Some(configuration) = configuration.as_table_mut() {
                update_dependency_groups(configuration, target, package_names);
            }
        }
    }
}

fn update_dependency_groups(table: &mut Table, target: Version, package_names: &HashSet<&str>) {
    for group_name in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(group) = table.get_mut(group_name).and_then(Item::as_table_mut) {
            for (name, specification) in group.iter_mut() {
                let name = name.get();
                if let Some(specification) = specification.as_table_mut() {
                    update_table_dependency(specification, name, target, package_names);
                } else if let Some(specification) = specification
                    .as_value_mut()
                    .and_then(TomlValue::as_inline_table_mut)
                {
                    update_inline_dependency(specification, name, target, package_names);
                }
            }
        }
    }
}

fn update_table_dependency(
    dependency: &mut Table,
    name: &str,
    target: Version,
    package_names: &HashSet<&str>,
) {
    let package_name = dependency
        .get("package")
        .and_then(Item::as_str)
        .unwrap_or(name);
    if package_names.contains(package_name) && dependency.get("version").is_some() {
        dependency["version"] = value(target.to_string());
    }
}

fn update_inline_dependency(
    dependency: &mut toml_edit::InlineTable,
    name: &str,
    target: Version,
    package_names: &HashSet<&str>,
) {
    let package_name = dependency
        .get("package")
        .and_then(TomlValue::as_str)
        .unwrap_or(name);
    if package_names.contains(package_name) && dependency.get("version").is_some() {
        dependency.insert("version", TomlValue::from(target.to_string()));
    }
}

fn replace_file(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::new(format!("{} has no parent directory", path.display())))?;
    let permissions = fs::metadata(path)?.permissions();
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(contents.as_bytes())?;
    temporary.as_file().set_permissions(permissions)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| Error::from(error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Environment, Project, shell_quote};

    fn fake_metadata(project: &Project, environment: &mut Environment, json: &str) {
        let path = project.write("metadata.json", json);
        let cargo = project.executable(
            "cargo-metadata",
            &format!("#!/bin/sh\ncat {}\n", shell_quote(&path)),
        );
        environment.set("CARGO", cargo.as_os_str());
    }

    fn workspace(project: &Project) {
        project.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/alpha\", \"crates/beta\", \"bake\"]\nresolver = \"3\"\n\n[workspace.package]\nversion = \"1.2.3\"\nedition = \"2024\"\n\n[workspace.dependencies]\nbeta = { path = \"crates/beta\", version = \"1.2.3\" }\n",
        );
        project.write(
            "crates/alpha/Cargo.toml",
            "[package]\nname = \"alpha\"\nversion.workspace = true\nedition.workspace = true\n\n[dependencies]\nbeta.workspace = true\n",
        );
        project.write("crates/alpha/src/lib.rs", "// fixture\n");
        project.write(
            "crates/beta/Cargo.toml",
            "[package]\nname = \"beta\"\nversion.workspace = true\nedition.workspace = true\n",
        );
        project.write("crates/beta/src/lib.rs", "// fixture\n");
        project.write(
            "bake/Cargo.toml",
            "[package]\nname = \"build-tools\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[dependencies]\nalpha = { path = \"../crates/alpha\", version = \"1.2.3\" }\n",
        );
        project.write("bake/src/lib.rs", "// fixture\n");
    }

    fn package(name: &str, version: &str) -> WorkspacePackage {
        WorkspacePackage {
            name: name.to_owned(),
            version: version.to_owned(),
            manifest_path: format!("{name}/Cargo.toml"),
        }
    }

    #[test]
    fn parses_only_stable_three_component_versions() {
        assert_eq!(
            Version::parse("1.23.4").unwrap(),
            Version {
                major: 1,
                minor: 23,
                patch: 4
            }
        );
        for invalid in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.3-rc.1",
            "18446744073709551616.0.0",
        ] {
            assert!(Version::parse(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn increments_version_components_and_reports_overflow() {
        let version = Version::parse("1.2.3").unwrap();
        assert_eq!(
            version.increment(Component::Patch).unwrap().to_string(),
            "1.2.4"
        );
        assert_eq!(
            version.increment(Component::Minor).unwrap().to_string(),
            "1.3.0"
        );
        assert_eq!(
            version.increment(Component::Major).unwrap().to_string(),
            "2.0.0"
        );
        assert!(
            Version::parse("18446744073709551615.0.0")
                .unwrap()
                .increment(Component::Major)
                .is_err()
        );
        assert!(
            Version::parse("0.18446744073709551615.0")
                .unwrap()
                .increment(Component::Minor)
                .is_err()
        );
        assert!(
            Version::parse("0.0.18446744073709551615")
                .unwrap()
                .increment(Component::Patch)
                .is_err()
        );
    }

    #[test]
    fn requires_all_publishable_packages_to_share_a_stable_version() {
        let packages = vec![package("first", "1.2.3"), package("second", "1.2.3")];
        assert_eq!(shared_version(&packages).unwrap().to_string(), "1.2.3");
        assert!(shared_version(&[package("first", "1.2.3"), package("second", "1.2.4")]).is_err());
        assert!(shared_version(&[]).is_err());
    }

    #[test]
    fn updates_explicit_and_workspace_inherited_package_versions() {
        let mut document: DocumentMut = "[package]\nversion = \"1.2.3\"\n".parse().unwrap();
        update_package_version(
            &mut document,
            Version::parse("1.2.3").unwrap(),
            Version::parse("1.2.4").unwrap(),
        )
        .unwrap();
        assert_eq!(document["package"]["version"].as_str(), Some("1.2.4"));

        let mut inherited: DocumentMut = "[package]\nversion.workspace = true\n".parse().unwrap();
        update_package_version(
            &mut inherited,
            Version::parse("1.2.3").unwrap(),
            Version::parse("1.2.4").unwrap(),
        )
        .unwrap();
        assert_eq!(
            inherited["package"]["version"]["workspace"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn updates_workspace_and_matching_local_dependency_versions() {
        let source = "[package]\nname = \"first\"\nversion = \"0.1.0\"\n\n[dependencies]\nalias = { package = \"second\", version = \"0.1.0\", path = \"../second\" }\nunrelated = { package = \"other\", version = \"3.0.0\" }\n\n[dev-dependencies]\nsecond-string = \"0.1.0\"\n\n[workspace.package]\nversion = \"0.1.0\"\n\n[workspace.dependencies]\nsecond = { version = \"0.1.0\", path = \"second\" }\n\n[target]\ninvalid-configuration = \"not a table\"\n\n[target.'cfg(unix)'.dependencies]\nsecond-target = { package = \"second\", version = \"0.1.0\", path = \"second\" }\n";
        let mut document: DocumentMut = source.parse().unwrap();
        let current = Version::parse("0.1.0").unwrap();
        let target = Version::parse("0.2.0").unwrap();

        update_package_version(&mut document, current, target).unwrap();
        update_workspace_version(&mut document, current, target).unwrap();
        update_local_dependency_versions(&mut document, target, &HashSet::from(["second"]));

        assert_eq!(document["package"]["version"].as_str(), Some("0.2.0"));
        assert_eq!(
            document["workspace"]["package"]["version"].as_str(),
            Some("0.2.0")
        );
        assert_eq!(
            document["dependencies"]["alias"]["version"].as_str(),
            Some("0.2.0")
        );
        assert_eq!(
            document["dependencies"]["unrelated"]["version"].as_str(),
            Some("3.0.0")
        );
        assert_eq!(
            document["dev-dependencies"]["second-string"].as_str(),
            Some("0.1.0")
        );
        assert_eq!(
            document["workspace"]["dependencies"]["second"]["version"].as_str(),
            Some("0.2.0")
        );
        assert_eq!(
            document["target"]["cfg(unix)"]["dependencies"]["second-target"]["version"].as_str(),
            Some("0.2.0")
        );
    }

    #[test]
    fn increments_and_sets_shared_workspace_versions_in_cargo_manifests() {
        let mut environment = Environment::new();
        let project = Project::new();
        workspace(&project);
        project.cargo_proxy(&mut environment, None);
        let context = project.context();

        assert_eq!(workspace_version(&context).unwrap(), "1.2.3");
        assert_eq!(
            increment(&context, Component::Patch).unwrap()["version"],
            "1.2.4"
        );
        assert_eq!(
            increment(&context, Component::Minor).unwrap()["version"],
            "1.3.0"
        );
        assert_eq!(
            increment(&context, Component::Major).unwrap()["version"],
            "2.0.0"
        );
        assert_eq!(set(&context, "2.4.0").unwrap()["version"], "2.4.0");

        let workspace_manifest = fs::read_to_string(project.root().join("Cargo.toml")).unwrap();
        let dependency_manifest =
            fs::read_to_string(project.root().join("bake/Cargo.toml")).unwrap();
        assert!(workspace_manifest.contains("version = \"2.4.0\""));
        assert!(workspace_manifest.contains("version = \"2.4.0\" }"));
        assert!(dependency_manifest.contains("version = \"2.4.0\""));
        assert!(project.cargo_arguments().contains("update --workspace"));
    }

    #[test]
    fn rejects_non_increasing_versions_before_mutating_the_workspace() {
        let mut environment = Environment::new();
        let project = Project::new();
        workspace(&project);
        project.cargo_proxy(&mut environment, None);
        let context = project.context();

        for requested in ["1.2.3", "1.2.2", "1.2", "1.2.3-rc.1"] {
            assert!(set(&context, requested).is_err(), "{requested}");
        }
        assert!(increment(&context, Component::Patch).is_ok());
    }

    #[test]
    fn reports_a_failed_lockfile_update_after_changing_manifest_versions() {
        let mut environment = Environment::new();
        let project = Project::new();
        workspace(&project);
        project.cargo_proxy(&mut environment, Some("update"));

        let error = increment(&project.context(), Component::Patch).unwrap_err();
        assert!(error.to_string().contains("could not update Cargo.lock"));
        assert!(
            fs::read_to_string(project.root().join("Cargo.toml"))
                .unwrap()
                .contains("version = \"1.2.4\"")
        );
    }

    #[test]
    fn validates_manifest_update_inputs_and_dependency_forms() {
        let current = Version::parse("1.2.3").unwrap();
        let target = Version::parse("1.2.4").unwrap();

        let mut absent_package: DocumentMut = "[workspace]\nmembers = []\n".parse().unwrap();
        update_package_version(&mut absent_package, current, target).unwrap();
        let mut missing_version: DocumentMut = "[package]\nname = \"fixture\"\n".parse().unwrap();
        update_package_version(&mut missing_version, current, target).unwrap();
        let mut mismatched: DocumentMut = "[package]\nversion = \"1.2.2\"\n".parse().unwrap();
        assert!(update_package_version(&mut mismatched, current, target).is_err());

        let mut no_workspace_version: DocumentMut = "[workspace]\nmembers = []\n".parse().unwrap();
        update_workspace_version(&mut no_workspace_version, current, target).unwrap();
        let mut wrong_workspace_version: DocumentMut = "[workspace.package]\nversion = \"1.2.2\"\n"
            .parse()
            .unwrap();
        assert!(update_workspace_version(&mut wrong_workspace_version, current, target).is_err());

        let source = "[dependencies.alias]\npackage = \"second\"\nversion = \"1.2.3\"\npath = \"second\"\n\n[dev-dependencies]\nsecond = { version = \"1.2.3\", path = \"second\" }\n\n[build-dependencies]\nsecond = { version = \"1.2.3\", path = \"second\" }\n";
        let mut document: DocumentMut = source.parse().unwrap();
        update_local_dependency_versions(&mut document, target, &HashSet::from(["second"]));
        assert_eq!(
            document["dependencies"]["alias"]["version"].as_str(),
            Some("1.2.4")
        );
        assert_eq!(
            document["dev-dependencies"]["second"]["version"].as_str(),
            Some("1.2.4")
        );
        assert_eq!(
            document["build-dependencies"]["second"]["version"].as_str(),
            Some("1.2.4")
        );
    }

    #[test]
    fn reports_missing_manifest_files() {
        let project = Project::new();
        assert!(replace_file(&project.root().join("missing.toml"), "data").is_err());
        assert!(replace_file(Path::new(""), "data").is_err());

        let directory = project.root().join("manifest-directory");
        fs::create_dir(&directory).unwrap();
        assert!(replace_file(&directory, "data").is_err());
        assert!(directory.is_dir());
    }

    #[test]
    fn rejects_a_non_increasing_target_even_when_called_internally() {
        let project = Project::new();
        let current = Version::parse("1.2.3").unwrap();
        assert!(
            set_workspace_version(
                &project.context(),
                &[package("fixture", "1.2.3")],
                current,
                current,
            )
            .unwrap_err()
            .to_string()
            .contains("must be greater")
        );
    }

    #[test]
    fn reports_workspace_metadata_errors_and_missing_manifest_fields() {
        let mut environment = Environment::new();
        let project = Project::new();
        let failed = project.executable(
            "cargo-failed",
            "#!/bin/sh\necho metadata-denied >&2\nexit 4\n",
        );
        environment.set("CARGO", failed.as_os_str());
        assert!(
            workspace_manifests(&project.context())
                .unwrap_err()
                .to_string()
                .contains("metadata-denied")
        );

        for (metadata, expected) in [
            ("not json", "could not parse Cargo metadata"),
            (r#"{"packages":[]}"#, "workspace root"),
            (r#"{"workspace_root":"/tmp"}"#, "packages array"),
            (
                r#"{"workspace_root":"/tmp","packages":[{}]}"#,
                "no manifest path",
            ),
        ] {
            fake_metadata(&project, &mut environment, metadata);
            assert!(
                workspace_manifests(&project.context())
                    .unwrap_err()
                    .to_string()
                    .contains(expected)
            );
        }
    }

    #[test]
    fn discovers_workspace_manifests_using_cargo_from_path() {
        let mut environment = Environment::new();
        environment.remove("CARGO");
        let project = Project::new();
        workspace(&project);

        let (_, manifests) = workspace_manifests(&project.context()).unwrap();

        assert_eq!(manifests.len(), 4);
        assert!(manifests.contains(&fs::canonicalize(project.root().join("Cargo.toml")).unwrap()));
    }

    #[test]
    fn reports_manifest_read_parse_and_version_mismatch_errors() {
        let mut environment = Environment::new();
        let project = Project::new();
        let workspace_root = project.root().to_string_lossy().into_owned();
        let missing = serde_json::json!({"workspace_root": workspace_root, "packages": []});
        fake_metadata(&project, &mut environment, &missing.to_string());
        assert!(
            set_workspace_version(
                &project.context(),
                &[package("fixture", "1.2.3")],
                Version::parse("1.2.3").unwrap(),
                Version::parse("1.2.4").unwrap(),
            )
            .is_err()
        );

        project.write("Cargo.toml", "this is not TOML = [\n");
        assert!(
            set_workspace_version(
                &project.context(),
                &[package("fixture", "1.2.3")],
                Version::parse("1.2.3").unwrap(),
                Version::parse("1.2.4").unwrap(),
            )
            .is_err()
        );

        project.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/alpha\"]\nresolver = \"3\"\n",
        );
        project.write(
            "crates/alpha/Cargo.toml",
            "[package]\nname = \"alpha\"\nversion = \"1.2.2\"\nedition = \"2024\"\n",
        );
        let metadata = serde_json::json!({
            "workspace_root": workspace_root,
            "packages": [{"manifest_path": project.root().join("crates/alpha/Cargo.toml")}]
        });
        fake_metadata(&project, &mut environment, &metadata.to_string());
        assert!(
            set_workspace_version(
                &project.context(),
                &[WorkspacePackage {
                    name: "alpha".to_owned(),
                    version: "1.2.3".to_owned(),
                    manifest_path: project
                        .root()
                        .join("crates/alpha/Cargo.toml")
                        .display()
                        .to_string(),
                }],
                Version::parse("1.2.3").unwrap(),
                Version::parse("1.2.4").unwrap(),
            )
            .unwrap_err()
            .to_string()
            .contains("package version is not the expected")
        );
    }
}
