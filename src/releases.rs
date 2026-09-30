// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

/// GitHub release creation using notes from `releases.md`.
pub mod github {
    use bake::{Context, Error, Result};
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::Stdio;

    use bake_releases::extract_notes;

    /// Create a GitHub Release using notes from the matching release heading.
    /// The remote tag must already exist; use `--draft true` to create a draft.
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

        let mut command = context.command("gh");
        command
            .args(["release", "create"])
            .arg(&tag)
            .arg("--title")
            .arg(&tag)
            .args(["--notes-file", "-", "--verify-tag"]);
        if draft {
            command.arg("--draft");
        }

        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or_else(|| Error::new("failed to open GitHub CLI input"))?
            .write_all(notes.as_bytes())?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(Error::new(format!(
                "GitHub release creation failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}
