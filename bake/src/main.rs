// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use bake::{Context, Registry, Result};
use bake_cargo as _;
use bake_license as _;
use bake_releases as _;

/// Update the project's license and release notes after changing its version.
#[bake::task(name = "cargo:after_version_bump")]
fn after_version_bump(context: &mut Context, version: String) -> Result<()> {
    context.call("license:update", &[])?;
    let release_heading = format!("v{version}");
    context.call("releases:update", &[&release_heading])?;
    Ok(())
}

fn main() -> Result<()> {
    Registry::discover()?.run()
}
