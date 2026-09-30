// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use bake::{Error, Result, Value};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::github::Repository;

const CRATES_IO_API: &str = "https://crates.io/api/v1";

#[derive(Debug, Deserialize)]
struct ConfigurationsResponse {
    #[serde(default)]
    github_configs: Vec<TrustedPublisher>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TrustedPublisher {
    id: u64,
    #[serde(rename = "crate")]
    package: String,
    repository_owner: String,
    repository_name: String,
    workflow_filename: String,
    environment: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConfigurationResponse {
    github_config: TrustedPublisher,
}

pub(crate) fn trusted_publisher_plan(
    package: &str,
    repository: &Repository,
    workflow: &str,
    environment: &str,
) -> Result<Value> {
    validate_configuration(package, workflow, environment)?;
    let mut configuration = json!({
        "crate": package,
        "repository_owner": repository.owner,
        "repository_name": repository.name,
        "workflow_filename": workflow,
    });
    if !environment.is_empty() {
        configuration["environment"] = json!(environment);
    }
    Ok(json!({
        "endpoint": format!("{CRATES_IO_API}/trusted_publishing/github_configs"),
        "method": "POST",
        "github_config": configuration,
        "next": "cargo:trusted-publishing:configure",
    }))
}

pub(crate) fn validate_trusted_publisher_inputs(
    package: &str,
    workflow: &str,
    environment: &str,
) -> Result<()> {
    validate_configuration(package, workflow, environment)?;
    registry_token().map(|_| ())
}

pub(crate) fn configure_trusted_publisher(
    package: &str,
    repository: &Repository,
    workflow: &str,
    environment: &str,
) -> Result<Value> {
    validate_configuration(package, workflow, environment)?;
    let token = registry_token()?;

    let configurations = list_trusted_publishers(&token, package)?;
    if let Some(configuration) = configurations.iter().find(|configuration| {
        configuration
            .repository_owner
            .eq_ignore_ascii_case(&repository.owner)
            && configuration
                .repository_name
                .eq_ignore_ascii_case(&repository.name)
            && configuration.workflow_filename == workflow
            && configuration.environment.as_deref() == nonempty(environment)
    }) {
        return Ok(json!({"status": "already_configured", "github_config": configuration}));
    }

    let mut requested = json!({
        "crate": package,
        "repository_owner": repository.owner,
        "repository_name": repository.name,
        "workflow_filename": workflow,
    });
    if let Some(environment) = nonempty(environment) {
        requested["environment"] = json!(environment);
    }

    let endpoint = format!("{CRATES_IO_API}/trusted_publishing/github_configs");
    let response: ConfigurationResponse = ureq::post(&endpoint)
        .set("Authorization", &token)
        .set("User-Agent", "bake-cargo")
        .send_json(json!({"github_config": requested}))
        .map_err(|error| api_error("create trusted publisher configuration", error))?
        .into_json()
        .map_err(|error| Error::new(format!("could not decode crates.io response: {error}")))?;

    Ok(json!({"status": "created", "github_config": response.github_config}))
}

pub(crate) fn set_trusted_publishing_only(package: &str, required: bool) -> Result<Value> {
    validate_package_name(package)?;
    let token = registry_token()?;

    let endpoint = format!("{CRATES_IO_API}/crates/{package}");
    let response = ureq::patch(&endpoint)
        .set("Authorization", &token)
        .set("User-Agent", "bake-cargo")
        .send_json(json!({"crate": {"trustpub_only": required}}))
        .map_err(|error| api_error("update trusted-publishing requirement", error))?
        .into_json::<Value>()
        .map_err(|error| Error::new(format!("could not decode crates.io response: {error}")))?;

    Ok(json!({
        "status": "updated",
        "crate": package,
        "trustpub_only": required,
        "response": response,
    }))
}

fn list_trusted_publishers(token: &str, package: &str) -> Result<Vec<TrustedPublisher>> {
    validate_package_name(package)?;
    let endpoint = format!("{CRATES_IO_API}/trusted_publishing/github_configs?crate={package}");
    let response: ConfigurationsResponse = ureq::get(&endpoint)
        .set("Authorization", token)
        .set("User-Agent", "bake-cargo")
        .call()
        .map_err(|error| api_error("list trusted publisher configurations", error))?
        .into_json()
        .map_err(|error| Error::new(format!("could not decode crates.io response: {error}")))?;
    Ok(response.github_configs)
}

fn validate_configuration(package: &str, workflow: &str, environment: &str) -> Result<()> {
    validate_package_name(package)?;
    if workflow.is_empty()
        || !workflow.ends_with(".yml") && !workflow.ends_with(".yaml")
        || !workflow
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(Error::new(
            "workflow must be a filename ending in .yml or .yaml",
        ));
    }
    if environment.contains('/')
        || environment.contains('\\')
        || environment.chars().any(char::is_control)
    {
        return Err(Error::new(
            "environment must be a single GitHub environment name",
        ));
    }
    Ok(())
}

fn validate_package_name(package: &str) -> Result<()> {
    if package.is_empty()
        || !package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(Error::new(
            "package name must contain only letters, numbers, hyphens, or underscores",
        ));
    }
    Ok(())
}

fn nonempty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

fn registry_token() -> Result<String> {
    let token = std::env::var("CARGO_REGISTRY_TOKEN").map_err(|_| {
        Error::new(
            "set CARGO_REGISTRY_TOKEN to a crates.io token with Trusted Publishing permission",
        )
    })?;
    if token.trim().is_empty() {
        return Err(Error::new("CARGO_REGISTRY_TOKEN is empty"));
    }
    Ok(token)
}

fn api_error(action: &str, error: ureq::Error) -> Error {
    match error {
        ureq::Error::Status(status, response) => {
            let details = response.into_string().unwrap_or_default();
            Error::new(format!(
                "could not {action}: crates.io returned HTTP {status}: {details}"
            ))
        }
        error => Error::new(format!("could not {action}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_trusted_publisher_inputs() {
        assert!(validate_configuration("socketry-crate", "publish.yml", "crates-io").is_ok());
        assert!(
            validate_configuration("socketry-crate", "nested/publish.yml", "crates-io").is_err()
        );
        assert!(validate_configuration("socketry-crate", "publish.txt", "crates-io").is_err());
        assert!(validate_configuration("bad name", "publish.yml", "crates-io").is_err());
        assert!(
            validate_configuration("socketry-crate", "publish.yml", "bad/environment").is_err()
        );
    }

    #[test]
    fn trusted_publisher_plan_omits_an_unspecified_environment() {
        let repository = Repository {
            owner: "socketry".to_owned(),
            name: "example".to_owned(),
        };
        let plan =
            trusted_publisher_plan("socketry-crate", &repository, "publish.yml", "").unwrap();

        assert_eq!(plan["method"], "POST");
        assert_eq!(plan["github_config"]["crate"], "socketry-crate");
        assert_eq!(plan["github_config"]["repository_owner"], "socketry");
        assert_eq!(plan["github_config"]["repository_name"], "example");
        assert!(plan["github_config"].get("environment").is_none());
    }
}
