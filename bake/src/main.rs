// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use bake::{Registry, Result};

fn main() -> Result<()> {
    run(Registry::discover())
}

fn run(registry: Result<Registry>) -> Result<()> {
    registry?.run()
}

#[path = "bake_generated_tasks/mod.rs"]
mod bake_generated_tasks;

#[cfg(test)]
mod tests {
    use super::run;
    use bake::Error;

    #[test]
    fn propagates_registry_discovery_errors() {
        assert!(
            run(Err(Error::new("registry discovery failed")))
                .unwrap_err()
                .to_string()
                .contains("registry discovery failed")
        );
    }
}
