// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use bake::{Context, Error, Result, Value};
use serde_json::json;
use std::fs;

pub(crate) fn prepare(context: &Context, version: &str) -> Result<Value> {
    let packages = crate::cargo_support::workspace_packages(context)?;
    if packages.is_empty() {
        return Err(Error::new("the workspace has no publishable packages"));
    }

    let release_notes_path = context.root().join("releases.md");
    let release_notes = fs::read_to_string(&release_notes_path).map_err(|error| {
        Error::new(format!(
            "could not read {}: {error}",
            release_notes_path.display()
        ))
    })?;
    if !contains_release_heading(&release_notes, version) {
        let heading = format!("## v{version}");
        return Err(Error::new(format!(
            "{} must contain a {heading:?} release heading",
            release_notes_path.display()
        )));
    }

    let mut package_arguments = vec!["package".to_owned(), "--locked".to_owned()];
    for package in &packages {
        package_arguments.extend(["--package".to_owned(), package.name.clone()]);
    }
    crate::cargo_support::run_cargo_arguments(context, &package_arguments)?;

    Ok(json!({
        "version": version,
        "packages": packages.iter().map(|package| package.name.as_str()).collect::<Vec<_>>(),
        "status": "ready for a release pull request",
        "next": "Commit the reviewed version, release notes, and any other release changes, then open a pull request. Merging it to the configured branch triggers publishing after the crates-io environment approval.",
    }))
}

fn contains_release_heading(release_notes: &str, version: &str) -> bool {
    let heading = format!("## v{version}");
    release_notes.lines().any(|line| {
        line.strip_prefix(&heading).is_some_and(|remainder| {
            remainder.is_empty() || remainder.chars().next().is_some_and(char::is_whitespace)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::contains_release_heading;

    #[test]
    fn requires_an_exact_release_heading() {
        assert!(contains_release_heading(
            "## Unreleased\n\n## v1.2.3\n",
            "1.2.3"
        ));
        assert!(contains_release_heading(
            "## v1.2.3 - Improvements\n",
            "1.2.3"
        ));
        assert!(!contains_release_heading("## v1.2.30\n", "1.2.3"));
        assert!(!contains_release_heading("## v1.2.3-rc.1\n", "1.2.3"));
    }
}
