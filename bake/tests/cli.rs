// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::process::Command;

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
}
