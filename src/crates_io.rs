// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use bake::{Error, Result, Value};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::github::Repository;

const CRATES_IO_API: &str = "https://crates.io/api/v1";

fn api_base() -> String {
    #[cfg(test)]
    if let Some(api) = std::env::var_os("BAKE_TEST_CRATES_IO_API") {
        return api.to_string_lossy().into_owned();
    }

    CRATES_IO_API.to_owned()
}

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

#[derive(Debug, Deserialize)]
struct CrateVersionsResponse {
    #[serde(default)]
    versions: Vec<CrateVersion>,
}

#[derive(Debug, Deserialize)]
struct CrateVersion {
    num: String,
}

pub(crate) fn version_is_published(package: &str, version: &str) -> Result<bool> {
    validate_package_name(package)?;
    let endpoint = format!("{}/crates/{package}/versions", api_base());
    let response = match ureq::get(&endpoint)
        .set("User-Agent", "socketry-bake-publish-workflow")
        .call()
    {
        Ok(response) => response,
        Err(ureq::Error::Status(404, _)) => return Ok(false),
        Err(error) => return Err(api_error("inspect published crate versions", error)),
    };

    let response: CrateVersionsResponse = response
        .into_json()
        .map_err(|error| Error::new(format!("could not decode crates.io response: {error}")))?;
    Ok(response
        .versions
        .iter()
        .any(|published| published.num == version))
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
    configure_trusted_publisher_at(package, repository, workflow, environment, &api_base())
}

fn configure_trusted_publisher_at(
    package: &str,
    repository: &Repository,
    workflow: &str,
    environment: &str,
    api: &str,
) -> Result<Value> {
    validate_configuration(package, workflow, environment)?;
    let token = registry_token()?;

    let configurations = list_trusted_publishers(&token, package, api)?;
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

    let endpoint = format!("{api}/trusted_publishing/github_configs");
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
    set_trusted_publishing_only_at(package, required, &api_base())
}

