// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn executable_lists_standard_project_tasks() {
    let output = Command::new(env!("CARGO_BIN_EXE_bake-cargo-project"))
        .arg("--list")
        .output()
        .expect("run Bake task executable");

    assert!(output.status.success());
    let output = String::from_utf8(output.stdout).expect("task listing is UTF-8");
    assert!(output.contains("cargo:after_version_bump"));
    assert!(output.contains("test:coverage"));
    assert!(output.contains("test:external"));
    for name in ["cargo:releases:github:release", "releases:github:release"] {
        assert!(
            output
                .lines()
                .any(|line| line.split_whitespace().next() == Some(name))
        );
    }
}

#[cfg(unix)]
#[test]
fn executable_runs_release_tasks_in_a_temporary_project() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    struct Project(PathBuf);

    impl Project {
        fn new() -> Self {
            let identifier = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "bake-cargo-cli-{}-{identifier}",
                std::process::id()
            ));
            fs::create_dir_all(root.join("src")).unwrap();
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[package.metadata.bake.release]\nreviewers = [\"alice\"]\n",
            )
            .unwrap();
            fs::write(root.join("src/lib.rs"), "// fixture\n").unwrap();
            fs::write(
                root.join("license.md"),
                "# MIT License\n\nCopyright, 2026, by Test User.\n",
            )
            .unwrap();
            fs::write(
                root.join("readme.md"),
                "# `fixture`\n\n## Motivation\n\nA temporary fixture.\n\n## Usage\n\nUsed to exercise Bake tasks.\n\n## Contributing\n\nOpen an Issue or Pull Request.\n\n## License\n\nMIT\n",
            )
            .unwrap();
            fs::write(
                root.join("releases.md"),
                "# Releases\n\n## Unreleased\n\nUpcoming changes.\n\n## v1.2.5\n\nUpdated release notes.\n\n## v1.2.4\n\nImprovement.\n\n## v1.2.3\n\nInitial release.\n\n## v0.1.0\n\nInitial fixture release.\n",
            )
            .unwrap();

            let status = Command::new("git")
                .current_dir(&root)
                .args(["init", "--quiet"])
                .status()
                .unwrap();
            assert!(status.success());
            let status = Command::new("git")
                .current_dir(&root)
                .args([
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/socketry/fixture.git",
                ])
                .status()
                .unwrap();
            assert!(status.success());
            let status = Command::new("git")
                .current_dir(&root)
                .args(["add", "."])
                .status()
                .unwrap();
            assert!(status.success());
            let status = Command::new("git")
                .current_dir(&root)
                .args([
                    "-c",
                    "user.name=Test User",
                    "-c",
                    "user.email=test@example.com",
                    "commit",
                    "--quiet",
                    "-m",
                    "Initial fixture",
                ])
                .status()
                .unwrap();
            assert!(status.success());

            Self(root)
        }

        fn run(&self, arguments: &[&str]) -> std::process::Output {
            Command::new(env!("CARGO_BIN_EXE_bake-cargo-project"))
                .current_dir(&self.0)
                .args(arguments)
                .env("BAKE_PROJECT_ROOT", &self.0)
                .env_remove("CARGO_REGISTRY_TOKEN")
                .output()
                .unwrap()
        }

        fn run_with_fake_gh(&self, arguments: &[&str]) -> std::process::Output {
            let bin = self.0.join("bin");
            fs::create_dir_all(&bin).unwrap();
            let executable = bin.join("gh");
            fs::write(
                &executable,
                "#!/bin/sh\nif [ \"$1 $2\" = 'release view' ]; then\n  case \"$3\" in\n    v1.2.4) printf '%s' '{\"tagName\":\"v1.2.4\",\"name\":\"v1.2.4\",\"body\":\"Improvement.\",\"isDraft\":false,\"url\":\"https://github.com/socketry/fixture/releases/tag/v1.2.4\"}'; exit 0 ;;\n    v1.2.5) printf '%s' '{\"tagName\":\"v1.2.5\",\"name\":\"Old title\",\"body\":\"Old notes.\",\"isDraft\":false,\"url\":\"https://github.com/socketry/fixture/releases/tag/v1.2.5\"}'; exit 0 ;;\n  esac\n  echo 'release not found' >&2; exit 1\nfi\nif [ \"$1 $2\" = 'release create' ] || [ \"$1 $2\" = 'release edit' ]; then printf '%s\\n' \"$@\" >gh-arguments.txt; cat >gh-notes.md; echo \"https://github.com/socketry/fixture/releases/tag/$3\"; exit 0; fi\ncase \"$3 $4\" in\n  'GET users/alice') printf '%s' '{\"id\":42}' ;;\n  'GET repos/socketry/fixture/environments?per_page=100') printf '%s' '{\"environments\":[]}' ;;\n  'GET repos/socketry/fixture/rulesets?per_page=100') printf '%s' '[]' ;;\n  'POST repos/socketry/fixture/rulesets') cat >/dev/null; printf '%s' '{\"id\":7}' ;;\n  'PUT repos/socketry/fixture/environments/crates-io') cat >/dev/null; printf '%s' '{\"name\":\"crates-io\"}' ;;\n  *) echo unexpected-request >&2; exit 1 ;;\nesac\n",
            )
            .unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();

            let mut paths = vec![bin];
            paths.extend(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ));
            Command::new(env!("CARGO_BIN_EXE_bake-cargo-project"))
                .current_dir(&self.0)
                .args(arguments)
                .env("BAKE_PROJECT_ROOT", &self.0)
                .env("PATH", std::env::join_paths(paths).unwrap())
                .env_remove("CARGO_REGISTRY_TOKEN")
                .output()
                .unwrap()
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn assert_success(output: std::process::Output, task: &str) {
        assert!(
            output.status.success(),
            "{task} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let project = Project::new();
    let root: &Path = &project.0;

    assert_success(project.run(&["cargo:packages"]), "cargo:packages");
    assert_success(project.run(&["cargo:release"]), "cargo:release");
    assert_success(
        project.run(&["cargo:setup:workflow"]),
        "cargo:setup:workflow",
    );
    assert_success(
        project.run_with_fake_gh(&["cargo:setup:github:plan"]),
        "cargo:setup:github:plan",
    );
    assert_success(
        project.run(&["cargo:trusted-publishing:plan", "fixture"]),
        "cargo:trusted-publishing:plan",
    );

    assert!(
        !project
            .run(&["cargo:trusted-publishing:configure", "fixture"])
            .status
            .success()
    );
    assert!(
        !project
            .run(&[
                "cargo:trusted-publishing:require",
                "fixture",
                "--required",
                "true"
            ])
            .status
            .success()
    );

    assert_success(
        project.run_with_fake_gh(&["cargo:setup:github:apply"]),
        "cargo:setup:github:apply",
    );
    for task in ["cargo:releases:github:release", "releases:github:release"] {
        for tag in ["v1.2.3", "v1.2.4", "v1.2.5"] {
            let output = project.run_with_fake_gh(&[task, tag]);
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).trim(),
                format!("https://github.com/socketry/fixture/releases/tag/{tag}")
            );
            assert_success(output, task);
        }

        fs::write(
            root.join("draft-notes.md"),
            "# Releases\n\n## v2.0.0\n\nDraft notes.\n",
        )
        .unwrap();
        assert_success(
            project.run_with_fake_gh(&[
                task,
                "v2.0.0",
                "--path",
                "draft-notes.md",
                "--draft",
                "true",
            ]),
            task,
        );
        assert!(
            fs::read_to_string(root.join("gh-arguments.txt"))
                .unwrap()
                .lines()
                .any(|argument| argument == "--draft")
        );
        assert_eq!(
            fs::read_to_string(root.join("gh-notes.md")).unwrap().trim(),
            "Draft notes."
        );

        let output = project.run(&[task, ""]);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("tag must be a nonempty GitHub release tag")
        );
    }
    assert!(
        !project
            .run(&["cargo:version:bump", "--version", "invalid"])
            .status
            .success()
    );
    assert_success(project.run(&["cargo:version:patch"]), "cargo:version:patch");

    assert!(root.join(".github/workflows/publish.yml").is_file());
    assert!(
        fs::read_to_string(root.join("Cargo.toml"))
            .unwrap()
            .contains("version = \"0.1.1\"")
    );

    for (arguments, expected_version) in [
        (&["cargo:version:minor"][..], "0.2.0"),
        (&["cargo:version:major"][..], "1.0.0"),
        (&["cargo:version:bump", "--version", "1.0.1"][..], "1.0.1"),
    ] {
        let project = Project::new();
        let task = arguments[0];
        assert_success(project.run(arguments), task);
        assert!(
            fs::read_to_string(project.0.join("Cargo.toml"))
                .unwrap()
                .contains(&format!("version = \"{expected_version}\""))
        );
    }
}
