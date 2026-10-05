// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use bake::{Context, Error, Result, Value};
use serde_json::{Value as JsonValue, json};
use std::io::Write;
use std::process::{ChildStdin, Output, Stdio};

#[derive(Clone, Debug)]
pub(crate) struct Repository {
    pub owner: String,
    pub name: String,
}

impl Repository {
    pub(crate) fn resolve(context: &Context, value: &str) -> Result<Self> {
        if value.is_empty() {
            return Self::from_origin(context);
        }
        let (owner, name) = value
            .split_once('/')
            .ok_or_else(|| Error::new("repository must use owner/name format"))?;
        Self::new(owner, name)
    }

    pub(crate) fn from_origin(context: &Context) -> Result<Self> {
        let output = context
            .command("git")
            .args(["remote", "get-url", "origin"])
            .output()?;
        if !output.status.success() {
            return Err(Error::new(format!(
                "could not read the origin Git remote: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let remote = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let path = if let Some(path) = remote.strip_prefix("https://github.com/") {
            path
        } else if let Some(path) = remote.strip_prefix("http://github.com/") {
            path
        } else if let Some(path) = remote.strip_prefix("ssh://git@github.com/") {
            path
        } else if let Some(path) = remote.strip_prefix("git@github.com:") {
            path
        } else {
            return Err(Error::new(format!(
                "origin remote {remote:?} is not a github.com repository"
            )));
        };
        let path = path.strip_suffix(".git").unwrap_or(path);
        let (owner, name) = path
            .split_once('/')
            .ok_or_else(|| Error::new("GitHub origin must include owner and repository name"))?;
        if name.contains('/') {
            return Err(Error::new(
                "GitHub origin has more than one repository path component",
            ));
        }
        Self::new(owner, name)
    }

    fn new(owner: &str, name: &str) -> Result<Self> {
        if !valid_repository_part(owner) || !valid_repository_part(name) {
            return Err(Error::new(
                "GitHub owner and repository must contain only letters, numbers, dots, underscores, or hyphens",
            ));
        }
        Ok(Self {
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }

    pub(crate) fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

fn valid_repository_part(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub(crate) fn validate_branch(branch: &str) -> Result<()> {
    if valid_repository_part(branch) {
        Ok(())
    } else {
        Err(Error::new(
            "branch must contain only letters, numbers, dots, underscores, or hyphens",
        ))
    }
}

pub(crate) fn effective_checks(checks: &[String]) -> Vec<String> {
    if checks.is_empty() {
        vec!["check".to_owned(), "test-result".to_owned()]
    } else {
        checks.to_vec()
    }
}

pub(crate) fn validate_setup(
    branch: &str,
    approvals: u32,
    checks: &[String],
    reviewers: &[String],
    wait_timer: Option<u32>,
    environment: &str,
) -> Result<()> {
    validate_branch(branch)?;
    if approvals > 6 {
        return Err(Error::new(
            "GitHub rulesets allow between zero and six required pull request approvals",
        ));
    }
    if checks.iter().any(|check| check.trim().is_empty()) {
        return Err(Error::new("status check names cannot be empty"));
    }
    if reviewers.len() > 6 {
        return Err(Error::new(
            "GitHub environments allow at most six reviewers",
        ));
    }
    if reviewers.is_empty() {
        return Err(Error::new(
            "configure at least one required environment reviewer before enabling crates.io publishing",
        ));
    }
    for reviewer in reviewers {
        validate_reviewer_reference(reviewer)?;
    }
    if wait_timer.is_some_and(|wait_timer| wait_timer > 43_200) {
        return Err(Error::new(
            "environment wait timer must be between zero and 43,200 minutes",
        ));
    }
    if environment.is_empty() || !valid_repository_part(environment) {
        return Err(Error::new(
            "environment must contain only letters, numbers, dots, underscores, or hyphens",
        ));
    }
    Ok(())
}

fn parse_reviewer(value: &str) -> Result<(&str, u64)> {
    let (kind, identifier) = value
        .split_once(':')
        .ok_or_else(|| Error::new(format!("reviewer {value:?} must use User:ID or Team:ID")))?;
    if !matches!(kind, "User" | "Team") {
        return Err(Error::new(format!(
            "reviewer type in {value:?} must be User or Team"
        )));
    }
    let identifier = identifier.parse::<u64>().map_err(|_| {
        Error::new(format!(
            "reviewer {value:?} must contain a numeric GitHub ID"
        ))
    })?;
    Ok((kind, identifier))
}

fn validate_reviewer_reference(value: &str) -> Result<()> {
    if parse_reviewer(value).is_ok() {
        return Ok(());
    }

    let valid_part = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    };
    if let Some((organization, team)) = value.split_once('/') {
        if !valid_part(organization) || !valid_part(team) || team.contains('/') {
            return Err(Error::new(format!(
                "reviewer {value:?} must use a GitHub user login or organization/team slug"
            )));
        }
    } else if !valid_part(value) {
        return Err(Error::new(format!(
            "reviewer {value:?} must use a GitHub user login or organization/team slug"
        )));
    }

    Ok(())
}

#[derive(Debug)]
pub(crate) struct ReviewerSet {
    pub configured: Vec<String>,
    pub resolved: Vec<String>,
}

/// Resolve configured user logins and organization/team slugs to GitHub IDs.
/// Explicit `User:ID` and `Team:ID` values bypass the lookup.
pub(crate) fn resolve_reviewers(
    context: &Context,
    repository: &Repository,
    reviewers: &[String],
) -> Result<ReviewerSet> {
    let resolved = reviewers
        .iter()
        .map(|reviewer| {
            validate_reviewer_reference(reviewer)?;
            if parse_reviewer(reviewer).is_ok() {
                return Ok(reviewer.clone());
            }

            let (kind, path) = if let Some((organization, team)) = reviewer.split_once('/') {
                if !organization.eq_ignore_ascii_case(&repository.owner) {
                    return Err(Error::new(format!(
                        "reviewer team {reviewer:?} must belong to GitHub owner {:?}",
                        repository.owner
                    )));
                }
                ("Team", format!("orgs/{organization}/teams/{team}"))
            } else {
                ("User", format!("users/{reviewer}"))
            };
            let response = github_api(context, "GET", &path, None)?;
            let identifier = response
                .get("id")
                .and_then(JsonValue::as_u64)
                .ok_or_else(|| {
                    Error::new(format!(
                        "GitHub returned no numeric ID for reviewer {reviewer:?}"
                    ))
                })?;

            Ok(format!("{kind}:{identifier}"))
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(ReviewerSet {
        configured: reviewers.to_vec(),
        resolved,
    })
}

pub(crate) fn setup_plan(
    repository: &Repository,
    branch: &str,
    approvals: u32,
    checks: &[String],
    reviewers: &ReviewerSet,
    wait_timer: Option<u32>,
    environment: &str,
) -> Value {
    let mut environment_settings = environment_payload(&reviewers.resolved, wait_timer, None);
    environment_settings["name"] = json!(environment);

    json!({
        "repository": repository.full_name(),
        "branch_ruleset": branch_ruleset(branch, approvals, checks),
        "tag_ruleset": tag_ruleset(),
        "reviewers": {"configured": reviewers.configured, "resolved": reviewers.resolved},
        "environment": environment_settings,
        "preservation": "Existing environment settings are preserved unless overridden above.",
        "apply": "cargo:setup:github:apply",
    })
}

fn branch_ruleset(branch: &str, approvals: u32, checks: &[String]) -> JsonValue {
    let mut rules = vec![json!({
        "type": "pull_request",
        "parameters": {
            "allowed_merge_methods": ["squash", "rebase"],
            "dismiss_stale_reviews_on_push": true,
            "require_code_owner_review": false,
            "require_last_push_approval": false,
            "required_approving_review_count": approvals,
            "required_review_thread_resolution": true,
        }
    })];
    rules.push(json!({
        "type": "required_status_checks",
        "parameters": {
            "do_not_enforce_on_create": false,
            "required_status_checks": checks.iter().map(|check| json!({"context": check})).collect::<Vec<_>>(),
            "strict_required_status_checks_policy": true,
        }
    }));
    rules.push(json!({"type": "non_fast_forward"}));

    // GitHub's repository role ID 5 is administrators. Restrict their bypass
    // to pull requests so branch rules continue to block direct pushes.
    json!({
        "name": "Socketry Cargo checks",
        "target": "branch",
        "enforcement": "active",
        "conditions": {"ref_name": {"include": [format!("refs/heads/{branch}")], "exclude": []}},
        "rules": rules,
        "bypass_actors": [{
            "actor_id": 5,
            "actor_type": "RepositoryRole",
            "bypass_mode": "pull_request",
        }],
    })
}

fn tag_ruleset() -> JsonValue {
    json!({
        "name": "Socketry Cargo release tags",
        "target": "tag",
        "enforcement": "active",
        "conditions": {"ref_name": {"include": ["refs/tags/v*"], "exclude": []}},
        "rules": [{"type": "deletion"}],
        "bypass_actors": [],
    })
}

fn environment_payload(
    requested_reviewers: &[String],
    wait_timer: Option<u32>,
    existing: Option<&JsonValue>,
) -> JsonValue {
    let mut payload = json!({});
    let existing_protection_rules = existing
        .and_then(|environment| environment.get("protection_rules"))
        .and_then(JsonValue::as_array);
    let existing_wait_timer = existing_protection_rules
        .into_iter()
        .flatten()
        .find(|rule| rule.get("type").and_then(JsonValue::as_str) == Some("wait_timer"))
        .and_then(|rule| rule.get("wait_timer"))
        .and_then(JsonValue::as_u64);
    if let Some(wait_timer) = wait_timer.or(existing_wait_timer.map(|timer| timer as u32)) {
        payload["wait_timer"] = json!(wait_timer);
    }

    if !requested_reviewers.is_empty() {
        let reviewers: Vec<_> = requested_reviewers
            .iter()
            .filter_map(|reviewer| parse_reviewer(reviewer).ok())
            .map(|(kind, identifier)| json!({"type": kind, "id": identifier}))
            .collect();
        payload["prevent_self_review"] = json!(false);
        payload["reviewers"] = json!(reviewers);
    } else if let Some(existing_rules) = existing_protection_rules
        && let Some(review_rule) = existing_rules
            .iter()
            .find(|rule| rule.get("type").and_then(JsonValue::as_str) == Some("required_reviewers"))
    {
        if let Some(prevent_self_review) = review_rule.get("prevent_self_review") {
            payload["prevent_self_review"] = prevent_self_review.clone();
        }
        if let Some(reviewers) = review_rule.get("reviewers").and_then(JsonValue::as_array) {
            payload["reviewers"] = json!(
                reviewers
                    .iter()
                    .filter_map(|reviewer| {
                        let kind = reviewer.get("type")?.as_str()?;
                        let identifier = reviewer.get("reviewer")?.get("id")?.as_u64()?;
                        Some(json!({"type": kind, "id": identifier}))
                    })
                    .collect::<Vec<_>>()
            );
        }
    }

    if let Some(branch_policy) =
        existing.and_then(|environment| environment.get("deployment_branch_policy"))
    {
        payload["deployment_branch_policy"] = branch_policy.clone();
    }
    payload
}

fn existing_environment(
    context: &Context,
    repository: &Repository,
    environment: &str,
) -> Result<Option<JsonValue>> {
    let list_path = format!("repos/{}/environments?per_page=100", repository.full_name());
    let response = github_api(context, "GET", &list_path, None)?;
    let environments = response
        .get("environments")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| Error::new("GitHub returned an invalid environments response"))?;
    if !environments
        .iter()
        .any(|existing| existing.get("name").and_then(JsonValue::as_str) == Some(environment))
    {
        return Ok(None);
    }

    let path = format!(
        "repos/{}/environments/{environment}",
        repository.full_name()
    );
    github_api(context, "GET", &path, None).map(Some)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_setup(
    context: &Context,
    repository: &Repository,
    branch: &str,
    approvals: u32,
    checks: &[String],
    reviewers: &[String],
    wait_timer: Option<u32>,
    environment: &str,
) -> Result<Value> {
    let names = crate::cargo_support::workspace_packages(context)?
        .into_iter()
        .map(|package| package.name)
        .collect::<Vec<_>>();
    if names.is_empty() {
        return Err(Error::new(
            "the workspace has no publishable packages to protect",
        ));
    }
    let existing_environment = existing_environment(context, repository, environment)?;
    let branch_ruleset = branch_ruleset(branch, approvals, checks);
    let tag_ruleset = tag_ruleset();

    let branch_result = upsert_ruleset(context, repository, &branch_ruleset)?;
    let tag_result = upsert_ruleset(context, repository, &tag_ruleset)?;
    let environment_body =
        environment_payload(reviewers, wait_timer, existing_environment.as_ref());
    let environment_path = format!(
        "repos/{}/environments/{}",
        repository.full_name(),
        environment
    );
    let environment_result =
        github_api(context, "PUT", &environment_path, Some(&environment_body))?;

    Ok(json!({
        "repository": repository.full_name(),
        "branch_ruleset": branch_result,
        "tag_ruleset": tag_result,
        "environment": environment_result,
    }))
}

fn upsert_ruleset(
    context: &Context,
    repository: &Repository,
    desired: &JsonValue,
) -> Result<JsonValue> {
    let name = desired
        .get("name")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| Error::new("managed ruleset has no name"))?;
    let path = format!("repos/{}/rulesets?per_page=100", repository.full_name());
    let existing = github_api(context, "GET", &path, None)?;
    let rulesets = existing
        .as_array()
        .ok_or_else(|| Error::new("GitHub returned an invalid rulesets response"))?;
    let matches: Vec<_> = rulesets
        .iter()
        .filter(|ruleset| ruleset.get("name").and_then(JsonValue::as_str) == Some(name))
        .collect();
    if matches.len() > 1 {
        return Err(Error::new(format!(
            "multiple GitHub rulesets are named {name:?}"
        )));
    }

    let payload = desired.clone();
    if let Some(existing) = matches.first() {
        let identifier = existing
            .get("id")
            .and_then(JsonValue::as_u64)
            .ok_or_else(|| Error::new(format!("GitHub ruleset {name:?} has no numeric ID")))?;
        let path = format!("repos/{}/rulesets/{identifier}", repository.full_name());
        github_api(context, "PUT", &path, Some(&payload))
    } else {
        let path = format!("repos/{}/rulesets", repository.full_name());
        github_api(context, "POST", &path, Some(&payload))
    }
}

pub(crate) fn github_api(
    context: &Context,
    method: &str,
    path: &str,
    body: Option<&JsonValue>,
) -> Result<JsonValue> {
    let mut command = context.command("gh");
    command.args(["api", "--method", method, path]);
    if body.is_some() {
        command.args(["--input", "-"]).stdin(Stdio::piped());
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| Error::new(format!("could not start GitHub CLI `gh`: {error}")))?;
    if let Some(body) = body {
        write_api_input(child.stdin.take(), body)?;
    }
    read_api_response(method, path, || child.wait_with_output())
}

fn read_api_response(
    method: &str,
    path: &str,
    wait: impl FnOnce() -> std::io::Result<Output>,
) -> Result<JsonValue> {
    let output = wait()?;
    if !output.status.success() {
        return Err(Error::new(format!(
            "GitHub API request {method} {path} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    if output.stdout.is_empty() {
        return Ok(JsonValue::Null);
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| Error::new(format!("could not parse GitHub API response: {error}")))
}

fn write_api_input<T: serde::Serialize>(stdin: Option<ChildStdin>, body: &T) -> Result<()> {
    let contents = serde_json::to_vec(body)?;
    stdin
        .ok_or_else(|| Error::new("could not open GitHub CLI input"))?
        .write_all(&contents)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Environment, Project, shell_quote};
    use std::process::Command;

    fn with_fake_gh(project: &Project, environment: &mut Environment, script: &str) {
        project.executable("gh", script);
        environment.prepend_path(&project.root().join("bin"));
    }

    fn initialize_origin(project: &Project, url: &str) {
        assert!(
            Command::new("git")
                .current_dir(project.root())
                .args(["init", "--quiet"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .current_dir(project.root())
                .args(["remote", "add", "origin", url])
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn defaults_to_publish_and_test_workflow_checks() {
        assert_eq!(
            effective_checks(&[]),
            vec!["check".to_owned(), "test-result".to_owned()]
        );
        assert_eq!(
            effective_checks(&["custom check".to_owned()]),
            vec!["custom check".to_owned()]
        );
    }

    #[test]
    fn environment_setup_allows_the_initiator_to_approve() {
        let payload = environment_payload(&["User:123".to_owned()], None, None);

        assert!(!payload["prevent_self_review"].as_bool().unwrap());
        assert_eq!(payload["reviewers"][0]["type"], "User");
        assert_eq!(payload["reviewers"][0]["id"], 123);
        assert!(payload.get("name").is_none());
    }

    #[test]
    fn validates_repository_setup_inputs() {
        assert!(
            validate_setup("main", 1, &[], &["User:123".to_owned()], None, "crates-io").is_ok()
        );
        assert!(
            validate_setup(
                "main",
                1,
                &[],
                &["socketry/managers".to_owned()],
                None,
                "crates-io"
            )
            .is_ok()
        );
        assert!(
            validate_setup("main", 1, &[], &["ioquatix".to_owned()], None, "crates-io").is_ok()
        );
        assert!(validate_setup("main", 1, &[], &[], None, "crates-io").is_err());
        assert!(validate_setup("feature/release", 1, &[], &[], None, "crates-io").is_err());
        assert!(validate_setup("main", 7, &[], &[], None, "crates-io").is_err());
        assert!(validate_setup("main", 1, &[" ".to_owned()], &[], None, "crates-io").is_err());
        assert!(
            validate_setup(
                "main",
                1,
                &[],
                &["Team:not-a-number".to_owned()],
                None,
                "crates-io"
            )
            .is_err()
        );
        assert!(validate_setup("main", 1, &[], &[], Some(43_201), "crates-io").is_err());
        assert!(validate_setup("main", 1, &[], &[], None, "crates/io").is_err());
        assert!(
            validate_setup(
                "main",
                1,
                &[],
                &[
                    "a".to_owned(),
                    "b".to_owned(),
                    "c".to_owned(),
                    "d".to_owned(),
                    "e".to_owned(),
                    "f".to_owned(),
                    "g".to_owned(),
                ],
                None,
                "crates-io"
            )
            .is_err()
        );
        assert!(
            validate_setup(
                "main",
                1,
                &[],
                &["User:123".to_owned()],
                Some(43_201),
                "crates-io"
            )
            .is_err()
        );
        assert!(validate_setup("main", 1, &[], &["User:123".to_owned()], None, "").is_err());
    }

    #[test]
    fn accepts_only_supported_reviewer_identifiers() {
        assert_eq!(parse_reviewer("User:123").unwrap(), ("User", 123));
        assert_eq!(parse_reviewer("Team:456").unwrap(), ("Team", 456));
        assert!(parse_reviewer("user:123").is_err());
        assert!(parse_reviewer("Team:abc").is_err());
        assert!(validate_reviewer_reference("bad org/team").is_err());
        assert!(validate_reviewer_reference("socketry/bad/team").is_err());
        assert!(validate_reviewer_reference("bad login").is_err());
    }

    #[test]
    fn resolves_and_validates_github_repository_names() {
        let _environment = Environment::new();
        let project = Project::new();
        let context = project.context();

        let repository = Repository::resolve(&context, "socketry/example").unwrap();
        assert_eq!(repository.full_name(), "socketry/example");
        assert!(Repository::resolve(&context, "").is_err());
        assert!(Repository::resolve(&context, "missing-slash").is_err());
        assert!(Repository::resolve(&context, "socketry/nested/repository").is_err());
        assert!(Repository::resolve(&context, "bad owner/example").is_err());
        assert!(validate_branch("release/1.2.3").is_err());
        assert!(validate_branch("main").is_ok());
    }

    #[test]
    fn reads_supported_github_origin_url_forms() {
        let _environment = Environment::new();
        for url in [
            "https://github.com/socketry/example.git",
            "http://github.com/socketry/example",
            "ssh://git@github.com/socketry/example.git",
            "git@github.com:socketry/example.git",
        ] {
            let project = Project::new();
            initialize_origin(&project, url);
            assert_eq!(
                Repository::from_origin(&project.context())
                    .unwrap()
                    .full_name(),
                "socketry/example"
            );
        }
    }

    #[test]
    fn reports_an_origin_command_failure_for_a_git_repository_without_origin() {
        let mut environment = Environment::new();
        let project = Project::new();
        assert!(
            Command::new("git")
                .current_dir(project.root())
                .args(["init", "--quiet"])
                .status()
                .unwrap()
                .success()
        );

        assert!(
            Repository::from_origin(&project.context())
                .unwrap_err()
                .to_string()
                .contains("could not read the origin Git remote")
        );

        environment.set("PATH", project.root().as_os_str());
        assert!(Repository::from_origin(&project.context()).is_err());
    }

    #[test]
    fn rejects_non_github_and_malformed_origin_urls() {
        let _environment = Environment::new();
        for url in [
            "https://example.com/socketry/example",
            "https://github.com/socketry",
            "https://github.com/socketry/nested/example",
            "https://github.com/bad owner/example",
        ] {
            let project = Project::new();
            initialize_origin(&project, url);
            assert!(
                Repository::from_origin(&project.context()).is_err(),
                "{url}"
            );
        }
    }

    #[test]
    fn resolves_user_and_team_reviewers_and_rejects_invalid_lookups() {
        let mut environment = Environment::new();
        let project = Project::new();
        with_fake_gh(
            &project,
            &mut environment,
            "#!/bin/sh\ncase \"$*\" in\n  'api --method GET users/ioquatix') printf '%s' '{\"id\":42}' ;;\n  'api --method GET orgs/Socketry/teams/managers') printf '%s' '{\"id\":99}' ;;\n  'api --method GET users/missing') printf '%s' '{}' ;;\n  'api --method GET users/denied') echo denied >&2; exit 1 ;;\n  *) echo unexpected-request >&2; exit 1 ;;\nesac\n",
        );
        let repository = Repository {
            owner: "socketry".to_owned(),
            name: "example".to_owned(),
        };
        let resolved = resolve_reviewers(
            &project.context(),
            &repository,
            &[
                "User:7".to_owned(),
                "ioquatix".to_owned(),
                "Socketry/managers".to_owned(),
            ],
        )
        .unwrap();
        assert_eq!(resolved.configured[0], "User:7");
        assert_eq!(resolved.resolved, ["User:7", "User:42", "Team:99"]);
        assert!(
            resolve_reviewers(&project.context(), &repository, &["bad login".to_owned()]).is_err()
        );
        assert!(
            resolve_reviewers(
                &project.context(),
                &repository,
                &["other-org/managers".to_owned()]
            )
            .unwrap_err()
            .to_string()
            .contains("must belong to GitHub owner")
        );
        assert!(
            resolve_reviewers(&project.context(), &repository, &["missing".to_owned()])
                .unwrap_err()
                .to_string()
                .contains("no numeric ID")
        );
        assert!(
            resolve_reviewers(&project.context(), &repository, &["denied".to_owned()])
                .unwrap_err()
                .to_string()
                .contains("denied")
        );
    }

    #[test]
    fn github_api_handles_json_empty_responses_and_errors() {
        let mut environment = Environment::new();
        let project = Project::new();
        with_fake_gh(
            &project,
            &mut environment,
            "#!/bin/sh\ncase \"$*\" in\n  'api --method GET json') printf '%s' '{\"ok\":true}' ;;\n  'api --method GET empty') : ;;\n  'api --method GET invalid') printf '{' ;;\n  'api --method GET failure') echo denied >&2; exit 4 ;;\n  'api --method PUT update --input -') cat >/dev/null; printf '%s' '{\"updated\":true}' ;;\n  *) echo unexpected-request >&2; exit 1 ;;\nesac\n",
        );
        let context = project.context();
        assert_eq!(
            github_api(&context, "GET", "json", None).unwrap()["ok"],
            true
        );
        assert!(
            github_api(&context, "GET", "empty", None)
                .unwrap()
                .is_null()
        );
        assert!(
            github_api(&context, "GET", "invalid", None)
                .unwrap_err()
                .to_string()
                .contains("could not parse GitHub API response")
        );
        assert!(
            github_api(&context, "GET", "failure", None)
                .unwrap_err()
                .to_string()
                .contains("denied")
        );
        assert_eq!(
            github_api(&context, "PUT", "update", Some(&json!({"x": 1}))).unwrap()["updated"],
            true
        );
    }

    #[test]
    fn github_api_reports_an_unavailable_cli() {
        let mut environment = Environment::new();
        let project = Project::new();
        environment.set("PATH", project.root().as_os_str());
        assert!(
            github_api(&project.context(), "GET", "users/test", None)
                .unwrap_err()
                .to_string()
                .contains("could not start GitHub CLI")
        );
    }

    #[test]
    fn reports_a_failure_while_waiting_for_the_github_cli() {
        assert!(
            read_api_response("GET", "fixture", || {
                Err(std::io::Error::other("wait failed"))
            })
            .unwrap_err()
            .to_string()
            .contains("wait failed")
        );
    }

    #[test]
    fn reports_missing_github_cli_input() {
        let mut environment = Environment::new();
        assert!(
            write_api_input(None, &json!({}))
                .unwrap_err()
                .to_string()
                .contains("could not open GitHub CLI input")
        );

        let project = Project::new();
        let ready = project.root().join("reader-closed");
        let script = format!("exec 0<&-; touch {}; sleep 1", shell_quote(&ready));
        let mut child = Command::new("sh")
            .args(["-c", script.as_str()])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        for _ in 0..200 {
            if ready.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            ready.exists(),
            "child closed stdin before the test timed out"
        );
        let error =
            write_api_input(child.stdin.take(), &json!({"value": "closed pipe"})).unwrap_err();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(error.to_string().contains("Broken pipe"));

        with_fake_gh(&project, &mut environment, "#!/bin/sh\nexec 0<&-\n");
        assert!(
            github_api(
                &project.context(),
                "PUT",
                "closed-pipe",
                Some(&json!({"x": "x".repeat(1024 * 1024)})),
            )
            .unwrap_err()
            .to_string()
            .contains("Broken pipe")
        );

        struct FailsSerialization;

        impl serde::Serialize for FailsSerialization {
            fn serialize<S>(&self, _serializer: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                Err(serde::ser::Error::custom("injected serialization failure"))
            }
        }

        assert!(
            write_api_input(None, &FailsSerialization)
                .unwrap_err()
                .to_string()
                .contains("injected serialization failure")
        );
    }

    #[test]
    fn preserves_existing_environment_protection_settings() {
        let existing = json!({
            "protection_rules": [
                {"type": "wait_timer", "wait_timer": 15},
                {"type": "required_reviewers", "prevent_self_review": true, "reviewers": [
                    {"type": "Team", "reviewer": {"id": 42}},
                    {"type": "User", "reviewer": {"id": 7}},
                    {"type": "Team", "reviewer": {}},
                    {},
                    {"type": 7},
                    {"type": "User"},
                    {"type": "User", "reviewer": {"id": "not numeric"}}
                ]}
            ],
            "deployment_branch_policy": {"protected_branches": true}
        });
        let payload = environment_payload(&[], None, Some(&existing));

        assert_eq!(payload["wait_timer"], 15);
        assert_eq!(payload["prevent_self_review"], true);
        assert_eq!(
            payload["reviewers"],
            json!([
                {"type": "Team", "id": 42},
                {"type": "User", "id": 7}
            ])
        );
        assert_eq!(
            payload["deployment_branch_policy"]["protected_branches"],
            true
        );
    }

    #[test]
    fn tolerates_existing_review_rules_with_incomplete_settings() {
        let existing = json!({
            "protection_rules": [{"type": "required_reviewers", "prevent_self_review": false}]
        });
        let payload = environment_payload(&[], None, Some(&existing));
        assert_eq!(payload["prevent_self_review"], false);
        assert!(payload.get("reviewers").is_none());
    }

    #[test]
    fn preserves_existing_reviewers_when_self_review_setting_is_absent() {
        let existing = json!({
            "protection_rules": [{
                "type": "required_reviewers",
                "reviewers": [{"type": "User", "reviewer": {"id": 7}}]
            }]
        });
        let payload = environment_payload(&[], None, Some(&existing));

        assert!(payload.get("prevent_self_review").is_none());
        assert_eq!(payload["reviewers"], json!([{"type": "User", "id": 7}]));
    }

    #[test]
    fn preserves_existing_environment_without_a_reviewer_rule() {
        let existing = json!({
            "protection_rules": [{"type": "wait_timer", "wait_timer": 15}],
            "deployment_branch_policy": {"protected_branches": true}
        });
        let payload = environment_payload(&[], None, Some(&existing));

        assert_eq!(payload["wait_timer"], 15);
        assert_eq!(
            payload["deployment_branch_policy"]["protected_branches"],
            true
        );
        assert!(payload.get("prevent_self_review").is_none());
        assert!(payload.get("reviewers").is_none());
    }

    #[test]
    fn explicit_environment_values_override_preserved_values() {
        let existing = json!({
            "protection_rules": [{"type": "wait_timer", "wait_timer": 15}],
            "deployment_branch_policy": {"protected_branches": true}
        });
        let payload = environment_payload(&["Team:123".to_owned()], Some(30), Some(&existing));

        assert_eq!(payload["wait_timer"], 30);
        assert_eq!(payload["reviewers"][0]["id"], 123);
        assert_eq!(
            payload["deployment_branch_policy"]["protected_branches"],
            true
        );
    }

    #[test]
    fn discovers_existing_environments_and_rejects_malformed_lists() {
        let mut environment = Environment::new();
        let project = Project::new();
        with_fake_gh(
            &project,
            &mut environment,
            "#!/bin/sh\ncase \"$3 $4\" in\n  'GET repos/socketry/example/environments?per_page=100') printf '%s' '{\"environments\":[{\"name\":\"crates-io\"}]}' ;;\n  'GET repos/socketry/example/environments/crates-io') printf '%s' '{\"name\":\"crates-io\"}' ;;\n  *) echo unexpected-request >&2; exit 1 ;;\nesac\n",
        );
        let repository = Repository {
            owner: "socketry".to_owned(),
            name: "example".to_owned(),
        };
        assert_eq!(
            existing_environment(&project.context(), &repository, "crates-io").unwrap(),
            Some(json!({"name": "crates-io"}))
        );

        let malformed = Project::new();
        with_fake_gh(
            &malformed,
            &mut environment,
            "#!/bin/sh\nprintf '%s' '{\"environments\":null}'\n",
        );
        assert!(
            existing_environment(&malformed.context(), &repository, "crates-io")
                .unwrap_err()
                .to_string()
                .contains("invalid environments response")
        );

        let failed = Project::new();
        with_fake_gh(
            &failed,
            &mut environment,
            "#!/bin/sh\necho environment lookup denied >&2; exit 1\n",
        );
        assert!(
            existing_environment(&failed.context(), &repository, "crates-io")
                .unwrap_err()
                .to_string()
                .contains("environment lookup denied")
        );
    }

    #[test]
    fn creates_and_updates_rulesets_by_their_managed_name() {
        let mut environment = Environment::new();
        let project = Project::new();
        with_fake_gh(
            &project,
            &mut environment,
            "#!/bin/sh\ncase \"$3 $4\" in\n  'GET repos/socketry/example/rulesets?per_page=100') printf '%s' '[]' ;;\n  'POST repos/socketry/example/rulesets') cat >/dev/null; printf '%s' '{\"id\":7}' ;;\n  *) echo unexpected-request >&2; exit 1 ;;\nesac\n",
        );
        let repository = Repository {
            owner: "socketry".to_owned(),
            name: "example".to_owned(),
        };
        assert_eq!(
            upsert_ruleset(
                &project.context(),
                &repository,
                &branch_ruleset("main", 1, &[])
            )
            .unwrap(),
            json!({"id": 7})
        );

        let existing = Project::new();
        with_fake_gh(
            &existing,
            &mut environment,
            "#!/bin/sh\ncase \"$3 $4\" in\n  'GET repos/socketry/example/rulesets?per_page=100') printf '%s' '[{\"name\":\"Socketry Cargo checks\",\"id\":42}]' ;;\n  'PUT repos/socketry/example/rulesets/42') cat >/dev/null; printf '%s' '{\"updated\":true}' ;;\n  *) echo unexpected-request >&2; exit 1 ;;\nesac\n",
        );
        assert_eq!(
            upsert_ruleset(
                &existing.context(),
                &repository,
                &branch_ruleset("main", 1, &[])
            )
            .unwrap(),
            json!({"updated": true})
        );
    }

    #[test]
    fn rejects_duplicate_missing_id_and_malformed_ruleset_responses() {
        let mut environment = Environment::new();
        let repository = Repository {
            owner: "socketry".to_owned(),
            name: "example".to_owned(),
        };
        assert!(
            upsert_ruleset(&bake::Registry::new().context("."), &repository, &json!({}))
                .unwrap_err()
                .to_string()
                .contains("managed ruleset has no name")
        );
        for (response, expected) in [
            (
                "[{\"name\":\"Socketry Cargo checks\",\"id\":42},{\"name\":\"Socketry Cargo checks\",\"id\":43}]",
                "multiple GitHub rulesets",
            ),
            ("[{\"name\":\"Socketry Cargo checks\"}]", "no numeric ID"),
            ("{}", "invalid rulesets response"),
        ] {
            let project = Project::new();
            with_fake_gh(
                &project,
                &mut environment,
                &format!("#!/bin/sh\nprintf '%s' '{response}'\n"),
            );
            assert!(
                upsert_ruleset(
                    &project.context(),
                    &repository,
                    &branch_ruleset("main", 1, &[])
                )
                .unwrap_err()
                .to_string()
                .contains(expected)
            );
        }
    }

    #[test]
    fn builds_setup_plan_and_preserves_requested_settings() {
        let repository = Repository {
            owner: "socketry".to_owned(),
            name: "example".to_owned(),
        };
        let reviewers = ReviewerSet {
            configured: vec!["socketry/managers".to_owned()],
            resolved: vec!["Team:123".to_owned()],
        };
        let plan = setup_plan(
            &repository,
            "main",
            2,
            &["test".to_owned()],
            &reviewers,
            Some(10),
            "crates-io",
        );

        assert_eq!(plan["repository"], "socketry/example");
        assert_eq!(plan["reviewers"]["configured"][0], "socketry/managers");
        assert_eq!(plan["environment"]["name"], "crates-io");
        assert_eq!(plan["environment"]["wait_timer"], 10);
        assert_eq!(
            plan["branch_ruleset"]["rules"][0]["parameters"]["required_approving_review_count"],
            2
        );
    }

    #[test]
    fn applies_rulesets_and_a_new_environment_without_hitting_github() {
        let mut environment = Environment::new();
        let project = Project::new();
        project.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/alpha\"]\nresolver = \"3\"\n",
        );
        project.write(
            "crates/alpha/Cargo.toml",
            "[package]\nname = \"alpha\"\nversion = \"1.2.3\"\nedition = \"2024\"\n",
        );
        project.write("crates/alpha/src/lib.rs", "// fixture\n");
        with_fake_gh(
            &project,
            &mut environment,
            "#!/bin/sh\ncase \"$3 $4\" in\n  'GET repos/socketry/example/environments?per_page=100') printf '%s' '{\"environments\":[]}' ;;\n  'GET repos/socketry/example/rulesets?per_page=100') printf '%s' '[]' ;;\n  'POST repos/socketry/example/rulesets') cat >/dev/null; printf '%s' '{\"created\":true}' ;;\n  'PUT repos/socketry/example/environments/crates-io') cat > requested-environment.json; printf '%s' '{\"name\":\"crates-io\"}' ;;\n  *) echo unexpected-request >&2; exit 1 ;;\nesac\n",
        );
        let repository = Repository {
            owner: "socketry".to_owned(),
            name: "example".to_owned(),
        };
        let result = apply_setup(
            &project.context(),
            &repository,
            "main",
            1,
            &["check".to_owned()],
            &["User:123".to_owned()],
            None,
            "crates-io",
        )
        .unwrap();
        assert_eq!(result["branch_ruleset"]["created"], true);
        assert_eq!(result["tag_ruleset"]["created"], true);
        let environment_body: JsonValue = serde_json::from_slice(
            &std::fs::read(project.root().join("requested-environment.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(environment_body["reviewers"][0]["id"], 123);
    }

    #[test]
    fn apply_setup_reports_failures_from_each_github_stage() {
        let mut environment = Environment::new();
        let project = Project::new();
        project.single_package("fixture", "1.2.3");
        project.cargo_proxy(&mut environment, None);
        let gets_file = project.root().join("ruleset-gets");
        let script = r#"#!/bin/sh
case "$3 $4" in
  'GET repos/socketry/example/environments?per_page=100')
    if [ "$BAKE_TEST_SETUP_FAILURE" = environments ]; then echo 'injected failure' >&2; exit 1; fi
    printf '%s' '{"environments":[]}' ;;
  'GET repos/socketry/example/rulesets?per_page=100')
    count=$(cat "$BAKE_TEST_RULESET_GETS" 2>/dev/null || echo 0)
    count=$((count + 1))
    printf '%s' "$count" > "$BAKE_TEST_RULESET_GETS"
    if [ "$count" = "$BAKE_TEST_SETUP_FAILURE" ]; then echo 'injected failure' >&2; exit 1; fi
    printf '%s' '[]' ;;
  'POST repos/socketry/example/rulesets') cat >/dev/null; printf '%s' '{}' ;;
  'PUT repos/socketry/example/environments/crates-io')
    cat >/dev/null
    if [ "$BAKE_TEST_SETUP_FAILURE" = environment-update ]; then echo 'injected failure' >&2; exit 1; fi
    printf '%s' '{}' ;;
  *) echo unexpected-request >&2; exit 1 ;;
esac
"#;
        with_fake_gh(&project, &mut environment, script);
        environment.set("BAKE_TEST_RULESET_GETS", gets_file.as_os_str());
        let repository = Repository {
            owner: "socketry".to_owned(),
            name: "example".to_owned(),
        };

        for failure in ["environments", "1", "2", "environment-update"] {
            let _ = std::fs::remove_file(&gets_file);
            environment.set("BAKE_TEST_SETUP_FAILURE", failure);
            assert!(
                apply_setup(
                    &project.context(),
                    &repository,
                    "main",
                    1,
                    &["check".to_owned()],
                    &["User:123".to_owned()],
                    None,
                    "crates-io",
                )
                .unwrap_err()
                .to_string()
                .contains("injected failure"),
                "failure stage {failure}"
            );
        }

        let failed_cargo = project.executable(
            "cargo-failed",
            "#!/bin/sh\necho metadata denied >&2; exit 1\n",
        );
        environment.set("CARGO", failed_cargo.as_os_str());
        assert!(
            apply_setup(
                &project.context(),
                &repository,
                "main",
                1,
                &["check".to_owned()],
                &["User:123".to_owned()],
                None,
                "crates-io",
            )
            .unwrap_err()
            .to_string()
            .contains("metadata denied")
        );
    }

    #[test]
    fn updates_existing_rulesets_and_preserves_environment_protection() {
        let mut environment = Environment::new();
        let project = Project::new();
        project.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/alpha\"]\nresolver = \"3\"\n",
        );
        project.write(
            "crates/alpha/Cargo.toml",
            "[package]\nname = \"alpha\"\nversion = \"1.2.3\"\nedition = \"2024\"\n",
        );
        project.write("crates/alpha/src/lib.rs", "// fixture\n");
        with_fake_gh(
            &project,
            &mut environment,
            "#!/bin/sh\ncase \"$3 $4\" in\n  'GET repos/socketry/example/environments?per_page=100') printf '%s' '{\"environments\":[{\"name\":\"crates-io\"}]}' ;;\n  'GET repos/socketry/example/environments/crates-io') printf '%s' '{\"protection_rules\":[{\"type\":\"wait_timer\",\"wait_timer\":5},{\"type\":\"required_reviewers\",\"prevent_self_review\":true,\"reviewers\":[{\"type\":\"User\",\"reviewer\":{\"id\":77}}]}],\"deployment_branch_policy\":{\"protected_branches\":true}}' ;;\n  'GET repos/socketry/example/rulesets?per_page=100') printf '%s' '[{\"name\":\"Socketry Cargo checks\",\"id\":42},{\"name\":\"Socketry Cargo release tags\",\"id\":43}]' ;;\n  'PUT repos/socketry/example/rulesets/42'|'PUT repos/socketry/example/rulesets/43') cat >/dev/null; printf '%s' '{\"updated\":true}' ;;\n  'PUT repos/socketry/example/environments/crates-io') cat > requested-environment.json; printf '%s' '{}' ;;\n  *) echo unexpected-request >&2; exit 1 ;;\nesac\n",
        );
        let repository = Repository {
            owner: "socketry".to_owned(),
            name: "example".to_owned(),
        };
        apply_setup(
            &project.context(),
            &repository,
            "main",
            1,
            &["check".to_owned()],
            &[],
            None,
            "crates-io",
        )
        .unwrap();
        let environment_body: JsonValue = serde_json::from_slice(
            &std::fs::read(project.root().join("requested-environment.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(environment_body["wait_timer"], 5);
        assert_eq!(environment_body["prevent_self_review"], true);
        assert_eq!(environment_body["reviewers"][0]["id"], 77);
        assert_eq!(
            environment_body["deployment_branch_policy"]["protected_branches"],
            true
        );
    }

    #[test]
    fn refuses_to_protect_a_workspace_with_no_publishable_packages() {
        let _cargo_environment = Environment::new();
        let project = Project::new();
        project.write(
            "Cargo.toml",
            "[workspace]\nmembers = []\nresolver = \"3\"\n",
        );
        let repository = Repository {
            owner: "socketry".to_owned(),
            name: "example".to_owned(),
        };

        assert!(
            apply_setup(
                &project.context(),
                &repository,
                "main",
                1,
                &["check".to_owned()],
                &["User:123".to_owned()],
                None,
                "crates-io",
            )
            .unwrap_err()
            .to_string()
            .contains("no publishable packages")
        );
    }

    #[test]
    fn plans_ruleset_and_tag_protection_shapes() {
        let branch = branch_ruleset("main", 0, &["test".to_owned()]);
        assert_eq!(
            branch["conditions"]["ref_name"]["include"][0],
            "refs/heads/main"
        );
        assert_eq!(
            branch["rules"][1]["parameters"]["required_status_checks"][0]["context"],
            "test"
        );
        assert_eq!(
            branch["bypass_actors"],
            json!([{
                "actor_id": 5,
                "actor_type": "RepositoryRole",
                "bypass_mode": "pull_request",
            }])
        );
        assert_eq!(
            tag_ruleset()["conditions"]["ref_name"]["include"][0],
            "refs/tags/v*"
        );
    }
}
