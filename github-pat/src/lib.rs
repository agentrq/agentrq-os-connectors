//! GitHub connector (token auth): repositories, issues, pull requests, files
//! and Actions through the GitHub REST API. Sign in by pasting a fine-grained
//! personal access token; the host adds `Authorization: Bearer …` after each
//! request leaves the guest: this code never sees it.
//!
//! Tool names and parameters follow the GitHub REST API; every call returns
//! the API's JSON as structured content.

use connectors_sdk::http::{Method, Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::Deserialize;
use serde_json::{Value, json};

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://api.github.com";

/// The connector.
#[derive(Debug)]
pub struct GithubPat;

#[derive(Debug, Deserialize)]
struct RepoRef {
    owner: String,
    repo: String,
}

#[derive(Debug, Deserialize)]
struct ListRepos {
    #[serde(rename = "type")]
    repo_type: Option<String>,
    sort: Option<String>,
    direction: Option<String>,
    per_page: Option<u32>,
    page: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct CreateRepo {
    name: String,
    description: Option<String>,
    private: Option<bool>,
    auto_init: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ListBranches {
    owner: String,
    repo: String,
    per_page: Option<u32>,
    page: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ListCommits {
    owner: String,
    repo: String,
    sha: Option<String>,
    path: Option<String>,
    per_page: Option<u32>,
    page: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct GetCommit {
    owner: String,
    repo: String,
    #[serde(rename = "ref")]
    ref_: String,
}

#[derive(Debug, Deserialize)]
struct GetContents {
    owner: String,
    repo: String,
    path: String,
    #[serde(rename = "ref")]
    ref_: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateOrUpdateFile {
    owner: String,
    repo: String,
    path: String,
    message: String,
    content: String,
    sha: Option<String>,
    branch: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeleteFile {
    owner: String,
    repo: String,
    path: String,
    message: String,
    sha: String,
    branch: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListIssues {
    owner: String,
    repo: String,
    state: Option<String>,
    labels: Option<String>,
    per_page: Option<u32>,
    page: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct IssueRef {
    owner: String,
    repo: String,
    issue_number: i64,
}

#[derive(Debug, Deserialize)]
struct CreateIssue {
    owner: String,
    repo: String,
    title: String,
    body: Option<String>,
    labels: Option<Vec<String>>,
    assignees: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct UpdateIssue {
    owner: String,
    repo: String,
    issue_number: i64,
    title: Option<String>,
    body: Option<String>,
    state: Option<String>,
    labels: Option<Vec<String>>,
    assignees: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct CreateIssueComment {
    owner: String,
    repo: String,
    issue_number: i64,
    body: String,
}

#[derive(Debug, Deserialize)]
struct ListPulls {
    owner: String,
    repo: String,
    state: Option<String>,
    head: Option<String>,
    base: Option<String>,
    per_page: Option<u32>,
    page: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct PullRef {
    owner: String,
    repo: String,
    pull_number: i64,
}

#[derive(Debug, Deserialize)]
struct CreatePull {
    owner: String,
    repo: String,
    title: String,
    head: String,
    base: String,
    body: Option<String>,
    draft: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct MergePull {
    owner: String,
    repo: String,
    pull_number: i64,
    commit_title: Option<String>,
    commit_message: Option<String>,
    merge_method: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListWorkflowRuns {
    owner: String,
    repo: String,
    status: Option<String>,
    per_page: Option<u32>,
    page: Option<u32>,
}

impl Connector for GithubPat {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        let base = host::config("api_base").unwrap_or_else(|| DEFAULT_API_BASE.into());
        answer(&outgoing(&base, &request)?.send()?)
    }
}

connectors_sdk::export_connector!(GithubPat);

/// The request a tool call makes to the API at `base`.
fn outgoing(base: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let api = base.trim_end_matches('/');
    let outgoing = match request.tool.as_str() {
        "getRepo" => {
            let input: RepoRef = request.json()?;
            Request::get(format!(
                "{api}/repos/{}/{}",
                encode(&input.owner),
                encode(&input.repo)
            ))
        }
        "listRepos" => {
            let input: ListRepos = request.json()?;
            Request::get(with_query(
                format!("{api}/user/repos"),
                &[
                    ("type", input.repo_type),
                    ("sort", input.sort),
                    ("direction", input.direction),
                    ("per_page", input.per_page.map(|n| n.to_string())),
                    ("page", input.page.map(|n| n.to_string())),
                ],
            ))
        }
        "createRepo" => {
            let input: CreateRepo = request.json()?;
            let body = object(&[
                ("name", Some(json!(input.name))),
                (
                    "description",
                    input
                        .description
                        .as_ref()
                        .map(|description| json!(description)),
                ),
                ("private", input.private.map(|private| json!(private))),
                (
                    "auto_init",
                    input.auto_init.map(|auto_init| json!(auto_init)),
                ),
            ]);
            Request::post(format!("{api}/user/repos")).json(&body)?
        }
        "listBranches" => {
            let input: ListBranches = request.json()?;
            Request::get(with_query(
                format!(
                    "{api}/repos/{}/{}",
                    encode(&input.owner),
                    encode(&input.repo)
                ) + "/branches",
                &[
                    ("per_page", input.per_page.map(|n| n.to_string())),
                    ("page", input.page.map(|n| n.to_string())),
                ],
            ))
        }
        "listCommits" => {
            let input: ListCommits = request.json()?;
            Request::get(with_query(
                format!(
                    "{api}/repos/{}/{}",
                    encode(&input.owner),
                    encode(&input.repo)
                ) + "/commits",
                &[
                    ("sha", input.sha),
                    ("path", input.path),
                    ("per_page", input.per_page.map(|n| n.to_string())),
                    ("page", input.page.map(|n| n.to_string())),
                ],
            ))
        }
        "getCommit" => {
            let input: GetCommit = request.json()?;
            Request::get(format!(
                "{api}/repos/{}/{}/commits/{}",
                encode(&input.owner),
                encode(&input.repo),
                encode(&input.ref_)
            ))
        }
        "getContents" => {
            let input: GetContents = request.json()?;
            Request::get(with_query(
                format!(
                    "{api}/repos/{}/{}/contents/{}",
                    encode(&input.owner),
                    encode(&input.repo),
                    encode_path(&input.path)
                ),
                &[("ref", input.ref_)],
            ))
        }
        "createOrUpdateFile" => {
            let input: CreateOrUpdateFile = request.json()?;
            let body = object(&[
                ("message", Some(json!(input.message))),
                ("content", Some(json!(input.content))),
                ("sha", input.sha.as_ref().map(|sha| json!(sha))),
                ("branch", input.branch.as_ref().map(|branch| json!(branch))),
            ]);
            Request::new(
                Method::Put,
                format!(
                    "{api}/repos/{}/{}/contents/{}",
                    encode(&input.owner),
                    encode(&input.repo),
                    encode_path(&input.path)
                ),
            )
            .json(&body)?
        }
        "deleteFile" => {
            let input: DeleteFile = request.json()?;
            let body = object(&[
                ("message", Some(json!(input.message))),
                ("sha", Some(json!(input.sha))),
                ("branch", input.branch.as_ref().map(|branch| json!(branch))),
            ]);
            Request::new(
                Method::Delete,
                format!(
                    "{api}/repos/{}/{}/contents/{}",
                    encode(&input.owner),
                    encode(&input.repo),
                    encode_path(&input.path)
                ),
            )
            .json(&body)?
        }
        "listIssues" => {
            let input: ListIssues = request.json()?;
            Request::get(with_query(
                format!(
                    "{api}/repos/{}/{}",
                    encode(&input.owner),
                    encode(&input.repo)
                ) + "/issues",
                &[
                    ("state", input.state),
                    ("labels", input.labels),
                    ("per_page", input.per_page.map(|n| n.to_string())),
                    ("page", input.page.map(|n| n.to_string())),
                ],
            ))
        }
        "getIssue" => {
            let input: IssueRef = request.json()?;
            Request::get(format!(
                "{api}/repos/{}/{}/issues/{}",
                encode(&input.owner),
                encode(&input.repo),
                input.issue_number
            ))
        }
        "createIssue" => {
            let input: CreateIssue = request.json()?;
            let body = object(&[
                ("title", Some(json!(input.title))),
                ("body", input.body.as_ref().map(|body| json!(body))),
                ("labels", input.labels.as_ref().map(|labels| json!(labels))),
                (
                    "assignees",
                    input.assignees.as_ref().map(|assignees| json!(assignees)),
                ),
            ]);
            Request::post(format!(
                "{api}/repos/{}/{}/issues",
                encode(&input.owner),
                encode(&input.repo)
            ))
            .json(&body)?
        }
        "updateIssue" => {
            let input: UpdateIssue = request.json()?;
            let body = object(&[
                ("title", input.title.as_ref().map(|title| json!(title))),
                ("body", input.body.as_ref().map(|body| json!(body))),
                ("state", input.state.as_ref().map(|state| json!(state))),
                ("labels", input.labels.as_ref().map(|labels| json!(labels))),
                (
                    "assignees",
                    input.assignees.as_ref().map(|assignees| json!(assignees)),
                ),
            ]);
            Request::new(
                Method::Patch,
                format!(
                    "{api}/repos/{}/{}/issues/{}",
                    encode(&input.owner),
                    encode(&input.repo),
                    input.issue_number
                ),
            )
            .json(&body)?
        }
        "createIssueComment" => {
            let input: CreateIssueComment = request.json()?;
            let body = object(&[("body", Some(json!(input.body)))]);
            Request::post(format!(
                "{api}/repos/{}/{}/issues/{}/comments",
                encode(&input.owner),
                encode(&input.repo),
                input.issue_number
            ))
            .json(&body)?
        }
        "listPulls" => {
            let input: ListPulls = request.json()?;
            Request::get(with_query(
                format!(
                    "{api}/repos/{}/{}",
                    encode(&input.owner),
                    encode(&input.repo)
                ) + "/pulls",
                &[
                    ("state", input.state),
                    ("head", input.head),
                    ("base", input.base),
                    ("per_page", input.per_page.map(|n| n.to_string())),
                    ("page", input.page.map(|n| n.to_string())),
                ],
            ))
        }
        "getPull" => {
            let input: PullRef = request.json()?;
            Request::get(format!(
                "{api}/repos/{}/{}/pulls/{}",
                encode(&input.owner),
                encode(&input.repo),
                input.pull_number
            ))
        }
        "createPull" => {
            let input: CreatePull = request.json()?;
            let body = object(&[
                ("title", Some(json!(input.title))),
                ("head", Some(json!(input.head))),
                ("base", Some(json!(input.base))),
                ("body", input.body.as_ref().map(|body| json!(body))),
                ("draft", input.draft.map(|draft| json!(draft))),
            ]);
            Request::post(format!(
                "{api}/repos/{}/{}/pulls",
                encode(&input.owner),
                encode(&input.repo)
            ))
            .json(&body)?
        }
        "mergePull" => {
            let input: MergePull = request.json()?;
            let body = object(&[
                (
                    "commit_title",
                    input.commit_title.as_ref().map(|title| json!(title)),
                ),
                (
                    "commit_message",
                    input.commit_message.as_ref().map(|message| json!(message)),
                ),
                (
                    "merge_method",
                    input.merge_method.as_ref().map(|method| json!(method)),
                ),
            ]);
            Request::new(
                Method::Put,
                format!(
                    "{api}/repos/{}/{}/pulls/{}/merge",
                    encode(&input.owner),
                    encode(&input.repo),
                    input.pull_number
                ),
            )
            .json(&body)?
        }
        "getAuthenticatedUser" => Request::get(format!("{api}/user")),
        "listWorkflowRuns" => {
            let input: ListWorkflowRuns = request.json()?;
            Request::get(with_query(
                format!(
                    "{api}/repos/{}/{}",
                    encode(&input.owner),
                    encode(&input.repo)
                ) + "/actions/runs",
                &[
                    ("status", input.status),
                    ("per_page", input.per_page.map(|n| n.to_string())),
                    ("page", input.page.map(|n| n.to_string())),
                ],
            ))
        }
        other => return Err(ConnectorError::NotFound(format!("no tool {other}"))),
    };
    // GitHub requires a User-Agent; the API version header pins the behavior.
    Ok(outgoing
        .header("accept", "application/vnd.github+json")
        .header(
            "user-agent",
            concat!("agentrq-github-connector/", env!("CARGO_PKG_VERSION")),
        )
        .header("x-github-api-version", "2022-11-28"))
}

/// Appends the `Some` query parameters to `url`, percent-encoding each value.
fn with_query(url: String, params: &[(&str, Option<String>)]) -> String {
    let query: Vec<String> = params
        .iter()
        .filter_map(|(name, value)| {
            value
                .as_deref()
                .map(|value| format!("{name}={}", encode(value)))
        })
        .collect();
    if query.is_empty() {
        url
    } else {
        format!("{url}?{}", query.join("&"))
    }
}

/// Builds a JSON object from the `Some` entries, skipping the `None`s.
fn object(fields: &[(&str, Option<Value>)]) -> Value {
    let mut map = serde_json::Map::with_capacity(fields.len());
    for (name, value) in fields {
        if let Some(value) = value {
            map.insert((*name).to_string(), value.clone());
        }
    }
    Value::Object(map)
}

/// The tool result of the API's response: its JSON, or a tool error that
/// says what the API answered, so the model can react (a 404 usually means
/// a wrong owner/repo, a 403 a missing scope or rate limiting).
fn answer(response: &Response) -> Result<ToolResponse, ConnectorError> {
    if !(200..300).contains(&response.status) {
        let body = String::from_utf8_lossy(&response.body);
        return Ok(ToolResponse::error(format!(
            "The API answered {}: {body}",
            response.status
        )));
    }
    let body: Value = response.json()?;
    ToolResponse::structured(&body)
}

/// Percent-encodes a path segment or query value.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// Percent-encodes a repository content path, keeping `/` as the separator.
fn encode_path(path: &str) -> String {
    path.split('/').map(encode).collect::<Vec<_>>().join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(tool: &str, body: &str) -> ToolRequest {
        ToolRequest {
            tool: tool.into(),
            body: body.into(),
            ..ToolRequest::default()
        }
    }

    fn planned(tool: &str, body: &str) -> Request {
        outgoing("https://api.github.com/", &request(tool, body)).unwrap()
    }

    fn response(status: u16, body: &str) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    const API: &str = "https://api.github.com";

    fn header(request: &Request, name: &str) -> Option<String> {
        request
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    #[test]
    fn plans_repository_tools() {
        assert_eq!(
            planned("getRepo", r#"{"owner":"octocat","repo":"hello-world"}"#).url,
            format!("{API}/repos/octocat/hello-world")
        );

        let listed = planned(
            "listRepos",
            r#"{"type":"owner","sort":"updated","direction":"asc","per_page":50,"page":2}"#,
        );
        assert_eq!(
            listed.url,
            format!("{API}/user/repos?type=owner&sort=updated&direction=asc&per_page=50&page=2")
        );
        assert_eq!(planned("listRepos", "{}").url, format!("{API}/user/repos"));

        let created = planned(
            "createRepo",
            r#"{"name":"new-repo","description":"A test","private":true,"auto_init":true}"#,
        );
        assert_eq!(created.url, format!("{API}/user/repos"));
        assert_eq!(created.method, Method::Post);
        let body: Value = serde_json::from_slice(&created.body).unwrap();
        assert_eq!(
            body,
            json!({
                "name": "new-repo",
                "description": "A test",
                "private": true,
                "auto_init": true,
            })
        );
        let minimal = planned("createRepo", r#"{"name":"new-repo"}"#);
        let body: Value = serde_json::from_slice(&minimal.body).unwrap();
        assert_eq!(body, json!({ "name": "new-repo" }));

        let branches = planned(
            "listBranches",
            r#"{"owner":"octocat","repo":"hello-world","per_page":30,"page":3}"#,
        );
        assert_eq!(
            branches.url,
            format!("{API}/repos/octocat/hello-world/branches?per_page=30&page=3")
        );
        assert_eq!(
            planned(
                "listBranches",
                r#"{"owner":"octocat","repo":"hello-world"}"#
            )
            .url,
            format!("{API}/repos/octocat/hello-world/branches")
        );

        let commits = planned(
            "listCommits",
            r#"{"owner":"octocat","repo":"hello-world","sha":"main","path":"src/lib.rs","per_page":10,"page":1}"#,
        );
        assert_eq!(
            commits.url,
            format!(
                "{API}/repos/octocat/hello-world/commits?sha=main&path=src%2Flib.rs&per_page=10&page=1"
            )
        );

        assert_eq!(
            planned(
                "getCommit",
                r#"{"owner":"octocat","repo":"hello-world","ref":"feature/cool"}"#
            )
            .url,
            format!("{API}/repos/octocat/hello-world/commits/feature%2Fcool")
        );
    }

    #[test]
    fn plans_content_tools() {
        assert_eq!(
            planned(
                "getContents",
                r#"{"owner":"octocat","repo":"hello-world","path":"src/lib.rs","ref":"main"}"#
            )
            .url,
            format!("{API}/repos/octocat/hello-world/contents/src/lib.rs?ref=main")
        );
        assert_eq!(
            planned(
                "getContents",
                r#"{"owner":"octocat","repo":"hello-world","path":"README.md"}"#
            )
            .url,
            format!("{API}/repos/octocat/hello-world/contents/README.md")
        );

        let written = planned(
            "createOrUpdateFile",
            r#"{"owner":"octocat","repo":"hello-world","path":"docs/a b.md","message":"Add docs","content":"aGVsbG8=","sha":"abc123","branch":"main"}"#,
        );
        assert_eq!(written.method, Method::Put);
        assert_eq!(
            written.url,
            format!("{API}/repos/octocat/hello-world/contents/docs/a%20b.md")
        );
        let body: Value = serde_json::from_slice(&written.body).unwrap();
        assert_eq!(
            body,
            json!({
                "message": "Add docs",
                "content": "aGVsbG8=",
                "sha": "abc123",
                "branch": "main",
            })
        );
        let created = planned(
            "createOrUpdateFile",
            r#"{"owner":"o","repo":"r","path":"f.md","message":"m","content":"Yg=="}"#,
        );
        let body: Value = serde_json::from_slice(&created.body).unwrap();
        assert_eq!(body, json!({ "message": "m", "content": "Yg==" }));

        let deleted = planned(
            "deleteFile",
            r#"{"owner":"octocat","repo":"hello-world","path":"old.md","message":"Remove","sha":"def456","branch":"main"}"#,
        );
        assert_eq!(deleted.method, Method::Delete);
        assert_eq!(
            deleted.url,
            format!("{API}/repos/octocat/hello-world/contents/old.md")
        );
        let body: Value = serde_json::from_slice(&deleted.body).unwrap();
        assert_eq!(
            body,
            json!({ "message": "Remove", "sha": "def456", "branch": "main" })
        );
    }

    #[test]
    fn plans_issue_tools() {
        let listed = planned(
            "listIssues",
            r#"{"owner":"octocat","repo":"hello-world","state":"open","labels":"bug,ui","per_page":25,"page":1}"#,
        );
        assert_eq!(
            listed.url,
            format!(
                "{API}/repos/octocat/hello-world/issues?state=open&labels=bug%2Cui&per_page=25&page=1"
            )
        );

        assert_eq!(
            planned(
                "getIssue",
                r#"{"owner":"octocat","repo":"hello-world","issue_number":42}"#
            )
            .url,
            format!("{API}/repos/octocat/hello-world/issues/42")
        );

        let created = planned(
            "createIssue",
            r#"{"owner":"octocat","repo":"hello-world","title":"Bug","body":"It broke","labels":["bug"],"assignees":["octocat"]}"#,
        );
        assert_eq!(created.method, Method::Post);
        assert_eq!(
            created.url,
            format!("{API}/repos/octocat/hello-world/issues")
        );
        let body: Value = serde_json::from_slice(&created.body).unwrap();
        assert_eq!(
            body,
            json!({
                "title": "Bug",
                "body": "It broke",
                "labels": ["bug"],
                "assignees": ["octocat"],
            })
        );

        let updated = planned(
            "updateIssue",
            r#"{"owner":"octocat","repo":"hello-world","issue_number":7,"state":"closed"}"#,
        );
        assert_eq!(updated.method, Method::Patch);
        assert_eq!(
            updated.url,
            format!("{API}/repos/octocat/hello-world/issues/7")
        );
        let body: Value = serde_json::from_slice(&updated.body).unwrap();
        assert_eq!(body, json!({ "state": "closed" }));

        let fully_updated = planned(
            "updateIssue",
            r#"{"owner":"o","repo":"r","issue_number":8,"title":"T","body":"B","labels":["l"],"assignees":["a"]}"#,
        );
        let body: Value = serde_json::from_slice(&fully_updated.body).unwrap();
        assert_eq!(
            body,
            json!({ "title": "T", "body": "B", "labels": ["l"], "assignees": ["a"] })
        );

        let commented = planned(
            "createIssueComment",
            r#"{"owner":"octocat","repo":"hello-world","issue_number":7,"body":"Nice work"}"#,
        );
        assert_eq!(commented.method, Method::Post);
        assert_eq!(
            commented.url,
            format!("{API}/repos/octocat/hello-world/issues/7/comments")
        );
        let body: Value = serde_json::from_slice(&commented.body).unwrap();
        assert_eq!(body, json!({ "body": "Nice work" }));
    }

    #[test]
    fn plans_pull_and_action_tools() {
        let listed = planned(
            "listPulls",
            r#"{"owner":"octocat","repo":"hello-world","state":"open","head":"feature","base":"main","per_page":20,"page":2}"#,
        );
        assert_eq!(
            listed.url,
            format!(
                "{API}/repos/octocat/hello-world/pulls?state=open&head=feature&base=main&per_page=20&page=2"
            )
        );

        assert_eq!(
            planned(
                "getPull",
                r#"{"owner":"octocat","repo":"hello-world","pull_number":3}"#
            )
            .url,
            format!("{API}/repos/octocat/hello-world/pulls/3")
        );

        let created = planned(
            "createPull",
            r#"{"owner":"o","repo":"r","title":"T","head":"feature","base":"main","body":"B","draft":true}"#,
        );
        assert_eq!(created.method, Method::Post);
        assert_eq!(created.url, format!("{API}/repos/o/r/pulls"));
        let body: Value = serde_json::from_slice(&created.body).unwrap();
        assert_eq!(
            body,
            json!({ "title": "T", "head": "feature", "base": "main", "body": "B", "draft": true })
        );

        let merged = planned(
            "mergePull",
            r#"{"owner":"o","repo":"r","pull_number":9,"commit_title":"T","commit_message":"M","merge_method":"squash"}"#,
        );
        assert_eq!(merged.method, Method::Put);
        assert_eq!(merged.url, format!("{API}/repos/o/r/pulls/9/merge"));
        let body: Value = serde_json::from_slice(&merged.body).unwrap();
        assert_eq!(
            body,
            json!({ "commit_title": "T", "commit_message": "M", "merge_method": "squash" })
        );
        let plain_merge = planned("mergePull", r#"{"owner":"o","repo":"r","pull_number":9}"#);
        let body: Value = serde_json::from_slice(&plain_merge.body).unwrap();
        assert_eq!(body, json!({}));

        assert_eq!(
            planned("getAuthenticatedUser", "{}").url,
            format!("{API}/user")
        );

        let runs = planned(
            "listWorkflowRuns",
            r#"{"owner":"octocat","repo":"hello-world","status":"completed","per_page":5,"page":2}"#,
        );
        assert_eq!(
            runs.url,
            format!(
                "{API}/repos/octocat/hello-world/actions/runs?status=completed&per_page=5&page=2"
            )
        );
    }

    #[test]
    fn sets_github_headers() {
        let request = planned("getRepo", r#"{"owner":"o","repo":"r"}"#);
        assert_eq!(
            header(&request, "accept").as_deref(),
            Some("application/vnd.github+json")
        );
        assert_eq!(
            header(&request, "x-github-api-version").as_deref(),
            Some("2022-11-28")
        );
        let agent = header(&request, "user-agent").unwrap();
        assert!(
            agent.starts_with("agentrq-github-connector/"),
            "unexpected user-agent {agent}"
        );
    }

    #[test]
    fn refuses_bad_input() {
        let missing = outgoing(
            DEFAULT_API_BASE,
            &request("getRepo", r#"{"owner":"octocat"}"#),
        );
        assert!(
            matches!(missing, Err(ConnectorError::InvalidInput(_))),
            "{missing:?}"
        );
        let unknown = outgoing(DEFAULT_API_BASE, &request("nope", "{}"));
        assert_eq!(
            unknown.unwrap_err(),
            ConnectorError::NotFound("no tool nope".into())
        );
    }

    #[test]
    fn maps_responses() {
        let ok = answer(&response(200, r#"{"login":"octocat"}"#)).unwrap();
        assert_eq!(ok.structured.as_deref(), Some(r#"{"login":"octocat"}"#));
        let created = answer(&response(201, r#"{"id":1}"#)).unwrap();
        assert_eq!(created.structured.as_deref(), Some(r#"{"id":1}"#));
        let missing = answer(&response(404, r#"{"message":"Not Found"}"#)).unwrap();
        assert_eq!(
            missing,
            ToolResponse::error(r#"The API answered 404: {"message":"Not Found"}"#)
        );
        let garbled = answer(&response(200, "<html>"));
        assert!(
            matches!(garbled, Err(ConnectorError::Upstream(_))),
            "{garbled:?}"
        );
    }

    #[test]
    fn off_wasm_nothing_is_sent() {
        let sent = GithubPat::call_tool(request(
            "getRepo",
            r#"{"owner":"octocat","repo":"hello-world"}"#,
        ));
        assert!(matches!(&sent, Err(ConnectorError::Upstream(m)) if m.contains("wasm32-wasip2")));
    }

    #[test]
    fn manifest_is_embedded() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["name"], "github-pat");
        assert_eq!(manifest["auth"]["schemes"][0]["type"], "bearer");
        assert_eq!(
            manifest["permissions"]["http"]["allow"][0],
            DEFAULT_API_BASE
        );
        assert_eq!(manifest["tools"].as_array().unwrap().len(), 20);
    }
}
