// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

/// GitHub release creation using notes from `releases.md`.
pub mod github {
    use bake::{Context, Error, Result};
    use serde::Deserialize;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{Command, Output, Stdio};

    use bake_releases::extract_notes;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "camelCase")]
    struct Release {
        tag_name: String,
        name: Option<String>,
        body: Option<String>,
        is_draft: bool,
        url: String,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum ReleaseAction {
        Create,
        Update,
        Keep,
    }

    fn action_for(
        existing: Option<&Release>,
        tag: &str,
        notes: &str,
        draft: bool,
    ) -> ReleaseAction {
        let Some(existing) = existing else {
            return ReleaseAction::Create;
        };

        if existing.tag_name == tag
            && existing.name.as_deref() == Some(tag)
            && existing.body.as_deref().unwrap_or_default() == notes
            && existing.is_draft == draft
        {
            ReleaseAction::Keep
        } else {
            ReleaseAction::Update
        }
    }

    fn existing_release(context: &Context, tag: &str) -> Result<Option<Release>> {
        let output = context
            .command("gh")
            .args([
                "release",
                "view",
                tag,
                "--json",
                "tagName,name,body,isDraft,url",
            ])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.trim() == "release not found" {
                return Ok(None);
            }

            return Err(Error::new(format!(
                "could not inspect GitHub release {tag:?}: {}",
                stderr.trim()
            )));
        }

        serde_json::from_slice(&output.stdout)
            .map(Some)
            .map_err(|error| Error::new(format!("could not parse GitHub release: {error}")))
    }

    fn run_with_notes(mut command: Command, notes: &str) -> Result<Output> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn()?;
        child
            .stdin
            .take()
            .ok_or_else(|| Error::new("failed to open GitHub CLI input"))?
            .write_all(notes.as_bytes())?;

        Ok(child.wait_with_output()?)
    }

    fn run_release_command(
        context: &Context,
        action: &ReleaseAction,
        tag: &str,
        notes: &str,
        draft: bool,
    ) -> Result<String> {
        let mut command = context.command("gh");
        match action {
            ReleaseAction::Create => {
                command.args(["release", "create"]).arg(tag).args([
                    "--title",
                    tag,
                    "--notes-file",
                    "-",
                    "--verify-tag",
                ]);
                if draft {
                    command.arg("--draft");
                }
            }
            ReleaseAction::Update => {
                command
                    .args(["release", "edit"])
                    .arg(tag)
                    .args(["--title", tag, "--notes-file", "-", "--verify-tag"])
                    .arg(if draft {
                        "--draft=true"
                    } else {
                        "--draft=false"
                    });
            }
            ReleaseAction::Keep => {
                return Err(Error::new(
                    "internal error: an unchanged GitHub release should not be updated",
                ));
            }
        }

        let output = run_with_notes(command, notes)?;
        if !output.status.success() {
            return Err(Error::new(format!(
                "GitHub release {} failed: {}",
                if matches!(action, ReleaseAction::Create) {
                    "creation"
                } else {
                    "update"
                },
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn existing_url(existing: Option<&Release>) -> Result<String> {
        existing
            .map(|release| release.url.clone())
            .ok_or_else(|| Error::new("internal error: the GitHub release does not exist"))
    }

    /// Create or update a GitHub Release from the matching release heading.
    /// The remote tag must already exist; use `--draft true` to create or update a draft.
    #[bake::task]
    pub fn release(
        context: &mut Context,
        tag: String,
        #[bake(default = "releases.md")] path: PathBuf,
        #[bake(default = false)] draft: bool,
    ) -> Result<String> {
        if tag.is_empty() || tag.starts_with('-') || tag.chars().any(char::is_control) {
            return Err(Error::new("tag must be a nonempty GitHub release tag"));
        }

        let path = context.root().join(path);
        let document = fs::read_to_string(&path)
            .map_err(|error| Error::new(format!("{}: {error}", path.display())))?;
        let notes = extract_notes(&document, &tag)?;
        let existing = existing_release(context, &tag)?;
        let action = action_for(existing.as_ref(), &tag, notes, draft);

        match action {
            ReleaseAction::Keep => existing_url(existing.as_ref()),
            ReleaseAction::Create => {
                let result = run_release_command(context, &action, &tag, notes, draft)?;
                if result.is_empty() {
                    Err(Error::new("GitHub release creation returned no URL"))
                } else {
                    Ok(result)
                }
            }
            ReleaseAction::Update => {
                run_release_command(context, &action, &tag, notes, draft)?;
                existing_url(existing.as_ref())
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{Release, ReleaseAction, action_for};

        fn release() -> Release {
            Release {
                tag_name: "v1.2.3".to_owned(),
                name: Some("v1.2.3".to_owned()),
                body: Some("\nRelease notes.\n\n".to_owned()),
                is_draft: false,
                url: "https://github.com/socketry/example/releases/tag/v1.2.3".to_owned(),
            }
        }

        #[test]
        fn creates_when_the_release_does_not_exist() {
            assert_eq!(
                action_for(None, "v1.2.3", "\nRelease notes.\n\n", false),
                ReleaseAction::Create
            );
        }

        #[test]
        fn leaves_an_identical_release_unchanged() {
            assert_eq!(
                action_for(Some(&release()), "v1.2.3", "\nRelease notes.\n\n", false),
                ReleaseAction::Keep
            );
        }

        #[test]
        fn updates_release_notes_when_they_change() {
            assert_eq!(
                action_for(Some(&release()), "v1.2.3", "\nUpdated notes.\n\n", false),
                ReleaseAction::Update
            );
        }

        #[test]
        fn updates_the_release_title_when_it_differs_from_the_tag() {
            let mut existing = release();
            existing.name = Some("v1.2.3: Initial release".to_owned());

            assert_eq!(
                action_for(Some(&existing), "v1.2.3", "\nRelease notes.\n\n", false),
                ReleaseAction::Update
            );
        }

        #[test]
        fn updates_draft_state_when_it_changes() {
            let mut existing = release();
            existing.is_draft = true;

            assert_eq!(
                action_for(Some(&existing), "v1.2.3", "\nRelease notes.\n\n", false),
                ReleaseAction::Update
            );
        }
    }
}
