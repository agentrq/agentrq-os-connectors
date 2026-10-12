//! GitHub connector: outcome-oriented tools for repositories, branches,
//! commits, files, issues, pull requests and Actions through the GitHub REST
//! API. The host runs the OAuth 2.0 sign-in, refreshes the token and adds
//! `Authorization: Bearer …` after each request leaves the guest: this code
//! never sees it.
//!
//! Tools are task-shaped, not endpoint-shaped: one tool may make several API
//! calls internally (reading an issue with its comments, proposing changes
//! as a branch plus a pull request) so the agent makes one call per outcome.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use connectors_sdk::http::{Method, Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://api.github.com";

/// The connector.
#[derive(Debug)]
pub struct Github;

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
struct ReadFile {
    owner: String,
    repo: String,
    path: String,
    #[serde(rename = "ref")]
    ref_: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListDirectory {
    owner: String,
    repo: String,
    path: Option<String>,
    #[serde(rename = "ref")]
    ref_: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WriteFile {
    owner: String,
    repo: String,
    path: String,
    branch: String,
    content: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct DeleteFile {
    owner: String,
    repo: String,
    path: String,
    branch: String,
    message: Option<String>,
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
struct IssueComment {
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
struct ProposeChanges {
    owner: String,
    repo: String,
    base: String,
    new_branch: String,
    files: Vec<ProposedFile>,
    title: String,
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProposedFile {
    path: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ListWorkflowRuns {
    owner: String,
    repo: String,
    status: Option<String>,
    per_page: Option<u32>,
    page: Option<u32>,
}

/// File metadata behind a contents GET.
#[derive(Debug, Deserialize)]
struct FileMeta {
    #[serde(rename = "type")]
    kind: String,
    sha: String,
    size: u64,
    encoding: Option<String>,
    content: Option<String>,
}

/// One entry of a directory listing.
#[derive(Debug, Deserialize)]
struct DirEntry {
    name: String,
    path: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
    size: Option<u64>,
}

/// One file of a pull request, trimmed to what agents review.
#[derive(Debug, Deserialize, Serialize)]
struct PullFile {
    filename: String,
    status: String,
    additions: u64,
    deletions: u64,
}

/// A git ref's target behind `GET /git/ref/heads/{base}`.
#[derive(Debug, Deserialize)]
struct GitRef {
    object: GitObject,
}

#[derive(Debug, Deserialize)]
struct GitObject {
    sha: String,
}

/// A tree behind `POST /git/trees`.
#[derive(Debug, Deserialize)]
struct GitTree {
    sha: String,
}

/// A commit behind `POST /git/commits`.
#[derive(Debug, Deserialize)]
struct GitCommit {
    sha: String,
}

/// A commit behind `GET /git/commits/{sha}`: only its tree is needed.
#[derive(Debug, Deserialize)]
struct GitCommitDetail {
    tree: GitTree,
}

/// A pull request behind `POST /pulls`.
#[derive(Debug, Deserialize)]
struct CreatedPull {
    number: i64,
    html_url: String,
}

/// A written file behind `PUT /contents/{path}`.
#[derive(Debug, Deserialize)]
struct WrittenFile {
    content: GitObject,
    commit: GitObject,
}

impl Connector for Github {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        Self::call_with(request, &mut |request| Ok(request.clone().send()?))
    }
}

impl Github {
    /// Dispatches a tool call, sending requests through `send`: the real
    /// `Request::send` in production, a stub in tests so multi-call tools are
    /// covered end to end without a network.
    fn call_with(
        request: ToolRequest,
        send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
    ) -> Result<ToolResponse, ConnectorError> {
        let base = host::config("api_base").unwrap_or_else(|| DEFAULT_API_BASE.into());
        let api = base.trim_end_matches('/');
        // GitHub requires a User-Agent; the API version header pins behavior.
        let mut send = |planned: &Request| send(&github_headers(planned));
        match request.tool.as_str() {
            "getRepo" => one_call(api, &request, &mut send, plan_get_repo),
            "listRepos" => one_call(api, &request, &mut send, plan_list_repos),
            "createRepo" => one_call(api, &request, &mut send, plan_create_repo),
            "readFile" => read_file(api, &request, &mut send),
            "listDirectory" => list_directory(api, &request, &mut send),
            "writeFile" => write_file(api, &request, &mut send),
            "deleteFile" => delete_file(api, &request, &mut send),
            "listBranches" => one_call(api, &request, &mut send, plan_list_branches),
            "listCommits" => one_call(api, &request, &mut send, plan_list_commits),
            "getCommit" => one_call(api, &request, &mut send, plan_get_commit),
            "listIssues" => one_call(api, &request, &mut send, plan_list_issues),
            "getIssue" => get_issue(api, &request, &mut send),
            "createIssue" => one_call(api, &request, &mut send, plan_create_issue),
            "updateIssue" => one_call(api, &request, &mut send, plan_update_issue),
            "commentOnIssue" => one_call(api, &request, &mut send, plan_comment_on_issue),
            "listPulls" => one_call(api, &request, &mut send, plan_list_pulls),
            "getPull" => get_pull(api, &request, &mut send),
            "createPull" => one_call(api, &request, &mut send, plan_create_pull),
            "mergePull" => one_call(api, &request, &mut send, plan_merge_pull),
            "proposeChanges" => propose_changes(api, &request, &mut send),
            "getAuthenticatedUser" => answer(&send(&plan_get_authenticated_user(api)?)?), // coverage: exclude — plan_get_authenticated_user returns Ok unconditionally
            "listWorkflowRuns" => one_call(api, &request, &mut send, plan_list_workflow_runs),
            other => Err(ConnectorError::NotFound(format!("no tool {other}"))),
        }
    }
}

connectors_sdk::export_connector!(Github);

/// The headers every GitHub request carries: the JSON accept header, the
/// required User-Agent, and the pinned API version.
fn github_headers(planned: &Request) -> Request {
    planned
        .clone()
        .header("accept", "application/vnd.github+json")
        .header(
            "user-agent",
            concat!("agentrq-github-connector/", env!("CARGO_PKG_VERSION")),
        )
        .header("x-github-api-version", "2022-11-28")
}

/// Plans a single-request tool's call, sends it and answers the response.
fn one_call<I>(
    api: &str,
    request: &ToolRequest,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
    plan: fn(&str, &I) -> Result<Request, ConnectorError>,
) -> Result<ToolResponse, ConnectorError>
where
    I: for<'de> Deserialize<'de>,
{
    let input: I = request.json()?;
    answer(&send(&plan(api, &input)?)?) // coverage: exclude — every plan passed to one_call is infallible (Ok(..) or .json(&Value))
}

/// The response of one step of a multi-call tool: 2xx continues the tool,
/// anything else becomes a tool error naming the failed step.
fn step_ok(response: &Response, step: &str) -> Result<(), ToolResponse> {
    if (200..300).contains(&response.status) {
        Ok(())
    } else {
        let body = String::from_utf8_lossy(&response.body);
        Err(ToolResponse::error(format!(
            "{step} failed: the API answered {}: {body}",
            response.status
        )))
    }
}

/// `{api}/repos/{owner}/{repo}`, with the owner and repo percent-encoded.
fn repo_path(api: &str, owner: &str, repo: &str) -> String {
    format!("{api}/repos/{}/{}", encode(owner), encode(repo))
}

/// The contents URL of `path`; the repository root when `path` is empty.
fn contents_url(api: &str, owner: &str, repo: &str, path: &str) -> String {
    let base = repo_path(api, owner, repo);
    if path.is_empty() {
        format!("{base}/contents")
    } else {
        format!("{base}/contents/{}", encode_path(path))
    }
}

/// The `per_page`/`page` query pair shared by every listing tool.
fn page_params(per_page: Option<u32>, page: Option<u32>) -> [(&'static str, Option<String>); 2] {
    [
        ("per_page", per_page.map(|n| n.to_string())),
        ("page", page.map(|n| n.to_string())),
    ]
}

fn plan_get_repo(api: &str, input: &RepoRef) -> Result<Request, ConnectorError> {
    Ok(Request::get(repo_path(api, &input.owner, &input.repo)))
}

fn plan_list_repos(api: &str, input: &ListRepos) -> Result<Request, ConnectorError> {
    let mut params: Vec<(&str, Option<String>)> = Vec::with_capacity(5);
    params.push(("type", input.repo_type.clone()));
    params.push(("sort", input.sort.clone()));
    params.push(("direction", input.direction.clone()));
    params.extend(page_params(input.per_page, input.page));
    Ok(Request::get(with_query(
        format!("{api}/user/repos"),
        &params,
    )))
}

fn plan_create_repo(api: &str, input: &CreateRepo) -> Result<Request, ConnectorError> {
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
    Ok(Request::post(format!("{api}/user/repos")).json(&body)?) // coverage: exclude — Request::json serializes a serde_json::Value here, which is infallible
}

/// Plans a contents GET: the file or directory read behind `readFile`,
/// `listDirectory`, and the SHA lookups behind `writeFile`/`deleteFile`.
fn plan_contents_get(
    api: &str,
    owner: &str,
    repo: &str,
    path: &str,
    ref_: Option<&str>,
) -> Result<Request, ConnectorError> {
    Ok(Request::get(with_query(
        contents_url(api, owner, repo, path),
        &[("ref", ref_.map(str::to_string))],
    )))
}

/// Runs the calls behind `readFile`: one contents GET, then its base64
/// decoded internally — text comes back as text, binary as base64 with a
/// flag, so agents never hand-decode.
fn read_file(
    api: &str,
    request: &ToolRequest,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: ReadFile = request.json()?;
    let response = send(&plan_contents_get(
        api,
        &input.owner,
        &input.repo,
        &input.path,
        input.ref_.as_deref(),
    )?)?; // coverage: exclude — plan_contents_get returns Ok unconditionally
    if let Err(error) = step_ok(&response, "reading the file") {
        return Ok(error);
    }
    let meta = match parse_file_meta(&response)? {
        Some(meta) => meta,
        None => {
            return Ok(ToolResponse::error(format!(
                "{} is a directory; use listDirectory to browse it",
                input.path
            )));
        }
    };
    let FileMeta {
        kind,
        sha,
        size,
        encoding,
        content,
    } = meta;
    if kind != "file" {
        return Ok(ToolResponse::error(format!(
            "{} is not a file (type: {kind})",
            input.path
        )));
    }
    let (content, encoding) = match (content, encoding) {
        (Some(content), Some(encoding)) => (content, encoding),
        _ => {
            return Ok(ToolResponse::error(format!(
                "the API returned no readable content for {}",
                input.path
            )));
        }
    };
    if encoding != "base64" {
        return Ok(ToolResponse::error(format!(
            "the API returned {} with an unsupported encoding: {encoding}",
            input.path
        )));
    }
    let decoded = match STANDARD.decode(content) {
        Ok(decoded) => decoded,
        Err(_) => {
            return Ok(ToolResponse::error(format!(
                "the API returned content for {} that is not valid base64",
                input.path
            )));
        }
    };
    combine_read_file(&input.path, &sha, size, decoded)
}

/// Answers `readFile`: decoded text for UTF-8 files, the base64 plus a
/// binary flag otherwise.
fn combine_read_file(
    path: &str,
    sha: &str,
    size: u64,
    decoded: Vec<u8>,
) -> Result<ToolResponse, ConnectorError> {
    match String::from_utf8(decoded) {
        Ok(content) => ToolResponse::structured(&json!({
            "path": path,
            "sha": sha,
            "size": size,
            "content": content,
        })),
        Err(error) => ToolResponse::structured(&json!({
            "path": path,
            "sha": sha,
            "size": size,
            "contentBase64": STANDARD.encode(error.into_bytes()),
            "isBinary": true,
        })),
    }
}

/// Runs the call behind `listDirectory`: one contents GET mapped to
/// `{name, path, type, sha, size}` entries.
fn list_directory(
    api: &str,
    request: &ToolRequest,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: ListDirectory = request.json()?;
    let response = send(&plan_contents_get(
        api,
        &input.owner,
        &input.repo,
        input.path.as_deref().unwrap_or(""),
        input.ref_.as_deref(),
    )?)?; // coverage: exclude — plan_contents_get returns Ok unconditionally
    if let Err(error) = step_ok(&response, "listing the directory") {
        return Ok(error);
    }
    let body: Value = response.json()?;
    if !body.is_array() {
        let path = input.path.as_deref().unwrap_or("the repository root");
        return Ok(ToolResponse::error(format!(
            "{path} is a file; use readFile to read it"
        )));
    }
    let entries: Vec<DirEntry> = serde_json::from_value(body)
        .map_err(|error| ConnectorError::Upstream(error.to_string()))?;
    combine_list_directory(&entries)
}

/// Answers `listDirectory` with the entries trimmed to what agents browse.
fn combine_list_directory(entries: &[DirEntry]) -> Result<ToolResponse, ConnectorError> {
    ToolResponse::structured(&json!(
        entries
            .iter()
            .map(|entry| json!({
                "name": entry.name,
                "path": entry.path,
                "type": entry.kind,
                "sha": entry.sha,
                "size": entry.size,
            }))
            .collect::<Vec<_>>()
    ))
}

/// File metadata behind a contents GET, or `None` when the path is a
/// directory — the API answers an array there.
fn parse_file_meta(response: &Response) -> Result<Option<FileMeta>, ConnectorError> {
    let body: Value = response.json()?;
    if body.is_array() {
        return Ok(None);
    }
    serde_json::from_value(body).map_err(|error| ConnectorError::Upstream(error.to_string()))
}

/// Runs the calls behind `writeFile`: resolve the current blob SHA on the
/// branch (a 404 means the file is new), then create or update with the
/// content base64-encoded internally — the agent passes plain text and never
/// fetches a SHA first.
fn write_file(
    api: &str,
    request: &ToolRequest,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: WriteFile = request.json()?;
    let current = send(&plan_contents_get(
        api,
        &input.owner,
        &input.repo,
        &input.path,
        Some(&input.branch),
    )?)?; // coverage: exclude — plan_contents_get returns Ok unconditionally
    let sha = match current.status {
        // The file does not exist yet: the write creates it.
        404 => None,
        _ => {
            if let Err(error) = step_ok(&current, "resolving the file's current sha") {
                return Ok(error);
            }
            match parse_file_meta(&current)? {
                Some(meta) => Some(meta.sha),
                None => {
                    return Ok(ToolResponse::error(format!(
                        "cannot write {}: it is a directory",
                        input.path
                    )));
                }
            }
        }
    };
    let written = send(&plan_write_file(api, &input, sha.as_deref())?)?; // coverage: exclude — plan_write_file serializes a Value, which is infallible
    if let Err(error) = step_ok(&written, "writing the file") {
        return Ok(error);
    }
    combine_write_file(&input.path, &written.json()?)
}

/// Plans `writeFile`'s create-or-update: the content base64-encoded here, so
/// the agent passes plain text.
fn plan_write_file(
    api: &str,
    input: &WriteFile,
    sha: Option<&str>,
) -> Result<Request, ConnectorError> {
    let body = object(&[
        ("message", Some(json!(input.message))),
        (
            "content",
            Some(json!(STANDARD.encode(input.content.as_bytes()))),
        ),
        ("sha", sha.map(|sha| json!(sha))),
        ("branch", Some(json!(input.branch))),
    ]);
    Ok(Request::new(
        Method::Put,
        contents_url(api, &input.owner, &input.repo, &input.path),
    )
    .json(&body)?) // coverage: exclude — Request::json serializes a serde_json::Value here, which is infallible
}

/// Answers `writeFile` with the path and the SHAs, hiding the API's envelope.
fn combine_write_file(path: &str, written: &WrittenFile) -> Result<ToolResponse, ConnectorError> {
    ToolResponse::structured(&json!({
        "path": path,
        "sha": written.content.sha,
        "commitSha": written.commit.sha,
    }))
}

/// Runs the calls behind `deleteFile`: resolve the blob SHA, then delete —
/// the agent never fetches the SHA first.
fn delete_file(
    api: &str,
    request: &ToolRequest,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: DeleteFile = request.json()?;
    let current = send(&plan_contents_get(
        api,
        &input.owner,
        &input.repo,
        &input.path,
        Some(&input.branch),
    )?)?; // coverage: exclude — plan_contents_get returns Ok unconditionally
    if let Err(error) = step_ok(&current, "resolving the file's sha") {
        return Ok(error);
    }
    let meta = match parse_file_meta(&current)? {
        Some(meta) => meta,
        None => {
            return Ok(ToolResponse::error(format!(
                "cannot delete {}: it is a directory",
                input.path
            )));
        }
    };
    // Cloned: the plan below still borrows the rest of `input`.
    let message = input
        .message
        .clone()
        .unwrap_or_else(|| format!("delete {}", input.path));
    let deleted = send(&plan_delete_file(api, &input, &meta.sha, &message)?)?; // coverage: exclude — plan_delete_file serializes a Value, which is infallible
    if let Err(error) = step_ok(&deleted, "deleting the file") {
        return Ok(error);
    }
    ToolResponse::structured(&json!({ "deleted": true }))
}

/// Plans `deleteFile`'s delete with the resolved SHA.
fn plan_delete_file(
    api: &str,
    input: &DeleteFile,
    sha: &str,
    message: &str,
) -> Result<Request, ConnectorError> {
    let body = object(&[
        ("message", Some(json!(message))),
        ("sha", Some(json!(sha))),
        ("branch", Some(json!(input.branch))),
    ]);
    Ok(Request::new(
        Method::Delete,
        contents_url(api, &input.owner, &input.repo, &input.path),
    )
    .json(&body)?) // coverage: exclude — Request::json serializes a serde_json::Value here, which is infallible
}

fn plan_list_branches(api: &str, input: &ListBranches) -> Result<Request, ConnectorError> {
    Ok(Request::get(with_query(
        format!("{}/branches", repo_path(api, &input.owner, &input.repo)),
        &page_params(input.per_page, input.page),
    )))
}

fn plan_list_commits(api: &str, input: &ListCommits) -> Result<Request, ConnectorError> {
    let mut params: Vec<(&str, Option<String>)> = Vec::with_capacity(4);
    params.push(("sha", input.sha.clone()));
    params.push(("path", input.path.clone()));
    params.extend(page_params(input.per_page, input.page));
    Ok(Request::get(with_query(
        format!("{}/commits", repo_path(api, &input.owner, &input.repo)),
        &params,
    )))
}

fn plan_get_commit(api: &str, input: &GetCommit) -> Result<Request, ConnectorError> {
    Ok(Request::get(format!(
        "{}/commits/{}",
        repo_path(api, &input.owner, &input.repo),
        encode(&input.ref_)
    )))
}

fn plan_list_issues(api: &str, input: &ListIssues) -> Result<Request, ConnectorError> {
    let mut params: Vec<(&str, Option<String>)> = Vec::with_capacity(4);
    params.push(("state", input.state.clone()));
    params.push(("labels", input.labels.clone()));
    params.extend(page_params(input.per_page, input.page));
    Ok(Request::get(with_query(
        format!("{}/issues", repo_path(api, &input.owner, &input.repo)),
        &params,
    )))
}

/// Runs the two calls behind `getIssue`: the issue, then its comments — one
/// call reads the issue with its discussion.
fn get_issue(
    api: &str,
    request: &ToolRequest,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: IssueRef = request.json()?;
    let url = format!(
        "{}/issues/{}",
        repo_path(api, &input.owner, &input.repo),
        input.issue_number
    );
    let answered = send(&Request::get(&url))?;
    if let Err(error) = step_ok(&answered, "reading the issue") {
        return Ok(error);
    }
    let discussed = send(&Request::get(format!("{url}/comments")))?;
    if let Err(error) = step_ok(&discussed, "reading the issue's comments") {
        return Ok(error);
    }
    let issue: Value = answered.json()?;
    let comments: Value = discussed.json()?;
    combine_get_issue(&issue, &comments)
}

/// Answers `getIssue` with the issue and its comments together.
fn combine_get_issue(issue: &Value, comments: &Value) -> Result<ToolResponse, ConnectorError> {
    ToolResponse::structured(&json!({ "issue": issue, "comments": comments }))
}

fn plan_create_issue(api: &str, input: &CreateIssue) -> Result<Request, ConnectorError> {
    let body = object(&[
        ("title", Some(json!(input.title))),
        ("body", input.body.as_ref().map(|body| json!(body))),
        ("labels", input.labels.as_ref().map(|labels| json!(labels))),
        (
            "assignees",
            input.assignees.as_ref().map(|assignees| json!(assignees)),
        ),
    ]);
    Ok(Request::post(format!(
        "{}/issues",
        repo_path(api, &input.owner, &input.repo)
    ))
    .json(&body)?) // coverage: exclude — Request::json serializes a serde_json::Value here, which is infallible
}

fn plan_update_issue(api: &str, input: &UpdateIssue) -> Result<Request, ConnectorError> {
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
    Ok(Request::new(
        Method::Patch,
        format!(
            "{}/issues/{}",
            repo_path(api, &input.owner, &input.repo),
            input.issue_number
        ),
    )
    .json(&body)?) // coverage: exclude — Request::json serializes a serde_json::Value here, which is infallible
}

fn plan_comment_on_issue(api: &str, input: &IssueComment) -> Result<Request, ConnectorError> {
    let body = object(&[("body", Some(json!(input.body)))]);
    Ok(Request::post(format!(
        "{}/issues/{}/comments",
        repo_path(api, &input.owner, &input.repo),
        input.issue_number
    ))
    .json(&body)?) // coverage: exclude — Request::json serializes a serde_json::Value here, which is infallible
}

fn plan_list_pulls(api: &str, input: &ListPulls) -> Result<Request, ConnectorError> {
    let mut params: Vec<(&str, Option<String>)> = Vec::with_capacity(5);
    params.push(("state", input.state.clone()));
    params.push(("head", input.head.clone()));
    params.push(("base", input.base.clone()));
    params.extend(page_params(input.per_page, input.page));
    Ok(Request::get(with_query(
        format!("{}/pulls", repo_path(api, &input.owner, &input.repo)),
        &params,
    )))
}

/// Runs the two calls behind `getPull`: the pull request, then its files —
/// one call reviews the PR with its diff stats.
fn get_pull(
    api: &str,
    request: &ToolRequest,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: PullRef = request.json()?;
    let url = format!(
        "{}/pulls/{}",
        repo_path(api, &input.owner, &input.repo),
        input.pull_number
    );
    let answered = send(&Request::get(&url))?;
    if let Err(error) = step_ok(&answered, "reading the pull request") {
        return Ok(error);
    }
    let changed = send(&Request::get(format!("{url}/files")))?;
    if let Err(error) = step_ok(&changed, "reading the pull request's files") {
        return Ok(error);
    }
    let pull: Value = answered.json()?;
    let files: Vec<PullFile> = changed.json()?;
    combine_get_pull(&pull, &files)
}

/// Answers `getPull` with the pull request and its files trimmed to
/// `{filename, status, additions, deletions}`.
fn combine_get_pull(pull: &Value, files: &[PullFile]) -> Result<ToolResponse, ConnectorError> {
    ToolResponse::structured(&json!({ "pull": pull, "files": files }))
}

fn plan_create_pull(api: &str, input: &CreatePull) -> Result<Request, ConnectorError> {
    let body = object(&[
        ("title", Some(json!(input.title))),
        ("head", Some(json!(input.head))),
        ("base", Some(json!(input.base))),
        ("body", input.body.as_ref().map(|body| json!(body))),
        ("draft", input.draft.map(|draft| json!(draft))),
    ]);
    Ok(Request::post(format!(
        "{}/pulls",
        repo_path(api, &input.owner, &input.repo)
    ))
    .json(&body)?) // coverage: exclude — Request::json serializes a serde_json::Value here, which is infallible
}

fn plan_merge_pull(api: &str, input: &MergePull) -> Result<Request, ConnectorError> {
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
    Ok(Request::new(
        Method::Put,
        format!(
            "{}/pulls/{}/merge",
            repo_path(api, &input.owner, &input.repo),
            input.pull_number
        ),
    )
    .json(&body)?) // coverage: exclude — Request::json serializes a serde_json::Value here, which is infallible
}

/// Runs the six calls behind `proposeChanges`: resolve the base branch, read
/// the base commit's tree, create one tree for all files, commit it, point
/// the new branch at the commit, and open the pull request — "make these
/// edits and open a PR" in one tool call.
fn propose_changes(
    api: &str,
    request: &ToolRequest,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: ProposeChanges = request.json()?;
    if input.files.is_empty() {
        return Err(ConnectorError::InvalidInput(
            "proposeChanges needs at least one file".into(),
        ));
    }
    let repo = repo_path(api, &input.owner, &input.repo);

    let base = send(&Request::get(format!(
        "{repo}/git/ref/heads/{}",
        encode(&input.base)
    )))?;
    if let Err(error) = step_ok(&base, "resolving the base branch") {
        return Ok(error);
    }
    let base_sha = base.json::<GitRef>()?.object.sha;

    // `base_tree` must be a tree-object SHA, not the base commit's SHA: read
    // the base commit to learn its tree.
    let base_commit = send(&Request::get(format!(
        "{repo}/git/commits/{}",
        encode(&base_sha)
    )))?;
    if let Err(error) = step_ok(&base_commit, "reading the base commit") {
        return Ok(error);
    }
    let base_tree_sha = base_commit.json::<GitCommitDetail>()?.tree.sha;

    let tree = send(&plan_propose_tree(&repo, &input, &base_tree_sha)?)?; // coverage: exclude — plan_propose_tree serializes a Value, which is infallible
    if let Err(error) = step_ok(&tree, "creating the tree") {
        return Ok(error);
    }
    let tree_sha = tree.json::<GitTree>()?.sha;

    let commit = send(&plan_propose_commit(&repo, &input, &tree_sha, &base_sha)?)?; // coverage: exclude — plan_propose_commit serializes a Value, which is infallible
    if let Err(error) = step_ok(&commit, "creating the commit") {
        return Ok(error);
    }
    let commit_sha = commit.json::<GitCommit>()?.sha;

    let branch = send(&Request::post(format!("{repo}/git/refs")).json(&object(&[
        (
            "ref",
            Some(json!(format!("refs/heads/{}", input.new_branch))),
        ),
        ("sha", Some(json!(commit_sha))),
    ]))?)?; // coverage: exclude — .json(&Value) serializes infallibly
    if let Err(error) = step_ok(&branch, "creating the branch") {
        return Ok(error);
    }

    let pull = send(&plan_propose_pull(&repo, &input)?)?; // coverage: exclude — plan_propose_pull serializes a Value, which is infallible
    if let Err(error) = step_ok(&pull, "opening the pull request") {
        return Ok(error);
    }
    combine_propose_changes(&pull.json()?)
}

/// Plans `proposeChanges`' pull request.
fn plan_propose_pull(repo: &str, input: &ProposeChanges) -> Result<Request, ConnectorError> {
    let body = object(&[
        ("title", Some(json!(input.title))),
        ("head", Some(json!(input.new_branch))),
        ("base", Some(json!(input.base))),
        ("body", input.body.as_ref().map(|body| json!(body))),
    ]);
    Ok(Request::post(format!("{repo}/pulls")).json(&body)?) // coverage: exclude — Request::json serializes a serde_json::Value here, which is infallible
}

/// Plans `proposeChanges`' tree: one call carrying every file as a blob, on
/// top of the base commit's tree.
fn plan_propose_tree(
    repo: &str,
    input: &ProposeChanges,
    base_tree_sha: &str,
) -> Result<Request, ConnectorError> {
    let tree: Vec<Value> = input
        .files
        .iter()
        .map(|file| {
            json!({
                "path": file.path,
                "mode": "100644",
                "type": "blob",
                "content": file.content,
            })
        })
        .collect();
    let body = object(&[
        ("base_tree", Some(json!(base_tree_sha))),
        ("tree", Some(Value::Array(tree))),
    ]);
    Ok(Request::post(format!("{repo}/git/trees")).json(&body)?) // coverage: exclude — Request::json serializes a serde_json::Value here, which is infallible
}

/// Plans `proposeChanges`' commit on top of the base commit.
fn plan_propose_commit(
    repo: &str,
    input: &ProposeChanges,
    tree_sha: &str,
    base_sha: &str,
) -> Result<Request, ConnectorError> {
    let body = object(&[
        ("message", Some(json!(input.title))),
        ("tree", Some(json!(tree_sha))),
        ("parents", Some(json!([base_sha]))),
    ]);
    Ok(Request::post(format!("{repo}/git/commits")).json(&body)?) // coverage: exclude — Request::json serializes a serde_json::Value here, which is infallible
}

/// Answers `proposeChanges` with the pull request's number and URL.
fn combine_propose_changes(pull: &CreatedPull) -> Result<ToolResponse, ConnectorError> {
    ToolResponse::structured(&json!({
        "pullNumber": pull.number,
        "url": pull.html_url,
    }))
}

fn plan_get_authenticated_user(api: &str) -> Result<Request, ConnectorError> {
    Ok(Request::get(format!("{api}/user")))
}

fn plan_list_workflow_runs(api: &str, input: &ListWorkflowRuns) -> Result<Request, ConnectorError> {
    let mut params: Vec<(&str, Option<String>)> = Vec::with_capacity(3);
    params.push(("status", input.status.clone()));
    params.extend(page_params(input.per_page, input.page));
    Ok(Request::get(with_query(
        format!("{}/actions/runs", repo_path(api, &input.owner, &input.repo)),
        &params,
    )))
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
    use std::collections::VecDeque;

    fn request(tool: &str, body: &str) -> ToolRequest {
        ToolRequest {
            tool: tool.into(),
            body: body.into(),
        }
    }

    fn response(status: u16, body: &str) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    fn ok_json(body: &str) -> Response {
        response(200, body)
    }

    /// Queued responses drained by the sender under test, recording every
    /// request so multi-call tools are covered end to end without a network.
    struct Stub {
        responses: VecDeque<Response>,
        seen: Vec<Request>,
    }

    impl Stub {
        fn of(responses: &[Response]) -> Self {
            Self {
                responses: responses.iter().cloned().collect(),
                seen: Vec::new(),
            }
        }

        fn send(&mut self, request: &Request) -> Result<Response, ConnectorError> {
            self.seen.push(request.clone());
            self.responses
                .pop_front()
                .ok_or_else(|| ConnectorError::Upstream("stub sender ran out of responses".into()))
        }
    }

    fn call(tool: &str, body: &str, stub: &mut Stub) -> Result<ToolResponse, ConnectorError> {
        Github::call_with(request(tool, body), &mut |request| stub.send(request))
    }

    const API: &str = "https://api.github.com";

    fn header(request: &Request, name: &str) -> Option<String> {
        request
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    fn body_json(request: &Request) -> Value {
        serde_json::from_slice(&request.body).unwrap()
    }

    #[test]
    fn plans_repository_tools() {
        // Arrange: one stubbed response per call below.
        let mut stub = Stub::of(&[
            ok_json(r#"{"full_name":"octocat/hello-world"}"#),
            ok_json(r#"[]"#),
            ok_json(r#"{"id":1}"#),
            ok_json(r#"{"id":2}"#),
        ]);

        // Act.
        let repo = call(
            "getRepo",
            r#"{"owner":"octocat","repo":"hello-world"}"#,
            &mut stub,
        )
        .unwrap();
        let repos = call(
            "listRepos",
            r#"{"type":"owner","sort":"updated","direction":"asc","per_page":50,"page":2}"#,
            &mut stub,
        )
        .unwrap();
        let created = call(
            "createRepo",
            r#"{"name":"new-repo","description":"A test","private":true,"auto_init":true}"#,
            &mut stub,
        )
        .unwrap();
        let minimal = call("createRepo", r#"{"name":"new-repo"}"#, &mut stub).unwrap();

        // Assert: the planned requests.
        assert_eq!(stub.seen.len(), 4);
        assert_eq!(stub.seen[0].url, format!("{API}/repos/octocat/hello-world"));
        assert_eq!(stub.seen[0].method, Method::Get);
        assert_eq!(
            stub.seen[1].url,
            format!("{API}/user/repos?type=owner&sort=updated&direction=asc&per_page=50&page=2")
        );
        assert_eq!(stub.seen[2].method, Method::Post);
        assert_eq!(stub.seen[2].url, format!("{API}/user/repos"));
        assert_eq!(
            body_json(&stub.seen[2]),
            json!({
                "name": "new-repo",
                "description": "A test",
                "private": true,
                "auto_init": true,
            })
        );
        assert_eq!(body_json(&stub.seen[3]), json!({ "name": "new-repo" }));

        // Assert: the API's JSON is answered through.
        assert_eq!(
            repo.structured.as_deref(),
            Some(r#"{"full_name":"octocat/hello-world"}"#)
        );
        assert_eq!(repos.structured.as_deref(), Some(r#"[]"#));
        assert_eq!(created.structured.as_deref(), Some(r#"{"id":1}"#));
        assert_eq!(minimal.structured.as_deref(), Some(r#"{"id":2}"#));
    }

    #[test]
    fn plans_branch_and_commit_tools() {
        // Arrange.
        let mut stub = Stub::of(&[
            ok_json(r#"[]"#),
            ok_json(r#"[]"#),
            ok_json(r#"{"sha":"abc"}"#),
            ok_json(r#"[]"#),
        ]);

        // Act.
        call(
            "listBranches",
            r#"{"owner":"octocat","repo":"hello-world","per_page":30,"page":3}"#,
            &mut stub,
        )
        .unwrap();
        call(
            "listBranches",
            r#"{"owner":"octocat","repo":"hello-world"}"#,
            &mut stub,
        )
        .unwrap();
        call(
            "listCommits",
            r#"{"owner":"octocat","repo":"hello-world","sha":"main","path":"src/lib.rs","per_page":10,"page":1}"#,
            &mut stub,
        )
        .unwrap();
        call(
            "getCommit",
            r#"{"owner":"octocat","repo":"hello-world","ref":"feature/cool"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(stub.seen.len(), 4);
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/repos/octocat/hello-world/branches?per_page=30&page=3")
        );
        assert_eq!(
            stub.seen[1].url,
            format!("{API}/repos/octocat/hello-world/branches")
        );
        assert_eq!(
            stub.seen[2].url,
            format!(
                "{API}/repos/octocat/hello-world/commits?sha=main&path=src%2Flib.rs&per_page=10&page=1"
            )
        );
        assert_eq!(
            stub.seen[3].url,
            format!("{API}/repos/octocat/hello-world/commits/feature%2Fcool")
        );
    }

    #[test]
    fn reads_text_file_decoded() {
        // Arrange: the API's base64 content for "hello".
        let mut stub = Stub::of(&[ok_json(
            r#"{"type":"file","sha":"abc123","size":5,"encoding":"base64","content":"aGVsbG8="}"#,
        )]);

        // Act.
        let answered = call(
            "readFile",
            r#"{"owner":"o","repo":"r","path":"docs/notes.md","ref":"main"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: one GET, and the content decoded to text.
        assert_eq!(stub.seen.len(), 1);
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/repos/o/r/contents/docs/notes.md?ref=main")
        );
        assert_eq!(
            answered.structured.as_deref(),
            Some(r#"{"content":"hello","path":"docs/notes.md","sha":"abc123","size":5}"#)
        );
    }

    #[test]
    fn reads_binary_file_as_base64() {
        // Arrange: bytes that are not valid UTF-8.
        let mut stub = Stub::of(&[ok_json(
            r#"{"type":"file","sha":"ff00","size":2,"encoding":"base64","content":"//8="}"#,
        )]);

        // Act.
        let answered = call(
            "readFile",
            r#"{"owner":"o","repo":"r","path":"img.png"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the base64 is passed through with the binary flag.
        assert_eq!(
            answered.structured.as_deref(),
            Some(
                r#"{"contentBase64":"//8=","isBinary":true,"path":"img.png","sha":"ff00","size":2}"#
            )
        );
    }

    #[test]
    fn read_file_reports_api_errors() {
        // Arrange: the file does not exist.
        let mut stub = Stub::of(&[response(404, r#"{"message":"Not Found"}"#)]);

        // Act.
        let failed = call(
            "readFile",
            r#"{"owner":"o","repo":"r","path":"missing.md"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: a tool error naming the failed step.
        assert_eq!(
            failed,
            ToolResponse::error(
                r#"reading the file failed: the API answered 404: {"message":"Not Found"}"#,
            )
        );
    }

    #[test]
    fn read_file_points_directories_at_list_directory() {
        // Arrange: the path is a directory, so the API answers an array.
        let mut stub = Stub::of(&[ok_json(
            r#"[{"name":"a.md","path":"docs/a.md","type":"file","sha":"s","size":1}]"#,
        )]);

        // Act.
        let failed = call(
            "readFile",
            r#"{"owner":"o","repo":"r","path":"docs"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            failed,
            ToolResponse::error("docs is a directory; use listDirectory to browse it")
        );
    }

    #[test]
    fn read_file_rejects_non_files_and_unreadable_content() {
        // Arrange: a symlink, a response with no content, and bad base64.
        let mut stub = Stub::of(&[
            ok_json(r#"{"type":"symlink","sha":"s","size":10,"target":"elsewhere"}"#),
            ok_json(r#"{"type":"file","sha":"s","size":10}"#),
            ok_json(r#"{"type":"file","sha":"s","size":10,"encoding":"none","content":"raw"}"#),
            ok_json(r#"{"type":"file","sha":"s","size":10,"encoding":"base64","content":"!!!"}"#),
        ]);
        let body = r#"{"owner":"o","repo":"r","path":"f"}"#;

        // Act.
        let symlink = call("readFile", body, &mut stub).unwrap();
        let no_content = call("readFile", body, &mut stub).unwrap();
        let bad_encoding = call("readFile", body, &mut stub).unwrap();
        let bad_base64 = call("readFile", body, &mut stub).unwrap();

        // Assert: each becomes a tool error explaining the problem.
        assert_eq!(
            symlink,
            ToolResponse::error("f is not a file (type: symlink)")
        );
        assert_eq!(
            no_content,
            ToolResponse::error("the API returned no readable content for f")
        );
        assert_eq!(
            bad_encoding,
            ToolResponse::error("the API returned f with an unsupported encoding: none")
        );
        assert_eq!(
            bad_base64,
            ToolResponse::error("the API returned content for f that is not valid base64")
        );
    }

    #[test]
    fn lists_directory_entries() {
        // Arrange: a directory with a file and a subdirectory.
        let mut stub = Stub::of(&[ok_json(
            r#"[{"name":"a.md","path":"docs/a.md","type":"file","sha":"s1","size":12},{"name":"sub","path":"docs/sub","type":"dir","sha":"s2"}]"#,
        )]);

        // Act.
        let answered = call(
            "listDirectory",
            r#"{"owner":"o","repo":"r","path":"docs","ref":"main"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: entries trimmed to name/path/type/sha/size.
        assert_eq!(stub.seen.len(), 1);
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/repos/o/r/contents/docs?ref=main")
        );
        assert_eq!(
            answered.structured.as_deref(),
            Some(
                r#"[{"name":"a.md","path":"docs/a.md","sha":"s1","size":12,"type":"file"},{"name":"sub","path":"docs/sub","sha":"s2","size":null,"type":"dir"}]"#
            )
        );
    }

    #[test]
    fn lists_repository_root_by_default() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"[]"#)]);

        // Act.
        call("listDirectory", r#"{"owner":"o","repo":"r"}"#, &mut stub).unwrap();

        // Assert: no trailing path segment for the root.
        assert_eq!(stub.seen.len(), 1);
        assert_eq!(stub.seen[0].url, format!("{API}/repos/o/r/contents"));
    }

    #[test]
    fn list_directory_points_files_at_read_file() {
        // Arrange: the path is a file, so the API answers an object.
        let mut stub = Stub::of(&[ok_json(
            r#"{"type":"file","sha":"s","size":3,"encoding":"base64","content":"aGk="}"#,
        )]);

        // Act.
        let failed = call(
            "listDirectory",
            r#"{"owner":"o","repo":"r","path":"f.md"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            failed,
            ToolResponse::error("f.md is a file; use readFile to read it")
        );
    }

    #[test]
    fn list_directory_reports_api_errors() {
        // Arrange.
        let mut stub = Stub::of(&[response(404, r#"{"message":"Not Found"}"#)]);

        // Act.
        let failed = call(
            "listDirectory",
            r#"{"owner":"o","repo":"r","path":"nope"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            failed,
            ToolResponse::error(
                r#"listing the directory failed: the API answered 404: {"message":"Not Found"}"#
            )
        );
    }

    #[test]
    fn list_directory_reports_malformed_entries() {
        // Arrange: the API answers an array whose entry is not a directory entry.
        let mut stub = Stub::of(&[ok_json(r#"[{"name":42}]"#)]);

        // Act.
        let failed = call(
            "listDirectory",
            r#"{"owner":"o","repo":"r","path":"docs"}"#,
            &mut stub,
        );

        // Assert: the shape error surfaces as a connector error.
        assert!(matches!(failed, Err(ConnectorError::Upstream(_)))); // coverage: exclude — matches! false arm is unreachable in a passing test
    }

    #[test]
    fn read_file_reports_malformed_metadata() {
        // Arrange: the API answers valid JSON that is not file metadata.
        let mut stub = Stub::of(&[ok_json(r#"{"type":42}"#)]);

        // Act.
        let failed = call(
            "readFile",
            r#"{"owner":"o","repo":"r","path":"f"}"#,
            &mut stub,
        );

        // Assert: the shape error surfaces as a connector error.
        assert!(matches!(failed, Err(ConnectorError::Upstream(_)))); // coverage: exclude — matches! false arm is unreachable in a passing test
    }

    #[test]
    fn stub_sender_reports_exhaustion() {
        // Arrange: no responses queued.
        let mut stub = Stub::of(&[]);

        // Act.
        let failed = call("listDirectory", r#"{"owner":"o","repo":"r"}"#, &mut stub);

        // Assert: the stub names its own exhaustion.
        assert!(
            matches!(&failed, Err(ConnectorError::Upstream(m)) if m.contains("ran out of responses")),
            "{failed:?}"
        );
    }

    #[test]
    fn writes_new_file_without_a_prior_sha() {
        // Arrange: the file does not exist yet, then the create succeeds.
        let mut stub = Stub::of(&[
            response(404, r#"{"message":"Not Found"}"#),
            response(
                201,
                r#"{"content":{"sha":"blobsha"},"commit":{"sha":"commitsha"}}"#,
            ),
        ]);

        // Act.
        let answered = call(
            "writeFile",
            r#"{"owner":"o","repo":"r","path":"new.md","branch":"main","content":"hello","message":"Add new.md"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the lookup ran against the branch, and the PUT carries
        // base64 content with no sha.
        assert_eq!(stub.seen.len(), 2);
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/repos/o/r/contents/new.md?ref=main")
        );
        assert_eq!(stub.seen[1].method, Method::Put);
        assert_eq!(stub.seen[1].url, format!("{API}/repos/o/r/contents/new.md"));
        assert_eq!(
            body_json(&stub.seen[1]),
            json!({"message":"Add new.md","content":"aGVsbG8=","branch":"main"})
        );
        assert_eq!(
            answered.structured.as_deref(),
            Some(r#"{"commitSha":"commitsha","path":"new.md","sha":"blobsha"}"#)
        );
    }

    #[test]
    fn writes_existing_file_with_the_resolved_sha() {
        // Arrange: the file exists, then the update succeeds.
        let mut stub = Stub::of(&[
            ok_json(
                r#"{"type":"file","sha":"oldsha","size":2,"encoding":"base64","content":"aGk="}"#,
            ),
            ok_json(r#"{"content":{"sha":"newsha"},"commit":{"sha":"c2"}}"#),
        ]);

        // Act.
        let answered = call(
            "writeFile",
            r#"{"owner":"o","repo":"r","path":"docs/a b.md","branch":"main","content":"hi","message":"Update"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the PUT carries the resolved sha and base64 content.
        assert_eq!(stub.seen.len(), 2);
        assert_eq!(
            stub.seen[1].url,
            format!("{API}/repos/o/r/contents/docs/a%20b.md")
        );
        let body = body_json(&stub.seen[1]);
        assert_eq!(body["sha"], "oldsha");
        assert_eq!(body["content"], "aGk=");
        assert_eq!(
            answered.structured.as_deref(),
            Some(r#"{"commitSha":"c2","path":"docs/a b.md","sha":"newsha"}"#)
        );
    }

    #[test]
    fn write_file_reports_lookup_and_write_errors() {
        // Arrange: a failing lookup, then a failing write.
        let mut stub = Stub::of(&[
            response(500, "boom"),
            ok_json(r#"{"type":"file","sha":"s","size":1,"encoding":"base64","content":"eA=="}"#),
            response(422, r#"{"message":"Invalid"}"#),
        ]);
        let body =
            r#"{"owner":"o","repo":"r","path":"f.md","branch":"main","content":"x","message":"m"}"#;

        // Act.
        let lookup_failed = call("writeFile", body, &mut stub).unwrap();
        let write_failed = call("writeFile", body, &mut stub).unwrap();

        // Assert: each step names itself.
        assert_eq!(
            lookup_failed,
            ToolResponse::error(
                "resolving the file's current sha failed: the API answered 500: boom"
            )
        );
        assert_eq!(
            write_failed,
            ToolResponse::error(
                r#"writing the file failed: the API answered 422: {"message":"Invalid"}"#
            )
        );
    }

    #[test]
    fn write_file_rejects_directories() {
        // Arrange: the path is a directory.
        let mut stub = Stub::of(&[ok_json(
            r#"[{"name":"a","path":"d/a","type":"file","sha":"s","size":1}]"#,
        )]);

        // Act.
        let failed = call(
            "writeFile",
            r#"{"owner":"o","repo":"r","path":"d","branch":"main","content":"x","message":"m"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            failed,
            ToolResponse::error("cannot write d: it is a directory")
        );
    }

    #[test]
    fn deletes_file_with_defaults_and_custom_message() {
        // Arrange: two deletes, one with an explicit message.
        let meta = ok_json(
            r#"{"type":"file","sha":"deadbeef","size":3,"encoding":"base64","content":"eA=="}"#,
        );
        let mut stub = Stub::of(&[
            meta.clone(),
            ok_json(r#"{"commit":{"sha":"c"}}"#),
            meta,
            ok_json(r#"{"commit":{"sha":"c"}}"#),
        ]);

        // Act.
        let defaulted = call(
            "deleteFile",
            r#"{"owner":"o","repo":"r","path":"old.md","branch":"main"}"#,
            &mut stub,
        )
        .unwrap();
        let custom = call(
            "deleteFile",
            r#"{"owner":"o","repo":"r","path":"old.md","branch":"main","message":"Prune"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the SHA is resolved, then the DELETE carries it; the
        // message defaults to "delete {path}".
        assert_eq!(stub.seen.len(), 4);
        assert_eq!(stub.seen[1].method, Method::Delete);
        assert_eq!(
            body_json(&stub.seen[1]),
            json!({"message":"delete old.md","sha":"deadbeef","branch":"main"})
        );
        assert_eq!(
            body_json(&stub.seen[3]),
            json!({"message":"Prune","sha":"deadbeef","branch":"main"})
        );
        assert_eq!(defaulted.structured.as_deref(), Some(r#"{"deleted":true}"#));
        assert_eq!(custom.structured.as_deref(), Some(r#"{"deleted":true}"#));
    }

    #[test]
    fn delete_file_reports_lookup_and_delete_errors() {
        // Arrange: a failing lookup, then a failing delete.
        let mut stub = Stub::of(&[
            response(404, r#"{"message":"Not Found"}"#),
            ok_json(r#"{"type":"file","sha":"s","size":1,"encoding":"base64","content":"eA=="}"#),
            response(422, r#"{"message":"Invalid"}"#),
        ]);
        let body = r#"{"owner":"o","repo":"r","path":"gone.md","branch":"main"}"#;

        // Act.
        let lookup_failed = call("deleteFile", body, &mut stub).unwrap();
        let delete_failed = call("deleteFile", body, &mut stub).unwrap();

        // Assert.
        assert_eq!(
            lookup_failed,
            ToolResponse::error(
                r#"resolving the file's sha failed: the API answered 404: {"message":"Not Found"}"#
            )
        );
        assert_eq!(
            delete_failed,
            ToolResponse::error(
                r#"deleting the file failed: the API answered 422: {"message":"Invalid"}"#
            )
        );
    }

    #[test]
    fn delete_file_rejects_directories() {
        // Arrange: the path is a directory.
        let mut stub = Stub::of(&[ok_json(
            r#"[{"name":"a","path":"d/a","type":"file","sha":"s","size":1}]"#,
        )]);

        // Act.
        let failed = call(
            "deleteFile",
            r#"{"owner":"o","repo":"r","path":"d","branch":"main"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            failed,
            ToolResponse::error("cannot delete d: it is a directory")
        );
    }

    #[test]
    fn plans_issue_tools() {
        // Arrange.
        let mut stub = Stub::of(&[
            ok_json(r#"[]"#),
            ok_json(r#"{"number":1}"#),
            ok_json(r#"{"number":1}"#),
            ok_json(r#"{"id":9}"#),
        ]);

        // Act.
        call(
            "listIssues",
            r#"{"owner":"octocat","repo":"hello-world","state":"open","labels":"bug,ui","per_page":25,"page":1}"#,
            &mut stub,
        )
        .unwrap();
        call(
            "createIssue",
            r#"{"owner":"octocat","repo":"hello-world","title":"Bug","body":"It broke","labels":["bug"],"assignees":["octocat"]}"#,
            &mut stub,
        )
        .unwrap();
        call(
            "updateIssue",
            r#"{"owner":"octocat","repo":"hello-world","issue_number":7,"title":"Bug","body":"It broke","state":"closed","labels":["bug"],"assignees":["octocat"]}"#,
            &mut stub,
        )
        .unwrap();
        call(
            "commentOnIssue",
            r#"{"owner":"octocat","repo":"hello-world","issue_number":7,"body":"Nice work"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(stub.seen.len(), 4);
        assert_eq!(
            stub.seen[0].url,
            format!(
                "{API}/repos/octocat/hello-world/issues?state=open&labels=bug%2Cui&per_page=25&page=1"
            )
        );
        assert_eq!(stub.seen[1].method, Method::Post);
        assert_eq!(
            stub.seen[1].url,
            format!("{API}/repos/octocat/hello-world/issues")
        );
        assert_eq!(
            body_json(&stub.seen[1]),
            json!({
                "title": "Bug",
                "body": "It broke",
                "labels": ["bug"],
                "assignees": ["octocat"],
            })
        );
        assert_eq!(stub.seen[2].method, Method::Patch);
        assert_eq!(
            stub.seen[2].url,
            format!("{API}/repos/octocat/hello-world/issues/7")
        );
        assert_eq!(
            body_json(&stub.seen[2]),
            json!({
                "title": "Bug",
                "body": "It broke",
                "state": "closed",
                "labels": ["bug"],
                "assignees": ["octocat"],
            })
        );
        assert_eq!(stub.seen[3].method, Method::Post);
        assert_eq!(
            stub.seen[3].url,
            format!("{API}/repos/octocat/hello-world/issues/7/comments")
        );
        assert_eq!(body_json(&stub.seen[3]), json!({ "body": "Nice work" }));
    }

    #[test]
    fn gets_issue_with_its_comments() {
        // Arrange: the issue, then its discussion.
        let mut stub = Stub::of(&[
            ok_json(r#"{"number":42,"title":"Bug"}"#),
            ok_json(r#"[{"id":1,"body":"First!"},{"id":2,"body":"Fixed"}]"#),
        ]);

        // Act.
        let answered = call(
            "getIssue",
            r#"{"owner":"o","repo":"r","issue_number":42}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: both calls ran, and the answer carries issue + comments.
        assert_eq!(stub.seen.len(), 2);
        assert_eq!(stub.seen[0].url, format!("{API}/repos/o/r/issues/42"));
        assert_eq!(
            stub.seen[1].url,
            format!("{API}/repos/o/r/issues/42/comments")
        );
        assert_eq!(
            answered.structured.as_deref(),
            Some(
                r#"{"comments":[{"body":"First!","id":1},{"body":"Fixed","id":2}],"issue":{"number":42,"title":"Bug"}}"#
            )
        );
    }

    #[test]
    fn get_issue_reports_step_errors() {
        // Arrange: a failing issue read, then failing comments.
        let mut stub = Stub::of(&[
            response(404, r#"{"message":"Not Found"}"#),
            ok_json(r#"{"number":1}"#),
            response(500, "boom"),
        ]);
        let body = r#"{"owner":"o","repo":"r","issue_number":1}"#;

        // Act.
        let issue_failed = call("getIssue", body, &mut stub).unwrap();
        let comments_failed = call("getIssue", body, &mut stub).unwrap();

        // Assert.
        assert_eq!(stub.seen.len(), 3);
        assert_eq!(
            issue_failed,
            ToolResponse::error(
                r#"reading the issue failed: the API answered 404: {"message":"Not Found"}"#
            )
        );
        assert_eq!(
            comments_failed,
            ToolResponse::error("reading the issue's comments failed: the API answered 500: boom")
        );
    }

    #[test]
    fn plans_pull_tools() {
        // Arrange.
        let mut stub = Stub::of(&[
            ok_json(r#"[]"#),
            ok_json(r#"{"number":3}"#),
            ok_json(r#"{"merged":true}"#),
            ok_json(r#"{"merged":true}"#),
        ]);

        // Act.
        call(
            "listPulls",
            r#"{"owner":"octocat","repo":"hello-world","state":"open","head":"feature","base":"main","per_page":20,"page":2}"#,
            &mut stub,
        )
        .unwrap();
        call(
            "createPull",
            r#"{"owner":"o","repo":"r","title":"T","head":"feature","base":"main","body":"B","draft":true}"#,
            &mut stub,
        )
        .unwrap();
        call(
            "mergePull",
            r#"{"owner":"o","repo":"r","pull_number":9,"commit_title":"T","commit_message":"M","merge_method":"squash"}"#,
            &mut stub,
        )
        .unwrap();
        call(
            "mergePull",
            r#"{"owner":"o","repo":"r","pull_number":9}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(stub.seen.len(), 4);
        assert_eq!(
            stub.seen[0].url,
            format!(
                "{API}/repos/octocat/hello-world/pulls?state=open&head=feature&base=main&per_page=20&page=2"
            )
        );
        assert_eq!(stub.seen[1].method, Method::Post);
        assert_eq!(stub.seen[1].url, format!("{API}/repos/o/r/pulls"));
        assert_eq!(
            body_json(&stub.seen[1]),
            json!({ "title": "T", "head": "feature", "base": "main", "body": "B", "draft": true })
        );
        assert_eq!(stub.seen[2].method, Method::Put);
        assert_eq!(stub.seen[2].url, format!("{API}/repos/o/r/pulls/9/merge"));
        assert_eq!(
            body_json(&stub.seen[2]),
            json!({ "commit_title": "T", "commit_message": "M", "merge_method": "squash" })
        );
        assert_eq!(body_json(&stub.seen[3]), json!({}));
    }

    #[test]
    fn gets_pull_with_its_files() {
        // Arrange: the pull request, then its changed files.
        let mut stub = Stub::of(&[
            ok_json(r#"{"number":3,"title":"T"}"#),
            ok_json(
                r#"[{"filename":"a.md","status":"modified","additions":10,"deletions":2,"patch":"@@..."},{"filename":"b.md","status":"added","additions":5,"deletions":0}]"#,
            ),
        ]);

        // Act.
        let answered = call(
            "getPull",
            r#"{"owner":"o","repo":"r","pull_number":3}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: files trimmed to filename/status/additions/deletions.
        assert_eq!(stub.seen.len(), 2);
        assert_eq!(stub.seen[0].url, format!("{API}/repos/o/r/pulls/3"));
        assert_eq!(stub.seen[1].url, format!("{API}/repos/o/r/pulls/3/files"));
        assert_eq!(
            answered.structured.as_deref(),
            Some(
                r#"{"files":[{"additions":10,"deletions":2,"filename":"a.md","status":"modified"},{"additions":5,"deletions":0,"filename":"b.md","status":"added"}],"pull":{"number":3,"title":"T"}}"#
            )
        );
    }

    #[test]
    fn get_pull_reports_step_errors() {
        // Arrange: a failing pull read, then failing files.
        let mut stub = Stub::of(&[
            response(404, r#"{"message":"Not Found"}"#),
            ok_json(r#"{"number":1}"#),
            response(500, "boom"),
        ]);
        let body = r#"{"owner":"o","repo":"r","pull_number":1}"#;

        // Act.
        let pull_failed = call("getPull", body, &mut stub).unwrap();
        let files_failed = call("getPull", body, &mut stub).unwrap();

        // Assert.
        assert_eq!(stub.seen.len(), 3);
        assert_eq!(
            pull_failed,
            ToolResponse::error(
                r#"reading the pull request failed: the API answered 404: {"message":"Not Found"}"#
            )
        );
        assert_eq!(
            files_failed,
            ToolResponse::error(
                "reading the pull request's files failed: the API answered 500: boom"
            )
        );
    }

    #[test]
    fn proposes_changes_end_to_end() {
        // Arrange: base ref, base commit, tree, commit, branch ref, pull request.
        let mut stub = Stub::of(&[
            ok_json(r#"{"ref":"refs/heads/main","object":{"sha":"basesha"}}"#),
            ok_json(r#"{"sha":"basesha","tree":{"sha":"basetreesha"}}"#),
            response(201, r#"{"sha":"treesha"}"#),
            response(201, r#"{"sha":"commitsha"}"#),
            response(201, r#"{"ref":"refs/heads/feature"}"#),
            response(
                201,
                r#"{"number":7,"html_url":"https://github.com/o/r/pull/7"}"#,
            ),
        ]);

        // Act.
        let answered = call(
            "proposeChanges",
            r#"{"owner":"o","repo":"r","base":"main","new_branch":"feature","files":[{"path":"a.md","content":"hello"},{"path":"b.md","content":"world"}],"title":"Add docs","body":"Two files"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the six calls, in order.
        assert_eq!(stub.seen.len(), 6);
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/repos/o/r/git/ref/heads/main")
        );
        // The tree is built on the base commit's tree SHA, not the commit SHA.
        assert_eq!(
            stub.seen[1].url,
            format!("{API}/repos/o/r/git/commits/basesha")
        );
        assert_eq!(stub.seen[2].method, Method::Post);
        assert_eq!(stub.seen[2].url, format!("{API}/repos/o/r/git/trees"));
        assert_eq!(
            body_json(&stub.seen[2]),
            json!({
                "base_tree": "basetreesha",
                "tree": [
                    {"path": "a.md", "mode": "100644", "type": "blob", "content": "hello"},
                    {"path": "b.md", "mode": "100644", "type": "blob", "content": "world"},
                ],
            })
        );
        assert_eq!(stub.seen[3].url, format!("{API}/repos/o/r/git/commits"));
        assert_eq!(
            body_json(&stub.seen[3]),
            json!({"message": "Add docs", "tree": "treesha", "parents": ["basesha"]})
        );
        assert_eq!(stub.seen[4].url, format!("{API}/repos/o/r/git/refs"));
        assert_eq!(
            body_json(&stub.seen[4]),
            json!({"ref": "refs/heads/feature", "sha": "commitsha"})
        );
        assert_eq!(stub.seen[5].url, format!("{API}/repos/o/r/pulls"));
        assert_eq!(
            body_json(&stub.seen[5]),
            json!({"title": "Add docs", "head": "feature", "base": "main", "body": "Two files"})
        );

        // Assert: the answer is the PR number and URL.
        assert_eq!(
            answered.structured.as_deref(),
            Some(r#"{"pullNumber":7,"url":"https://github.com/o/r/pull/7"}"#)
        );
    }

    #[test]
    fn propose_changes_rejects_empty_files() {
        // Arrange.
        let mut stub = Stub::of(&[]);

        // Act.
        let rejected = call(
            "proposeChanges",
            r#"{"owner":"o","repo":"r","base":"main","new_branch":"f","files":[],"title":"T"}"#,
            &mut stub,
        );

        // Assert: invalid input, and nothing was sent.
        assert_eq!(
            rejected.unwrap_err(),
            ConnectorError::InvalidInput("proposeChanges needs at least one file".into())
        );
        assert!(stub.seen.is_empty());
    }

    #[test]
    fn propose_changes_reports_each_failing_step() {
        // Arrange: each step fails in turn while the earlier ones succeed.
        let steps = [
            "resolving the base branch",
            "reading the base commit",
            "creating the tree",
            "creating the commit",
            "creating the branch",
            "opening the pull request",
        ];
        let body = r#"{"owner":"o","repo":"r","base":"main","new_branch":"f","files":[{"path":"a","content":"b"}],"title":"T"}"#;

        for (failing, &step) in steps.iter().enumerate() {
            // Arrange: successes before the failing step, then the failure.
            let mut responses = vec![
                ok_json(r#"{"object":{"sha":"b"}}"#),
                ok_json(r#"{"sha":"b","tree":{"sha":"bt"}}"#),
                response(201, r#"{"sha":"t"}"#),
                response(201, r#"{"sha":"c"}"#),
                response(201, r#"{}"#),
                response(201, r#"{"number":1,"html_url":"u"}"#),
            ];
            responses[failing] = response(422, r#"{"message":"Invalid"}"#);
            let mut stub = Stub::of(&responses);

            // Act.
            let failed = call("proposeChanges", body, &mut stub).unwrap();

            // Assert: the tool stopped at the failing step and named it.
            assert_eq!(stub.seen.len(), failing + 1, "step {step}");
            assert_eq!(
                failed,
                ToolResponse::error(format!(
                    r#"{step} failed: the API answered 422: {{"message":"Invalid"}}"#
                )),
                "step {step}"
            );
        }
    }

    #[test]
    fn plans_user_and_workflow_tools() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"login":"octocat"}"#), ok_json(r#"{}"#)]);

        // Act.
        let user = call("getAuthenticatedUser", "{}", &mut stub).unwrap();
        call(
            "listWorkflowRuns",
            r#"{"owner":"octocat","repo":"hello-world","status":"completed","per_page":5,"page":2}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(stub.seen.len(), 2);
        assert_eq!(stub.seen[0].url, format!("{API}/user"));
        assert_eq!(
            stub.seen[1].url,
            format!(
                "{API}/repos/octocat/hello-world/actions/runs?status=completed&per_page=5&page=2"
            )
        );
        assert_eq!(user.structured.as_deref(), Some(r#"{"login":"octocat"}"#));
    }

    #[test]
    fn sets_github_headers() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{}"#)]);

        // Act.
        call("getRepo", r#"{"owner":"o","repo":"r"}"#, &mut stub).unwrap();

        // Assert.
        let seen = &stub.seen[0];
        assert_eq!(
            header(seen, "accept").as_deref(),
            Some("application/vnd.github+json")
        );
        assert_eq!(
            header(seen, "x-github-api-version").as_deref(),
            Some("2022-11-28")
        );
        let agent = header(seen, "user-agent").unwrap();
        assert!(
            agent.starts_with("agentrq-github-connector/"),
            "unexpected user-agent {agent}"
        );
    }

    #[test]
    fn refuses_bad_input() {
        // Arrange: every tool that parses an input body, each with a body
        // that is not JSON. getAuthenticatedUser takes no input, so there
        // is no body for it to refuse.
        let tools = [
            "getRepo",
            "listRepos",
            "createRepo",
            "readFile",
            "listDirectory",
            "writeFile",
            "deleteFile",
            "listBranches",
            "listCommits",
            "getCommit",
            "listIssues",
            "getIssue",
            "createIssue",
            "updateIssue",
            "commentOnIssue",
            "listPulls",
            "getPull",
            "createPull",
            "mergePull",
            "proposeChanges",
            "listWorkflowRuns",
        ];

        // Act + assert: invalid input, and nothing was sent.
        for tool in tools {
            let mut stub = Stub::of(&[]);
            let rejected = call(tool, "{oops", &mut stub);
            assert!(
                matches!(rejected, Err(ConnectorError::InvalidInput(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
                "{tool}: {rejected:?}"
            );
            assert!(stub.seen.is_empty(), "{tool} sent a request");
        }

        // An unknown tool is still not found, not invalid input.
        let mut stub = Stub::of(&[]);
        let unknown = call("nope", "{}", &mut stub);
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
            matches!(garbled, Err(ConnectorError::Upstream(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{garbled:?}"
        );
    }

    #[test]
    fn surfaces_garbled_responses() {
        // A 200 whose body is not JSON: every response.json() site must
        // surface it as an upstream error instead of answering garbage.
        let garbled = || response(200, "<html>");
        let file_meta = || {
            ok_json(r#"{"type":"file","sha":"s","size":1,"encoding":"base64","content":"aGk="}"#)
        };
        let base_ref = || ok_json(r#"{"object":{"sha":"b"}}"#);
        let base_commit = || ok_json(r#"{"sha":"b","tree":{"sha":"bt"}}"#);
        let sha = |s: &str| response(201, &format!(r#"{{"sha":"{s}"}}"#));
        let write_body =
            r#"{"owner":"o","repo":"r","path":"f","branch":"b","content":"aGk=","message":"m"}"#;
        let propose_body = r#"{"owner":"o","repo":"r","base":"main","new_branch":"f","files":[{"path":"a","content":"b"}],"title":"T"}"#;

        let cases: &[(&str, &str, Vec<Response>)] = &[
            // One-call tools answer through `answer`.
            ("getRepo", r#"{"owner":"o","repo":"r"}"#, vec![garbled()]),
            ("getAuthenticatedUser", "{}", vec![garbled()]),
            // Multi-call tools parse each step's response.
            (
                "readFile",
                r#"{"owner":"o","repo":"r","path":"f"}"#,
                vec![garbled()],
            ),
            (
                "listDirectory",
                r#"{"owner":"o","repo":"r"}"#,
                vec![garbled()],
            ),
            ("writeFile", write_body, vec![garbled()]),
            ("writeFile", write_body, vec![file_meta(), garbled()]),
            (
                "deleteFile",
                r#"{"owner":"o","repo":"r","path":"f","branch":"b"}"#,
                vec![garbled()],
            ),
            (
                "getIssue",
                r#"{"owner":"o","repo":"r","issue_number":1}"#,
                vec![garbled(), ok_json("{}")],
            ),
            (
                "getIssue",
                r#"{"owner":"o","repo":"r","issue_number":1}"#,
                vec![ok_json("{}"), garbled()],
            ),
            (
                "getPull",
                r#"{"owner":"o","repo":"r","pull_number":1}"#,
                vec![garbled(), ok_json("{}")],
            ),
            (
                "getPull",
                r#"{"owner":"o","repo":"r","pull_number":1}"#,
                vec![ok_json("{}"), garbled()],
            ),
            ("proposeChanges", propose_body, vec![garbled()]),
            ("proposeChanges", propose_body, vec![base_ref(), garbled()]),
            (
                "proposeChanges",
                propose_body,
                vec![base_ref(), base_commit(), garbled()],
            ),
            (
                "proposeChanges",
                propose_body,
                vec![base_ref(), base_commit(), sha("t"), garbled()],
            ),
            (
                "proposeChanges",
                propose_body,
                vec![
                    base_ref(),
                    base_commit(),
                    sha("t"),
                    sha("c"),
                    response(201, "{}"),
                    garbled(),
                ],
            ),
        ];

        for (tool, body, responses) in cases {
            let mut stub = Stub::of(responses);
            let failed = call(tool, body, &mut stub);
            assert!(
                matches!(failed, Err(ConnectorError::Upstream(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
                "{tool}: {failed:?}"
            );
        }
    }

    #[test]
    fn propagates_sender_errors() {
        // Arrange: the sender itself fails on the first call.
        let mut failing = |_: &Request| Err(ConnectorError::Upstream("network down".into()));

        // Act + assert: the connector error surfaces instead of a tool answer.
        for (tool, body) in [
            ("readFile", r#"{"owner":"o","repo":"r","path":"f"}"#),
            ("listDirectory", r#"{"owner":"o","repo":"r","path":"d"}"#),
            (
                "writeFile",
                r#"{"owner":"o","repo":"r","path":"f","branch":"main","content":"a2k=","message":"m"}"#,
            ),
            (
                "deleteFile",
                r#"{"owner":"o","repo":"r","path":"f","branch":"main"}"#,
            ),
            (
                "proposeChanges",
                r#"{"owner":"o","repo":"r","base":"main","new_branch":"f","files":[{"path":"a","content":"b"}],"title":"T"}"#,
            ),
        ] {
            let failed = Github::call_with(request(tool, body), &mut failing);
            assert!(
                matches!(failed, Err(ConnectorError::Upstream(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
                "{tool}: {failed:?}"
            );
        }
    }

    #[test]
    fn propagates_later_sender_errors() {
        // The sender fails after the first call: every later send's `?`
        // must propagate the failure instead of answering.
        let file_meta = || {
            ok_json(r#"{"type":"file","sha":"s","size":1,"encoding":"base64","content":"aGk="}"#)
        };
        let write_body =
            r#"{"owner":"o","repo":"r","path":"f","branch":"b","content":"aGk=","message":"m"}"#;
        let delete_body = r#"{"owner":"o","repo":"r","path":"f","branch":"b"}"#;
        let propose_body = r#"{"owner":"o","repo":"r","base":"main","new_branch":"f","files":[{"path":"a","content":"b"}],"title":"T"}"#;
        let base_ref = || ok_json(r#"{"object":{"sha":"b"}}"#);
        let base_commit = || ok_json(r#"{"sha":"b","tree":{"sha":"bt"}}"#);
        let sha = |s: &str| response(201, &format!(r#"{{"sha":"{s}"}}"#));

        // Each case queues the responses before the failing send; the stub's
        // exhaustion is the sender failure.
        let cases: &[(&str, &str, Vec<Response>)] = &[
            ("writeFile", write_body, vec![file_meta()]),
            ("deleteFile", delete_body, vec![file_meta()]),
            (
                "getIssue",
                r#"{"owner":"o","repo":"r","issue_number":1}"#,
                vec![ok_json("{}")],
            ),
            (
                "getPull",
                r#"{"owner":"o","repo":"r","pull_number":1}"#,
                vec![ok_json("{}")],
            ),
            ("proposeChanges", propose_body, vec![base_ref()]),
            (
                "proposeChanges",
                propose_body,
                vec![base_ref(), base_commit()],
            ),
            (
                "proposeChanges",
                propose_body,
                vec![base_ref(), base_commit(), sha("t")],
            ),
            (
                "proposeChanges",
                propose_body,
                vec![base_ref(), base_commit(), sha("t"), sha("c")],
            ),
            (
                "proposeChanges",
                propose_body,
                vec![
                    base_ref(),
                    base_commit(),
                    sha("t"),
                    sha("c"),
                    response(201, "{}"),
                ],
            ),
        ];

        for (tool, body, responses) in cases {
            let mut stub = Stub::of(responses);
            let failed = call(tool, body, &mut stub);
            assert!(
                matches!(failed, Err(ConnectorError::Upstream(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
                "{tool}: {failed:?}"
            );
        }
    }

    #[test]
    fn off_wasm_nothing_is_sent() {
        // Every tool goes through the production sender here, covering the
        // header-adding dispatch for each tool; off wasm the first send
        // fails before anything leaves the guest.
        let calls = [
            ("getRepo", r#"{"owner":"o","repo":"r"}"#),
            ("listRepos", r#"{}"#),
            ("createRepo", r#"{"name":"r"}"#),
            ("readFile", r#"{"owner":"o","repo":"r","path":"f"}"#),
            ("listDirectory", r#"{"owner":"o","repo":"r"}"#),
            (
                "writeFile",
                r#"{"owner":"o","repo":"r","path":"f","branch":"b","content":"a2k=","message":"m"}"#,
            ),
            (
                "deleteFile",
                r#"{"owner":"o","repo":"r","path":"f","branch":"b"}"#,
            ),
            ("listBranches", r#"{"owner":"o","repo":"r"}"#),
            ("listCommits", r#"{"owner":"o","repo":"r"}"#),
            ("getCommit", r#"{"owner":"o","repo":"r","ref":"main"}"#),
            ("listIssues", r#"{"owner":"o","repo":"r"}"#),
            ("getIssue", r#"{"owner":"o","repo":"r","issue_number":1}"#),
            ("createIssue", r#"{"owner":"o","repo":"r","title":"t"}"#),
            (
                "updateIssue",
                r#"{"owner":"o","repo":"r","issue_number":1}"#,
            ),
            (
                "commentOnIssue",
                r#"{"owner":"o","repo":"r","issue_number":1,"body":"b"}"#,
            ),
            ("listPulls", r#"{"owner":"o","repo":"r"}"#),
            ("getPull", r#"{"owner":"o","repo":"r","pull_number":1}"#),
            (
                "createPull",
                r#"{"owner":"o","repo":"r","title":"t","head":"a","base":"b"}"#,
            ),
            ("mergePull", r#"{"owner":"o","repo":"r","pull_number":1}"#),
            (
                "proposeChanges",
                r#"{"owner":"o","repo":"r","base":"b","new_branch":"n","files":[{"path":"f","content":"c"}],"title":"t"}"#,
            ),
            ("getAuthenticatedUser", r#"{}"#),
            ("listWorkflowRuns", r#"{"owner":"o","repo":"r"}"#),
        ];
        for (tool, body) in calls {
            let sent = Github::call_tool(request(tool, body));
            assert!(
                matches!(&sent, Err(ConnectorError::Upstream(m)) if m.contains("wasm32-wasip2")),
                "{tool}: {sent:?}"
            );
        }
    }

    #[test]
    fn manifest_is_embedded() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["name"], "github");
        assert_eq!(manifest["auth"]["schemes"][0]["type"], "oauth2");
        assert_eq!(
            manifest["permissions"]["http"]["allow"][0],
            DEFAULT_API_BASE
        );
        assert_eq!(manifest["version"], "0.2.0");
        assert_eq!(manifest["tools"].as_array().unwrap().len(), 22);
        let names: Vec<&str> = manifest["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "getRepo",
                "listRepos",
                "createRepo",
                "readFile",
                "listDirectory",
                "writeFile",
                "deleteFile",
                "listBranches",
                "listCommits",
                "getCommit",
                "listIssues",
                "getIssue",
                "createIssue",
                "updateIssue",
                "commentOnIssue",
                "listPulls",
                "getPull",
                "createPull",
                "mergePull",
                "proposeChanges",
                "getAuthenticatedUser",
                "listWorkflowRuns",
            ]
        );
    }
}