fn set_trusted_publishing_only_at(package: &str, required: bool, api: &str) -> Result<Value> {
    validate_package_name(package)?;
    let token = registry_token()?;

    let endpoint = format!("{api}/crates/{package}");
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

fn list_trusted_publishers(token: &str, package: &str, api: &str) -> Result<Vec<TrustedPublisher>> {
    validate_package_name(package)?;
    let endpoint = format!("{api}/trusted_publishing/github_configs?crate={package}");
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
pub(crate) mod tests {
    use super::*;
    use crate::test_support::Environment;
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::thread;

    pub(crate) fn mock_server(
        responses: Vec<(u16, &'static str)>,
    ) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                let mut expected_length = None;
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    if expected_length.is_none()
                        && let Some(header_end) =
                            request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                    {
                        let headers = String::from_utf8_lossy(&request[..header_end]);
                        expected_length = headers.lines().find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        });
                        expected_length.get_or_insert(0);
                    }
                    if let Some(header_end) =
                        request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                        && expected_length
                            .is_some_and(|length| request.len() >= header_end + 4 + length)
                    {
                        break;
                    }
                }
                requests.push(String::from_utf8_lossy(&request).into_owned());
                let reason = if status < 400 {
                    "OK"
                } else {
                    "Unprocessable Entity"
                };
                write!(
                    stream,
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            requests
        });
        (format!("http://{address}/api/v1"), handle)
    }

    #[test]
    fn mock_server_stops_reading_when_the_client_closes_early() {
        let (api, server) = mock_server(vec![(200, "{}")]);
        let address = api
            .strip_prefix("http://")
            .unwrap()
            .split('/')
            .next()
            .unwrap();
        let mut client = TcpStream::connect(address).unwrap();
        client.write_all(b"partial request").unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        assert_eq!(server.join().unwrap(), ["partial request"]);
    }

    fn repository(owner: &str, name: &str) -> Repository {
        Repository {
            owner: owner.to_owned(),
            name: name.to_owned(),
        }
    }

    #[test]
    fn checks_crates_io_versions_for_an_exact_match() {
        let mut environment = Environment::new();
        let (api, server) = mock_server(vec![
            (200, r#"{"versions":[{"num":"1.2.3"},{"num":"1.2.2"}]}"#),
            (200, r#"{"versions":[{"num":"1.2.2"}]}"#),
            (200, "{}"),
        ]);
        environment.set("BAKE_TEST_CRATES_IO_API", &api);

        assert!(version_is_published("fixture", "1.2.3").unwrap());
        assert!(!version_is_published("fixture", "1.2.3").unwrap());
        assert!(!version_is_published("fixture", "1.2.3").unwrap());
        assert_eq!(server.join().unwrap().len(), 3);
    }

    #[test]
    fn treats_missing_crates_as_unpublished_and_reports_registry_errors() {
        let mut environment = Environment::new();
        assert!(version_is_published("bad/name", "1.2.3").is_err());

        let (api, server) = mock_server(vec![(404, "crate not found")]);
        environment.set("BAKE_TEST_CRATES_IO_API", &api);
        assert!(!version_is_published("missing", "1.2.3").unwrap());
        server.join().unwrap();

        let (api, server) = mock_server(vec![(403, "registry denied")]);
        environment.set("BAKE_TEST_CRATES_IO_API", &api);
        assert!(
            version_is_published("fixture", "1.2.3")
                .unwrap_err()
                .to_string()
                .contains("HTTP 403: registry denied")
        );
        server.join().unwrap();

        let (api, server) = mock_server(vec![(200, "not json")]);
        environment.set("BAKE_TEST_CRATES_IO_API", &api);
        assert!(
            version_is_published("fixture", "1.2.3")
                .unwrap_err()
                .to_string()
                .contains("could not decode crates.io response")
        );
        server.join().unwrap();
    }

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
        assert!(validate_configuration("socketry-crate", "", "crates-io").is_err());
        assert!(
            validate_configuration("socketry-crate", "publish.yml", "bad\\environment").is_err()
        );
        assert!(
            validate_configuration("socketry-crate", "publish.yml", "bad\nenvironment").is_err()
        );
        assert!(validate_package_name("bad/name").is_err());
        assert!(validate_package_name("").is_err());
        assert!(validate_package_name("okay_name-1").is_ok());

        let repository = repository("socketry", "example");
        assert!(trusted_publisher_plan("fixture", &repository, "bad.txt", "").is_err());
        assert!(validate_trusted_publisher_inputs("fixture", "bad.txt", "").is_err());
        assert!(
            configure_trusted_publisher_at(
                "fixture",
                &repository,
                "bad.txt",
                "",
                "http://127.0.0.1"
            )
            .is_err()
        );
        assert!(set_trusted_publishing_only_at("", true, "http://127.0.0.1").is_err());
        assert!(list_trusted_publishers("token", "", "http://127.0.0.1").is_err());
    }

    #[test]
    fn defaults_to_the_crates_io_api_when_no_test_override_is_set() {
        let mut environment = Environment::new();
        environment.remove("BAKE_TEST_CRATES_IO_API");

        assert_eq!(api_base(), CRATES_IO_API);
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

    #[test]
    fn trusted_publisher_plan_includes_a_named_environment() {
        let plan = trusted_publisher_plan(
            "socketry_crate",
            &repository("socketry", "example"),
            "publish.yaml",
            "crates-io",
        )
        .unwrap();
        assert_eq!(
            plan["endpoint"],
            "https://crates.io/api/v1/trusted_publishing/github_configs"
        );
        assert_eq!(plan["github_config"]["environment"], "crates-io");
    }

    #[test]
    fn creates_a_trusted_publisher_when_no_matching_configuration_exists() {
        let mut environment = Environment::new();
        environment.set("CARGO_REGISTRY_TOKEN", "token-value");
        let (api, server) = mock_server(vec![
            (200, r#"{"github_configs":[]}"#),
            (
                201,
                r#"{"github_config":{"id":7,"crate":"fixture","repository_owner":"socketry","repository_name":"example","workflow_filename":"publish.yml","environment":"crates-io"}}"#,
            ),
        ]);

        let result = configure_trusted_publisher_at(
            "fixture",
            &repository("socketry", "example"),
            "publish.yml",
            "crates-io",
            &api,
        )
        .unwrap();
        let requests = server.join().unwrap();

        assert_eq!(result["status"], "created");
        assert_eq!(result["github_config"]["id"], 7);
        assert!(
            requests[0].starts_with("GET /api/v1/trusted_publishing/github_configs?crate=fixture ")
        );
        assert!(
            requests[0]
                .to_ascii_lowercase()
                .contains("authorization: token-value")
        );
        assert!(requests[1].starts_with("POST /api/v1/trusted_publishing/github_configs "));
        assert!(requests[1].contains("\"environment\":\"crates-io\""));
    }

    #[test]
    fn existing_trusted_publisher_matches_repository_names_case_insensitively() {
        let mut environment = Environment::new();
        environment.set("CARGO_REGISTRY_TOKEN", "token-value");
        let (api, server) = mock_server(vec![(
            200,
            r#"{"github_configs":[{"id":7,"crate":"fixture","repository_owner":"SOCKETRY","repository_name":"EXAMPLE","workflow_filename":"publish.yml","environment":"crates-io"}]}"#,
        )]);

        let result = configure_trusted_publisher_at(
            "fixture",
            &repository("socketry", "example"),
            "publish.yml",
            "crates-io",
            &api,
        )
        .unwrap();
        let requests = server.join().unwrap();

        assert_eq!(result["status"], "already_configured");
        assert_eq!(requests.len(), 1);
    }

    #[test]
    fn mismatched_workflow_or_environment_creates_an_additional_configuration() {
        let mut environment = Environment::new();
        environment.set("CARGO_REGISTRY_TOKEN", "token-value");
        let (api, server) = mock_server(vec![
            (
                200,
                r#"{"github_configs":[{"id":7,"crate":"fixture","repository_owner":"socketry","repository_name":"example","workflow_filename":"old.yml","environment":null}]}"#,
            ),
            (
                201,
                r#"{"github_config":{"id":8,"crate":"fixture","repository_owner":"socketry","repository_name":"example","workflow_filename":"publish.yml","environment":null}}"#,
            ),
        ]);

        let result = configure_trusted_publisher_at(
            "fixture",
            &repository("socketry", "example"),
            "publish.yml",
            "",
            &api,
        )
        .unwrap();
        let requests = server.join().unwrap();

        assert_eq!(result["status"], "created");
        assert!(!requests[1].contains("\"environment\""));
    }

    #[test]
    fn empty_configuration_lists_default_to_no_publishers() {
        let mut environment = Environment::new();
        environment.set("CARGO_REGISTRY_TOKEN", "token-value");
        let (api, server) = mock_server(vec![
            (200, "{}"),
            (
                201,
                r#"{"github_config":{"id":8,"crate":"fixture","repository_owner":"socketry","repository_name":"example","workflow_filename":"publish.yml","environment":null}}"#,
            ),
        ]);

        assert_eq!(
            configure_trusted_publisher_at(
                "fixture",
                &repository("socketry", "example"),
                "publish.yml",
                "",
                &api,
            )
            .unwrap()["status"],
            "created"
        );
        assert_eq!(server.join().unwrap().len(), 2);
    }

    #[test]
    fn reports_registry_http_and_response_decoding_errors() {
        let mut environment = Environment::new();
        environment.set("CARGO_REGISTRY_TOKEN", "token-value");
        let (api, server) = mock_server(vec![(403, "permission denied")]);
        assert!(
            configure_trusted_publisher_at(
                "fixture",
                &repository("socketry", "example"),
                "publish.yml",
                "",
                &api,
            )
            .unwrap_err()
            .to_string()
            .contains("HTTP 403: permission denied")
        );
        server.join().unwrap();

        let (api, server) = mock_server(vec![(200, "{}"), (403, "configuration denied")]);
        assert!(
            configure_trusted_publisher_at(
                "fixture",
                &repository("socketry", "example"),
                "publish.yml",
                "",
                &api,
            )
            .unwrap_err()
            .to_string()
            .contains("HTTP 403: configuration denied")
        );
        server.join().unwrap();

        let (api, server) = mock_server(vec![(200, "not json")]);
        assert!(
            list_trusted_publishers("token-value", "fixture", &api)
                .unwrap_err()
                .to_string()
                .contains("could not decode crates.io response")
        );
        server.join().unwrap();

        let (api, server) = mock_server(vec![(200, "{}"), (201, "{}")]);
        assert!(
            configure_trusted_publisher_at(
                "fixture",
                &repository("socketry", "example"),
                "publish.yml",
                "",
                &api,
            )
            .unwrap_err()
            .to_string()
            .contains("could not decode crates.io response")
        );
        server.join().unwrap();
    }

    #[test]
    fn formats_non_http_transport_errors() {
        let error = ureq::get("http://127.0.0.1:0/unavailable")
            .call()
            .unwrap_err();
        assert!(
            api_error("connect to registry", error)
                .to_string()
                .contains("could not connect to registry")
        );
    }

    #[test]
    fn updates_trusted_publishing_only_and_reports_http_errors() {
        let mut environment = Environment::new();
        environment.set("CARGO_REGISTRY_TOKEN", "token-value");
        let (api, server) = mock_server(vec![(200, r#"{"crate":{"trustpub_only":true}}"#)]);
        let result = set_trusted_publishing_only_at("fixture", true, &api).unwrap();
        let requests = server.join().unwrap();
        assert_eq!(result["status"], "updated");
        assert_eq!(result["trustpub_only"], true);
        assert!(requests[0].starts_with("PATCH /api/v1/crates/fixture "));
        assert!(requests[0].contains("\"trustpub_only\":true"));

        let (api, server) = mock_server(vec![(422, "cannot disable trusted publishing")]);
        assert!(
            set_trusted_publishing_only_at("fixture", false, &api)
                .unwrap_err()
                .to_string()
                .contains("HTTP 422: cannot disable trusted publishing")
        );
        server.join().unwrap();

        let (api, server) = mock_server(vec![(200, "not json")]);
        assert!(
            set_trusted_publishing_only_at("fixture", false, &api)
                .unwrap_err()
                .to_string()
                .contains("could not decode crates.io response")
        );
        server.join().unwrap();
    }

    #[test]
    fn reports_missing_or_empty_registry_tokens_before_network_access() {
        let mut environment = Environment::new();
        environment.remove("CARGO_REGISTRY_TOKEN");
        assert!(
            validate_trusted_publisher_inputs("fixture", "publish.yml", "")
                .unwrap_err()
                .to_string()
                .contains("set CARGO_REGISTRY_TOKEN")
        );
        assert!(
            configure_trusted_publisher(
                "fixture",
                &repository("socketry", "example"),
                "publish.yml",
                ""
            )
            .is_err()
        );
        assert!(set_trusted_publishing_only("fixture", true).is_err());

        environment.set("CARGO_REGISTRY_TOKEN", "  \n");
        assert!(
            registry_token()
                .unwrap_err()
                .to_string()
                .contains("CARGO_REGISTRY_TOKEN is empty")
        );
    }
}
