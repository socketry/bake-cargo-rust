// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use bake::{Registry, Result};

fn main() -> Result<()> {
    Registry::discover()?.run()
}

#[path = "bake_generated_tasks/mod.rs"]
mod bake_generated_tasks;
