// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

/// GitHub release creation using notes from `releases.md`.
pub mod github {
    use bake::{Context, Error, Result};
    use serde::Deserialize;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{ChildStdin, Command, Output, Stdio};

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
        write_release_notes(child.stdin.take(), notes)?;

        Ok(child.wait_with_output()?)
    }

    fn write_release_notes(stdin: Option<ChildStdin>, notes: &str) -> Result<()> {
        stdin
            .ok_or_else(|| Error::new("failed to open GitHub CLI input"))?
            .write_all(notes.as_bytes())?;
        Ok(())
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
        use super::{
            Release, ReleaseAction, action_for, existing_release, existing_url,
            release as release_task, run_release_command, run_with_notes, write_release_notes,
        };
        use crate::test_support::{Environment, Project, shell_quote};
        use bake::{Context, Registry};

        fn with_gh(project: &Project, environment: &mut Environment, script: &str) {
            project.executable("gh", script);
            environment.prepend_path(&project.root().join("bin"));
        }

        fn release_notes(project: &Project) -> String {
            project.write("releases.md", "# Releases\n\n## v1.2.3\n\nRelease notes.\n");
            bake_releases::extract_notes(
                &std::fs::read_to_string(project.root().join("releases.md")).unwrap(),
                "v1.2.3",
            )
            .unwrap()
            .to_owned()
        }

        fn context(project: &Project) -> Context {
            Registry::new().context(project.root())
        }

        fn sample_release() -> Release {
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
                action_for(
                    Some(&sample_release()),
                    "v1.2.3",
                    "\nRelease notes.\n\n",
                    false
                ),
                ReleaseAction::Keep
            );
        }

        #[test]
        fn updates_release_notes_when_they_change() {
            assert_eq!(
                action_for(
                    Some(&sample_release()),
                    "v1.2.3",
                    "\nUpdated notes.\n\n",
                    false
                ),
                ReleaseAction::Update
            );
        }

        #[test]
        fn updates_the_release_title_when_it_differs_from_the_tag() {
            let mut existing = sample_release();
            existing.name = Some("v1.2.3: Initial release".to_owned());

            assert_eq!(
                action_for(Some(&existing), "v1.2.3", "\nRelease notes.\n\n", false),
                ReleaseAction::Update
            );
        }

        #[test]
        fn updates_draft_state_when_it_changes() {
            let mut existing = sample_release();
            existing.is_draft = true;

            assert_eq!(
                action_for(Some(&existing), "v1.2.3", "\nRelease notes.\n\n", false),
                ReleaseAction::Update
            );
        }

        #[test]
        fn updates_when_the_existing_tag_name_differs() {
            let mut existing = sample_release();
            existing.tag_name = "v1.2.2".to_owned();

            assert_eq!(
                action_for(Some(&existing), "v1.2.3", "\nRelease notes.\n\n", false),
                ReleaseAction::Update
            );
        }

        #[test]
        fn creates_the_release_with_notes_and_draft_status() {
            let mut environment = Environment::new();
            let project = Project::new();
            let notes = release_notes(&project);
            with_gh(
                &project,
                &mut environment,
                "#!/bin/sh\ncase \"$2\" in\n  view) echo 'release not found' >&2; exit 1 ;;\n  create) cat > received-notes.md; printf '%s' 'https://github.com/socketry/example/releases/tag/v1.2.3' ;;\n  *) echo unexpected-request >&2; exit 1 ;;\nesac\n",
            );

            let result = release_task(
                &mut context(&project),
                "v1.2.3".to_owned(),
                "releases.md".into(),
                true,
            )
            .unwrap();
            assert_eq!(
                result,
                "https://github.com/socketry/example/releases/tag/v1.2.3"
            );
            assert_eq!(
                std::fs::read_to_string(project.root().join("received-notes.md")).unwrap(),
                notes
            );
        }

        #[test]
        fn keeps_an_identical_release_without_mutating_it() {
            let mut environment = Environment::new();
            let project = Project::new();
            let notes = release_notes(&project);
            let mut existing = sample_release();
            existing.body = Some(notes);
            let output = serde_json::json!({
                "tagName": existing.tag_name,
                "name": existing.name,
                "body": existing.body,
                "isDraft": existing.is_draft,
                "url": existing.url,
            });
            with_gh(
                &project,
                &mut environment,
                &format!(
                    "#!/bin/sh\nprintf '%s' {}\n",
                    shell_quote(std::path::Path::new(&output.to_string()))
                ),
            );

            let url = release_task(
                &mut context(&project),
                "v1.2.3".to_owned(),
                "releases.md".into(),
                false,
            )
            .unwrap();
            assert_eq!(url, existing.url);
        }

        #[test]
        fn updates_changed_notes_and_returns_the_existing_url() {
            let mut environment = Environment::new();
            let project = Project::new();
            let notes = release_notes(&project);
            let output = serde_json::json!({
                "tagName": "v1.2.3",
                "name": "old title",
                "body": "Old notes.",
                "isDraft": true,
                "url": "https://github.com/socketry/example/releases/tag/v1.2.3",
            });
            with_gh(
                &project,
                &mut environment,
                &format!(
                    "#!/bin/sh\ncase \"$2\" in\n  view) printf '%s' {} ;;\n  edit) cat > received-notes.md; printf '%s' 'ignored output' ;;\n  *) exit 1 ;;\nesac\n",
                    shell_quote(std::path::Path::new(&output.to_string()))
                ),
            );

            let url = release_task(
                &mut context(&project),
                "v1.2.3".to_owned(),
                "releases.md".into(),
                false,
            )
            .unwrap();
            assert_eq!(url, output["url"]);
            assert_eq!(
                std::fs::read_to_string(project.root().join("received-notes.md")).unwrap(),
                notes
            );
        }

        #[test]
        fn rejects_invalid_tags_missing_notes_and_unreadable_release_files() {
            let _environment = Environment::new();
            let project = Project::new();
            let mut context = context(&project);
            for tag in ["", "-bad", "bad\ntag"] {
                assert!(
                    release_task(&mut context, tag.to_owned(), "releases.md".into(), false)
                        .is_err()
                );
            }
            assert!(
                release_task(
                    &mut context,
                    "v1.2.3".to_owned(),
                    "releases.md".into(),
                    false
                )
                .unwrap_err()
                .to_string()
                .contains("releases.md")
            );
            release_notes(&project);
            assert!(
                release_task(
                    &mut context,
                    "v9.9.9".to_owned(),
                    "releases.md".into(),
                    false
                )
                .is_err()
            );
        }

        #[test]
        fn reports_inspection_creation_and_update_command_failures() {
            let mut environment = Environment::new();
            let project = Project::new();
            release_notes(&project);
            with_gh(
                &project,
                &mut environment,
                "#!/bin/sh\necho authentication failed >&2\nexit 2\n",
            );
            assert!(
                release_task(
                    &mut context(&project),
                    "v1.2.3".to_owned(),
                    "releases.md".into(),
                    false
                )
                .unwrap_err()
                .to_string()
                .contains("authentication failed")
            );

            let create_failure = Project::new();
            release_notes(&create_failure);
            with_gh(
                &create_failure,
                &mut environment,
                "#!/bin/sh\ncase \"$2\" in\n  view) echo 'release not found' >&2; exit 1 ;;\n  create) cat >/dev/null; echo create-denied >&2; exit 2 ;;\nesac\n",
            );
            assert!(
                release_task(
                    &mut context(&create_failure),
                    "v1.2.3".to_owned(),
                    "releases.md".into(),
                    false
                )
                .unwrap_err()
                .to_string()
                .contains("creation failed")
            );

            let update_failure = Project::new();
            release_notes(&update_failure);
            let existing = serde_json::json!({
                "tagName": "v1.2.3",
                "name": "old",
                "body": "old",
                "isDraft": false,
                "url": "https://example.test/release",
            });
            with_gh(
                &update_failure,
                &mut environment,
                &format!(
                    "#!/bin/sh\ncase \"$2\" in\n  view) printf '%s' {} ;;\n  edit) cat >/dev/null; echo update-denied >&2; exit 2 ;;\nesac\n",
                    shell_quote(std::path::Path::new(&existing.to_string()))
                ),
            );
            assert!(
                release_task(
                    &mut context(&update_failure),
                    "v1.2.3".to_owned(),
                    "releases.md".into(),
                    false
                )
                .unwrap_err()
                .to_string()
                .contains("update failed")
            );
        }

        #[test]
        fn rejects_malformed_release_json_and_missing_creation_urls() {
            let mut environment = Environment::new();
            let invalid = Project::new();
            release_notes(&invalid);
            with_gh(&invalid, &mut environment, "#!/bin/sh\nprintf '{'\n");
            assert!(
                release_task(
                    &mut context(&invalid),
                    "v1.2.3".to_owned(),
                    "releases.md".into(),
                    false
                )
                .unwrap_err()
                .to_string()
                .contains("could not parse GitHub release")
            );

            let no_url = Project::new();
            release_notes(&no_url);
            with_gh(
                &no_url,
                &mut environment,
                "#!/bin/sh\ncase \"$2\" in\n  view) echo 'release not found' >&2; exit 1 ;;\n  create) cat >/dev/null ;;\nesac\n",
            );
            assert!(
                release_task(
                    &mut context(&no_url),
                    "v1.2.3".to_owned(),
                    "releases.md".into(),
                    false
                )
                .unwrap_err()
                .to_string()
                .contains("returned no URL")
            );
        }

        #[test]
        fn reports_internal_inconsistent_release_actions() {
            let _environment = Environment::new();
            let project = Project::new();
            assert!(existing_url(None).is_err());
            assert!(
                run_release_command(
                    &context(&project),
                    &ReleaseAction::Keep,
                    "v1.2.3",
                    "notes",
                    false,
                )
                .unwrap_err()
                .to_string()
                .contains("unchanged GitHub release")
            );
        }

        #[test]
        fn reports_missing_github_cli_input() {
            assert!(
                write_release_notes(None, "Release notes.")
                    .unwrap_err()
                    .to_string()
                    .contains("failed to open GitHub CLI input")
            );
        }

        #[test]
        fn existing_release_recognizes_the_exact_not_found_message() {
            let mut environment = Environment::new();
            let project = Project::new();
            with_gh(
                &project,
                &mut environment,
                "#!/bin/sh\necho ' release not found ' >&2\nexit 1\n",
            );
            assert!(
                existing_release(&context(&project), "v1.2.3")
                    .unwrap()
                    .is_none()
            );
        }

        #[test]
        fn reports_failure_to_start_the_github_cli() {
            let mut environment = Environment::new();
            let project = Project::new();
            project.write("releases.md", "## v1.2.3\n\nNotes.\n");
            environment.set("PATH", project.root().as_os_str());
            assert!(
                release_task(
                    &mut context(&project),
                    "v1.2.3".to_owned(),
                    "releases.md".into(),
                    false
                )
                .is_err()
            );
        }

        #[test]
        fn reports_failure_to_spawn_a_release_command() {
            let project = Project::new();
            let command = std::process::Command::new(project.root().join("missing-gh"));

            assert!(run_with_notes(command, "Release notes.").is_err());
        }

        #[test]
        fn launches_release_commands_with_a_configured_draft_state() {
            let mut environment = Environment::new();
            let project = Project::new();
            with_gh(
                &project,
                &mut environment,
                "#!/bin/sh\ncat >/dev/null\nprintf '%s' 'https://example.test/release'\n",
            );
            assert_eq!(
                run_release_command(
                    &context(&project),
                    &ReleaseAction::Update,
                    "v1.2.3",
                    "notes",
                    true,
                )
                .unwrap(),
                "https://example.test/release"
            );
        }
    }
}
