//! GitLab connector: task-shaped tools over the GitLab REST API (v4).
//! Instead of mirroring endpoints one to one, each tool completes an agent's
//! job in one call: reading files as text, writing them without a
//! create/update split, fetching issues and merge requests together with
//! their notes, and proposing multi-file changes as a merge request. One
//! tool may make several HTTP calls internally to get there.
//!
//! The host runs the OAuth 2.0 sign-in, refreshes the token and adds
//! `Authorization: Bearer …` after each request leaves the guest: this code
//! never sees it.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use connectors_sdk::http::{Method, Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::Deserialize;
use serde_json::{Value, json};

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://gitlab.com/api/v4";

/// The connector.
#[derive(Debug)]
pub struct Gitlab;

#[derive(Debug, Deserialize)]
struct ProjectRef {
    project_id: String,
}

#[derive(Debug, Deserialize)]
struct ListProjects {
    archived: Option<bool>,
    visibility: Option<String>,
    order_by: Option<String>,
    sort: Option<String>,
    search: Option<String>,
    owned: Option<bool>,
    membership: Option<bool>,
    starred: Option<bool>,
    per_page: Option<u32>,
    page: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct CreateProject {
    name: String,
    path: Option<String>,
    description: Option<String>,
    visibility: Option<String>,
    initialize_with_readme: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ReadFile {
    project_id: String,
    file_path: String,
    #[serde(rename = "ref")]
    ref_: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListDirectory {
    project_id: String,
    path: Option<String>,
    #[serde(rename = "ref")]
    ref_: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WriteFile {
    project_id: String,
    file_path: String,
    branch: String,
    content: String,
    commit_message: String,
}

#[derive(Debug, Deserialize)]
struct DeleteFile {
    project_id: String,
    file_path: String,
    branch: String,
    commit_message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListBranches {
    project_id: String,
    search: Option<String>,
    per_page: Option<u32>,
    page: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ListCommits {
    project_id: String,
    ref_name: Option<String>,
    path: Option<String>,
    per_page: Option<u32>,
    page: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct GetCommit {
    project_id: String,
    sha: String,
}

#[derive(Debug, Deserialize)]
struct ListIssues {
    project_id: String,
    state: Option<String>,
    labels: Option<String>,
    per_page: Option<u32>,
    page: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct IssueRef {
    project_id: String,
    issue_iid: i64,
}

#[derive(Debug, Deserialize)]
struct CreateIssue {
    project_id: String,
    title: String,
    description: Option<String>,
    labels: Option<String>,
    assignee_ids: Option<Vec<i64>>,
}

#[derive(Debug, Deserialize)]
struct UpdateIssue {
    project_id: String,
    issue_iid: i64,
    title: Option<String>,
    description: Option<String>,
    state_event: Option<String>,
    labels: Option<String>,
    assignee_ids: Option<Vec<i64>>,
}

#[derive(Debug, Deserialize)]
struct CommentOnIssue {
    project_id: String,
    issue_iid: i64,
    body: String,
}

#[derive(Debug, Deserialize)]
struct ListMergeRequests {
    project_id: String,
    state: Option<String>,
    source_branch: Option<String>,
    target_branch: Option<String>,
    per_page: Option<u32>,
    page: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct MergeRequestRef {
    project_id: String,
    merge_request_iid: i64,
}

#[derive(Debug, Deserialize)]
struct CreateMergeRequest {
    project_id: String,
    title: String,
    source_branch: String,
    target_branch: String,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AcceptMergeRequest {
    project_id: String,
    merge_request_iid: i64,
    merge_commit_message: Option<String>,
    squash: Option<bool>,
    should_remove_source_branch: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ProposeChanges {
    project_id: String,
    target_branch: String,
    new_branch: String,
    files: Vec<ChangedFile>,
    title: String,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChangedFile {
    path: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ListPipelines {
    project_id: String,
    status: Option<String>,
    #[serde(rename = "ref")]
    ref_: Option<String>,
    per_page: Option<u32>,
    page: Option<u32>,
}

impl Connector for Gitlab {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        Self::call_with(request, |request| request.send())
    }
}

impl Gitlab {
    /// Dispatches a tool call, sending requests through `send`: the real
    /// `Request::send` in production, a stub in tests so multi-call tools are
    /// covered end to end without a network.
    fn call_with(
        request: ToolRequest,
        mut send: impl FnMut(&Request) -> Result<Response, ConnectorError>,
    ) -> Result<ToolResponse, ConnectorError> {
        let base = host::config("api_base").unwrap_or_else(|| DEFAULT_API_BASE.into());
        let api = base.trim_end_matches('/');
        match request.tool.as_str() {
            "getProject" => one_call(api, &request, &mut send, plan_get_project),
            "listProjects" => one_call(api, &request, &mut send, plan_list_projects),
            "createProject" => one_call(api, &request, &mut send, plan_create_project),
            "readFile" => read_file(api, &request, &mut send),
            "listDirectory" => list_directory(api, &request, &mut send),
            "writeFile" => write_file(api, &request, &mut send),
            "deleteFile" => one_call(api, &request, &mut send, plan_delete_file),
            "listBranches" => one_call(api, &request, &mut send, plan_list_branches),
            "listCommits" => one_call(api, &request, &mut send, plan_list_commits),
            "getCommit" => one_call(api, &request, &mut send, plan_get_commit),
            "listIssues" => one_call(api, &request, &mut send, plan_list_issues),
            "getIssue" => get_issue(api, &request, &mut send),
            "createIssue" => one_call(api, &request, &mut send, plan_create_issue),
            "updateIssue" => one_call(api, &request, &mut send, plan_update_issue),
            "commentOnIssue" => one_call(api, &request, &mut send, plan_comment_on_issue),
            "listMergeRequests" => one_call(api, &request, &mut send, plan_list_merge_requests),
            "getMergeRequest" => get_merge_request(api, &request, &mut send),
            "createMergeRequest" => one_call(api, &request, &mut send, plan_create_merge_request),
            "acceptMergeRequest" => one_call(api, &request, &mut send, plan_accept_merge_request),
            "proposeChanges" => propose_changes(api, &request, &mut send),
            "getCurrentUser" => {
                let planned = plan_get_current_user(api);
                answer(&send_with_agent(&mut send, planned)?)
            }
            "listPipelines" => one_call(api, &request, &mut send, plan_list_pipelines),
            other => Err(ConnectorError::NotFound(format!("no tool {other}"))),
        }
    }
}

connectors_sdk::export_connector!(Gitlab);

/// The project segment of a URL: a numeric ID passes through, a
/// `namespace/project` path is URL-encoded the way the API requires.
fn project(input: &str) -> String {
    encode(input)
}

/// `{api}/projects/{id}`: a numeric ID passes through, a `namespace/project`
/// path is URL-encoded the way the API requires.
fn project_path(api: &str, project_id: &str) -> String {
    format!("{api}/projects/{}", project(project_id))
}

/// `{project}/repository/files/{path}`, with the file path percent-encoded.
fn file_path(api: &str, project_id: &str, file_path: &str) -> String {
    format!(
        "{}/repository/files/{}",
        project_path(api, project_id),
        encode(file_path)
    )
}

/// The `per_page`/`page` query pair shared by the listing tools.
fn page_params(per_page: Option<u32>, page: Option<u32>) -> [(&'static str, Option<String>); 2] {
    [
        ("per_page", per_page.map(|n| n.to_string())),
        ("page", page.map(|n| n.to_string())),
    ]
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
/// a wrong project, a 401 an expired or missing token). A 2xx with an empty
/// body — GitLab's delete answers 204 — becomes `{"deleted": true}`.
fn answer(response: &Response) -> Result<ToolResponse, ConnectorError> {
    if !(200..300).contains(&response.status) {
        let body = String::from_utf8_lossy(&response.body);
        return Ok(ToolResponse::error(format!(
            "The API answered {}: {body}",
            response.status
        )));
    }
    if response.body.is_empty() {
        return ToolResponse::structured(&json!({ "deleted": true }));
    }
    let body: Value = response.json()?;
    ToolResponse::structured(&body)
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

/// Tags `planned` with the connector's user-agent and sends it through `send`.
fn send_with_agent(
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
    planned: Request,
) -> Result<Response, ConnectorError> {
    send(&planned.header(
        "user-agent",
        concat!("agentrq-gitlab-connector/", env!("CARGO_PKG_VERSION")),
    ))
}

/// Plans a single-request tool's call, sends it and answers the response.
fn one_call(
    api: &str,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
    plan: fn(&str, &ToolRequest) -> Result<Request, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let planned = plan(api, request)?;
    answer(&send_with_agent(send, planned)?)
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

fn plan_get_project(api: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: ProjectRef = request.json()?;
    Ok(Request::get(project_path(api, &input.project_id)))
}

fn plan_list_projects(api: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: ListProjects = request.json()?;
    let mut params: Vec<(&str, Option<String>)> = Vec::with_capacity(10);
    params.push(("archived", input.archived.map(|b| b.to_string())));
    params.push(("visibility", input.visibility));
    params.push(("order_by", input.order_by));
    params.push(("sort", input.sort));
    params.push(("search", input.search));
    params.push(("owned", input.owned.map(|b| b.to_string())));
    params.push(("membership", input.membership.map(|b| b.to_string())));
    params.push(("starred", input.starred.map(|b| b.to_string())));
    params.extend(page_params(input.per_page, input.page));
    Ok(Request::get(with_query(format!("{api}/projects"), &params)))
}

fn plan_create_project(api: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: CreateProject = request.json()?;
    let body = object(&[
        ("name", Some(json!(input.name))),
        ("path", input.path.as_ref().map(|path| json!(path))),
        (
            "description",
            input
                .description
                .as_ref()
                .map(|description| json!(description)),
        ),
        (
            "visibility",
            input
                .visibility
                .as_ref()
                .map(|visibility| json!(visibility)),
        ),
        (
            "initialize_with_readme",
            input
                .initialize_with_readme
                .map(|initialize| json!(initialize)),
        ),
    ]);
    Request::post(format!("{api}/projects")).json(&body)
}

fn plan_list_directory(api: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: ListDirectory = request.json()?;
    let params: [(&str, Option<String>); 2] = [("path", input.path), ("ref", input.ref_)];
    Ok(Request::get(with_query(
        format!("{}/repository/tree", project_path(api, &input.project_id)),
        &params,
    )))
}

fn plan_delete_file(api: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: DeleteFile = request.json()?;
    let body = object(&[
        ("branch", Some(json!(input.branch))),
        (
            "commit_message",
            input.commit_message.as_ref().map(|message| json!(message)),
        ),
    ]);
    Request::new(
        Method::Delete,
        file_path(api, &input.project_id, &input.file_path),
    )
    .json(&body)
}

fn plan_list_branches(api: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: ListBranches = request.json()?;
    let mut params: Vec<(&str, Option<String>)> = Vec::with_capacity(3);
    params.push(("search", input.search));
    params.extend(page_params(input.per_page, input.page));
    Ok(Request::get(with_query(
        format!(
            "{}/repository/branches",
            project_path(api, &input.project_id)
        ),
        &params,
    )))
}

fn plan_list_commits(api: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: ListCommits = request.json()?;
    let mut params: Vec<(&str, Option<String>)> = Vec::with_capacity(4);
    params.push(("ref_name", input.ref_name));
    params.push(("path", input.path));
    params.extend(page_params(input.per_page, input.page));
    Ok(Request::get(with_query(
        format!(
            "{}/repository/commits",
            project_path(api, &input.project_id)
        ),
        &params,
    )))
}

fn plan_get_commit(api: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: GetCommit = request.json()?;
    Ok(Request::get(format!(
        "{}/repository/commits/{}",
        project_path(api, &input.project_id),
        encode(&input.sha)
    )))
}

fn plan_list_issues(api: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: ListIssues = request.json()?;
    let mut params: Vec<(&str, Option<String>)> = Vec::with_capacity(4);
    params.push(("state", input.state));
    params.push(("labels", input.labels));
    params.extend(page_params(input.per_page, input.page));
    Ok(Request::get(with_query(
        format!("{}/issues", project_path(api, &input.project_id)),
        &params,
    )))
}

fn plan_create_issue(api: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: CreateIssue = request.json()?;
    let body = object(&[
        ("title", Some(json!(input.title))),
        (
            "description",
            input
                .description
                .as_ref()
                .map(|description| json!(description)),
        ),
        ("labels", input.labels.as_ref().map(|labels| json!(labels))),
        (
            "assignee_ids",
            input
                .assignee_ids
                .as_ref()
                .map(|assignees| json!(assignees)),
        ),
    ]);
    Request::post(format!("{}/issues", project_path(api, &input.project_id))).json(&body)
}

fn plan_update_issue(api: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: UpdateIssue = request.json()?;
    let body = object(&[
        ("title", input.title.as_ref().map(|title| json!(title))),
        (
            "description",
            input
                .description
                .as_ref()
                .map(|description| json!(description)),
        ),
        (
            "state_event",
            input.state_event.as_ref().map(|event| json!(event)),
        ),
        ("labels", input.labels.as_ref().map(|labels| json!(labels))),
        (
            "assignee_ids",
            input
                .assignee_ids
                .as_ref()
                .map(|assignees| json!(assignees)),
        ),
    ]);
    Request::new(
        Method::Put,
        format!(
            "{}/issues/{}",
            project_path(api, &input.project_id),
            input.issue_iid
        ),
    )
    .json(&body)
}

fn plan_comment_on_issue(api: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: CommentOnIssue = request.json()?;
    let body = object(&[("body", Some(json!(input.body)))]);
    Request::post(format!(
        "{}/issues/{}/notes",
        project_path(api, &input.project_id),
        input.issue_iid
    ))
    .json(&body)
}

fn plan_list_merge_requests(api: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: ListMergeRequests = request.json()?;
    let mut params: Vec<(&str, Option<String>)> = Vec::with_capacity(5);
    params.push(("state", input.state));
    params.push(("source_branch", input.source_branch));
    params.push(("target_branch", input.target_branch));
    params.extend(page_params(input.per_page, input.page));
    Ok(Request::get(with_query(
        format!("{}/merge_requests", project_path(api, &input.project_id)),
        &params,
    )))
}

fn plan_create_merge_request(api: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: CreateMergeRequest = request.json()?;
    let body = object(&[
        ("title", Some(json!(input.title))),
        ("source_branch", Some(json!(input.source_branch))),
        ("target_branch", Some(json!(input.target_branch))),
        (
            "description",
            input
                .description
                .as_ref()
                .map(|description| json!(description)),
        ),
    ]);
    Request::post(format!(
        "{}/merge_requests",
        project_path(api, &input.project_id)
    ))
    .json(&body)
}

fn plan_accept_merge_request(api: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: AcceptMergeRequest = request.json()?;
    let body = object(&[
        (
            "merge_commit_message",
            input
                .merge_commit_message
                .as_ref()
                .map(|message| json!(message)),
        ),
        ("squash", input.squash.map(|squash| json!(squash))),
        (
            "should_remove_source_branch",
            input
                .should_remove_source_branch
                .map(|remove| json!(remove)),
        ),
    ]);
    Request::new(
        Method::Put,
        format!(
            "{}/merge_requests/{}/merge",
            project_path(api, &input.project_id),
            input.merge_request_iid
        ),
    )
    .json(&body)
}

fn plan_get_current_user(api: &str) -> Request {
    Request::get(format!("{api}/user"))
}

fn plan_list_pipelines(api: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: ListPipelines = request.json()?;
    let mut params: Vec<(&str, Option<String>)> = Vec::with_capacity(4);
    params.push(("status", input.status));
    params.push(("ref", input.ref_));
    params.extend(page_params(input.per_page, input.page));
    Ok(Request::get(with_query(
        format!("{}/pipelines", project_path(api, &input.project_id)),
        &params,
    )))
}

/// Runs the calls behind `readFile`: fetches the file, then decodes its
/// base64 content to text — or answers the base64 untouched with `is_binary`
/// when the content is not UTF-8, so the agent never hand-decodes base64.
fn read_file(
    api: &str,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: ReadFile = request.json()?;
    let fetched = send_with_agent(send, plan_read_file(api, &input)?)?;
    if let Err(error) = step_ok(&fetched, "reading the file") {
        return Ok(error);
    }
    combine_read_file(&input, &fetched)
}

/// Plans `readFile`'s fetch: the file at `ref`, defaulting to the default
/// branch when the agent omits it.
fn plan_read_file(api: &str, input: &ReadFile) -> Result<Request, ConnectorError> {
    Ok(Request::get(with_query(
        file_path(api, &input.project_id, &input.file_path),
        &[("ref", input.ref_.clone())],
    )))
}

/// Turns the fetched file into `readFile`'s answer: decoded text, or the
/// base64 with `is_binary` when the bytes are not UTF-8.
fn combine_read_file(input: &ReadFile, fetched: &Response) -> Result<ToolResponse, ConnectorError> {
    let file: Value = fetched.json()?;
    let encoded = file
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| ConnectorError::Upstream("the API answered without file content".into()))?;
    let decoded = STANDARD.decode(encoded).map_err(|error| {
        ConnectorError::Upstream(format!("the API answered invalid base64 content: {error}"))
    })?;
    match String::from_utf8(decoded) {
        Ok(content) => ToolResponse::structured(&json!({
            "file_path": &input.file_path,
            "content": content,
        })),
        Err(_) => ToolResponse::structured(&json!({
            "file_path": &input.file_path,
            "content_base64": encoded,
            "is_binary": true,
        })),
    }
}

/// Runs the calls behind `listDirectory`: one listing, slimmed to the
/// name/path/type entries an agent actually navigates by.
fn list_directory(
    api: &str,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let listed = send_with_agent(send, plan_list_directory(api, request)?)?;
    if let Err(error) = step_ok(&listed, "listing the directory") {
        return Ok(error);
    }
    combine_directory(&listed)
}

/// Slims the tree entries to `{name, path, type}`.
fn combine_directory(listed: &Response) -> Result<ToolResponse, ConnectorError> {
    let entries: Vec<Value> = listed.json()?;
    let slim: Vec<Value> = entries
        .iter()
        .map(|entry| {
            json!({
                "name": entry.get("name"),
                "path": entry.get("path"),
                "type": entry.get("type"),
            })
        })
        .collect();
    ToolResponse::structured(&json!(slim))
}

/// Runs the calls behind `writeFile`: probes whether the file exists on the
/// branch, then creates or updates it — one tool call either way, with plain
/// text content the agent never base64-encodes.
fn write_file(
    api: &str,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: WriteFile = request.json()?;
    let probe = send_with_agent(send, plan_write_probe(api, &input)?)?;
    let committed = if probe.status == 404 {
        let created = send_with_agent(send, plan_write_create(api, &input)?)?;
        if let Err(error) = step_ok(&created, "creating the file") {
            return Ok(error);
        }
        created
    } else {
        if let Err(error) = step_ok(&probe, "checking whether the file exists") {
            return Ok(error);
        }
        let updated = send_with_agent(send, plan_write_update(api, &input)?)?;
        if let Err(error) = step_ok(&updated, "updating the file") {
            return Ok(error);
        }
        updated
    };
    combine_write_file(&input, &committed)
}

/// Plans `writeFile`'s existence probe: the file at the target branch.
fn plan_write_probe(api: &str, input: &WriteFile) -> Result<Request, ConnectorError> {
    Ok(Request::get(with_query(
        file_path(api, &input.project_id, &input.file_path),
        &[("ref", Some(input.branch.clone()))],
    )))
}

/// Plans `writeFile`'s create step: plain text content, no hand-encoding.
fn plan_write_create(api: &str, input: &WriteFile) -> Result<Request, ConnectorError> {
    let body = object(&[
        ("branch", Some(json!(&input.branch))),
        ("commit_message", Some(json!(&input.commit_message))),
        ("content", Some(json!(&input.content))),
        ("encoding", Some(json!("text"))),
    ]);
    Request::post(file_path(api, &input.project_id, &input.file_path)).json(&body)
}

/// Plans `writeFile`'s update step: the same body, sent as PUT.
fn plan_write_update(api: &str, input: &WriteFile) -> Result<Request, ConnectorError> {
    let body = object(&[
        ("branch", Some(json!(&input.branch))),
        ("commit_message", Some(json!(&input.commit_message))),
        ("content", Some(json!(&input.content))),
        ("encoding", Some(json!("text"))),
    ]);
    Request::new(
        Method::Put,
        file_path(api, &input.project_id, &input.file_path),
    )
    .json(&body)
}

/// Turns the create/update answer into `writeFile`'s result: the file path,
/// plus the commit SHA when the API reports one.
fn combine_write_file(
    input: &WriteFile,
    committed: &Response,
) -> Result<ToolResponse, ConnectorError> {
    let answer: Value = committed.json()?;
    let mut file = serde_json::Map::with_capacity(2);
    file.insert("file_path".to_string(), json!(&input.file_path));
    if let Some(sha) = answer.pointer("/commit/id").and_then(Value::as_str) {
        file.insert("commit_sha".to_string(), json!(sha));
    }
    ToolResponse::structured(&Value::Object(file))
}

/// Runs the calls behind `getIssue`: the issue plus its notes, so the agent
/// sees the discussion without a second tool call.
fn get_issue(
    api: &str,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: IssueRef = request.json()?;
    let fetched = send_with_agent(send, plan_get_issue(api, &input)?)?;
    if let Err(error) = step_ok(&fetched, "getting the issue") {
        return Ok(error);
    }
    let issue: Value = fetched.json()?;
    let listed = send_with_agent(send, plan_issue_notes(api, &input)?)?;
    if let Err(error) = step_ok(&listed, "listing the issue notes") {
        return Ok(error);
    }
    let notes: Value = listed.json()?;
    combine_issue_with_notes(issue, notes)
}

/// Plans `getIssue`'s issue fetch.
fn plan_get_issue(api: &str, input: &IssueRef) -> Result<Request, ConnectorError> {
    Ok(Request::get(format!(
        "{}/issues/{}",
        project_path(api, &input.project_id),
        input.issue_iid
    )))
}

/// Plans `getIssue`'s notes listing.
fn plan_issue_notes(api: &str, input: &IssueRef) -> Result<Request, ConnectorError> {
    Ok(Request::get(format!(
        "{}/issues/{}/notes",
        project_path(api, &input.project_id),
        input.issue_iid
    )))
}

/// Pairs the issue with its notes.
fn combine_issue_with_notes(issue: Value, notes: Value) -> Result<ToolResponse, ConnectorError> {
    ToolResponse::structured(&json!({ "issue": issue, "notes": notes }))
}

/// Runs the calls behind `getMergeRequest`: the merge request plus its
/// notes, so the agent sees the review discussion without a second call.
/// (The diff stays one `getCommit` on the merge request's SHA away.)
fn get_merge_request(
    api: &str,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: MergeRequestRef = request.json()?;
    let fetched = send_with_agent(send, plan_get_merge_request(api, &input)?)?;
    if let Err(error) = step_ok(&fetched, "getting the merge request") {
        return Ok(error);
    }
    let merge_request: Value = fetched.json()?;
    let listed = send_with_agent(send, plan_merge_request_notes(api, &input)?)?;
    if let Err(error) = step_ok(&listed, "listing the merge request notes") {
        return Ok(error);
    }
    let notes: Value = listed.json()?;
    combine_merge_request_with_notes(merge_request, notes)
}

/// Plans `getMergeRequest`'s merge request fetch.
fn plan_get_merge_request(api: &str, input: &MergeRequestRef) -> Result<Request, ConnectorError> {
    Ok(Request::get(format!(
        "{}/merge_requests/{}",
        project_path(api, &input.project_id),
        input.merge_request_iid
    )))
}

/// Plans `getMergeRequest`'s notes listing.
fn plan_merge_request_notes(api: &str, input: &MergeRequestRef) -> Result<Request, ConnectorError> {
    Ok(Request::get(format!(
        "{}/merge_requests/{}/notes",
        project_path(api, &input.project_id),
        input.merge_request_iid
    )))
}

/// Pairs the merge request with its notes.
fn combine_merge_request_with_notes(
    merge_request: Value,
    notes: Value,
) -> Result<ToolResponse, ConnectorError> {
    ToolResponse::structured(&json!({ "merge_request": merge_request, "notes": notes }))
}

/// Runs the calls behind `proposeChanges`: creates the branch, commits every
/// file in one multi-action commit (probing each file first so existing files
/// are updated and missing files are created), then opens the merge request.
fn propose_changes(
    api: &str,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: ProposeChanges = request.json()?;
    if input.files.is_empty() {
        return Err(ConnectorError::InvalidInput(
            "proposeChanges needs at least one file".into(),
        ));
    }
    let branched = send_with_agent(send, plan_create_branch(api, &input)?)?;
    if let Err(error) = step_ok(&branched, "creating the branch") {
        return Ok(error);
    }
    let mut actions: Vec<Value> = Vec::with_capacity(input.files.len());
    for file in &input.files {
        let probe = send_with_agent(send, plan_probe_change(api, &input, file)?)?;
        let action = match probe.status {
            404 => "create",
            _ => {
                if let Err(error) = step_ok(&probe, "checking whether the file exists") {
                    return Ok(error);
                }
                "update"
            }
        };
        actions.push(json!({
            "action": action,
            "file_path": &file.path,
            "content": &file.content,
            "encoding": "text",
        }));
    }
    let committed = send_with_agent(send, plan_commit_changes(api, &input, &actions)?)?;
    if let Err(error) = step_ok(&committed, "committing the changes") {
        return Ok(error);
    }
    let opened = send_with_agent(send, plan_open_merge_request(api, &input)?)?;
    if let Err(error) = step_ok(&opened, "opening the merge request") {
        return Ok(error);
    }
    combine_proposed_changes(&opened)
}

/// Plans `proposeChanges`' branch creation off the target branch.
fn plan_create_branch(api: &str, input: &ProposeChanges) -> Result<Request, ConnectorError> {
    let body = object(&[
        ("branch", Some(json!(&input.new_branch))),
        ("ref", Some(json!(&input.target_branch))),
    ]);
    Request::post(format!(
        "{}/repository/branches",
        project_path(api, &input.project_id)
    ))
    .json(&body)
}

/// Plans `proposeChanges`' per-file existence probe on the new branch.
fn plan_probe_change(
    api: &str,
    input: &ProposeChanges,
    file: &ChangedFile,
) -> Result<Request, ConnectorError> {
    Ok(Request::get(with_query(
        file_path(api, &input.project_id, &file.path),
        &[("ref", Some(input.new_branch.clone()))],
    )))
}

/// Plans `proposeChanges`' single commit: one call carrying every file as a
/// create/update action, with the title as the commit message.
fn plan_commit_changes(
    api: &str,
    input: &ProposeChanges,
    actions: &[Value],
) -> Result<Request, ConnectorError> {
    let body = object(&[
        ("branch", Some(json!(&input.new_branch))),
        ("commit_message", Some(json!(&input.title))),
        ("actions", Some(json!(actions))),
    ]);
    Request::post(format!(
        "{}/repository/commits",
        project_path(api, &input.project_id)
    ))
    .json(&body)
}

/// Plans `proposeChanges`' merge request from the new branch.
fn plan_open_merge_request(api: &str, input: &ProposeChanges) -> Result<Request, ConnectorError> {
    let body = object(&[
        ("title", Some(json!(&input.title))),
        ("source_branch", Some(json!(&input.new_branch))),
        ("target_branch", Some(json!(&input.target_branch))),
        (
            "description",
            input
                .description
                .as_ref()
                .map(|description| json!(description)),
        ),
    ]);
    Request::post(format!(
        "{}/merge_requests",
        project_path(api, &input.project_id)
    ))
    .json(&body)
}

/// Turns the opened merge request into `proposeChanges`' answer: its
/// project-scoped ID and its web URL.
fn combine_proposed_changes(opened: &Response) -> Result<ToolResponse, ConnectorError> {
    let merge_request: Value = opened.json()?;
    let iid = merge_request
        .get("iid")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            ConnectorError::Upstream("the API opened a merge request without an iid".into())
        })?;
    let url = merge_request
        .get("web_url")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ConnectorError::Upstream("the API opened a merge request without a web_url".into())
        })?;
    ToolResponse::structured(&json!({ "merge_request_iid": iid, "url": url }))
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

    /// A stub sender: drains queued responses and records the requests it saw.
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
        Gitlab::call_with(request(tool, body), |request| stub.send(request))
    }

    fn header(request: &Request, name: &str) -> Option<String> {
        request
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    const API: &str = "https://gitlab.com/api/v4";

    const FILE_BODY: &str = r#"{"project_id":"123","file_path":"src/lib.rs"}"#;

    fn file_fixture(content_base64: &str) -> String {
        format!(
            r#"{{"file_name":"lib.rs","file_path":"src/lib.rs","size":11,"encoding":"base64","content":"{content_base64}","ref":"main","blob_id":"abc","commit_id":"def","last_commit_id":"def"}}"#
        )
    }

    #[test]
    fn project_ids_encode() {
        assert_eq!(project("123"), "123");
        assert_eq!(project("group/project"), "group%2Fproject");
        assert_eq!(project("group/sub group/p"), "group%2Fsub%20group%2Fp");
    }

    #[test]
    fn plans_project_tools() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"id":123}"#)]);

        // Act.
        let answered = call("getProject", r#"{"project_id":"123"}"#, &mut stub).unwrap();

        // Assert.
        assert_eq!(answered.structured.as_deref(), Some(r#"{"id":123}"#));
        assert_eq!(stub.seen[0].url, format!("{API}/projects/123"));

        // Arrange: every listProjects parameter set.
        let mut stub = Stub::of(&[ok_json("[]")]);

        // Act.
        call(
            "listProjects",
            r#"{"membership":true,"owned":true,"starred":false,"archived":false,"visibility":"private","order_by":"updated_at","sort":"desc","search":"demo","per_page":50,"page":2}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            stub.seen[0].url,
            format!(
                "{API}/projects?archived=false&visibility=private&order_by=updated_at&sort=desc&search=demo&owned=true&membership=true&starred=false&per_page=50&page=2"
            )
        );

        // Arrange: no parameters at all.
        let mut stub = Stub::of(&[ok_json("[]")]);

        // Act.
        call("listProjects", "{}", &mut stub).unwrap();

        // Assert: no query string.
        assert_eq!(stub.seen[0].url, format!("{API}/projects"));

        // Arrange: a full createProject call.
        let mut stub = Stub::of(&[ok_json(r#"{"id":9}"#)]);

        // Act.
        call(
            "createProject",
            r#"{"name":"new-project","path":"new-project","description":"A test","visibility":"private","initialize_with_readme":true}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(stub.seen[0].url, format!("{API}/projects"));
        assert_eq!(stub.seen[0].method, Method::Post);
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            body,
            json!({
                "name": "new-project",
                "path": "new-project",
                "description": "A test",
                "visibility": "private",
                "initialize_with_readme": true,
            })
        );

        // Arrange: a minimal createProject call.
        let mut stub = Stub::of(&[ok_json(r#"{"id":9}"#)]);

        // Act.
        call("createProject", r#"{"name":"new-project"}"#, &mut stub).unwrap();

        // Assert: unset optionals are omitted from the body.
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(body, json!({ "name": "new-project" }));
    }

    #[test]
    fn plans_branch_and_commit_tools() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json("[]")]);

        // Act.
        call(
            "listBranches",
            r#"{"project_id":"group/p","search":"feat","per_page":30,"page":3}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/projects/group%2Fp/repository/branches?search=feat&per_page=30&page=3")
        );

        // Arrange.
        let mut stub = Stub::of(&[ok_json("[]")]);

        // Act.
        call(
            "listCommits",
            r#"{"project_id":"123","ref_name":"main","path":"src/lib.rs","per_page":10,"page":1}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            stub.seen[0].url,
            format!(
                "{API}/projects/123/repository/commits?ref_name=main&path=src%2Flib.rs&per_page=10&page=1"
            )
        );

        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"id":"abc"}"#)]);

        // Act.
        call(
            "getCommit",
            r#"{"project_id":"123","sha":"feature/cool"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the SHA is percent-encoded as a path segment.
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/projects/123/repository/commits/feature%2Fcool")
        );
    }

    #[test]
    fn reads_file_as_text() {
        // Arrange: the API's base64 file payload.
        let mut stub = Stub::of(&[ok_json(&file_fixture("aGVsbG8gd29ybGQ="))]);

        // Act.
        let answered = call(
            "readFile",
            r#"{"project_id":"123","file_path":"src/lib.rs","ref":"main"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: decoded text, no base64 in sight.
        assert_eq!(
            answered.structured.as_deref(),
            Some(r#"{"content":"hello world","file_path":"src/lib.rs"}"#)
        );
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/projects/123/repository/files/src%2Flib.rs?ref=main")
        );
    }

    #[test]
    fn reads_file_at_default_branch_without_ref() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(&file_fixture("aGVsbG8="))]);

        // Act.
        call("readFile", FILE_BODY, &mut stub).unwrap();

        // Assert: no ref query parameter when omitted.
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/projects/123/repository/files/src%2Flib.rs")
        );
    }

    #[test]
    fn reads_binary_file_as_base64() {
        // Arrange: bytes that are not valid UTF-8.
        let mut stub = Stub::of(&[ok_json(&file_fixture("//8="))]);

        // Act.
        let answered = call("readFile", FILE_BODY, &mut stub).unwrap();

        // Assert: the base64 comes back untouched, flagged binary.
        assert_eq!(
            answered.structured.as_deref(),
            Some(r#"{"content_base64":"//8=","file_path":"src/lib.rs","is_binary":true}"#)
        );
    }

    #[test]
    fn reports_file_read_failure() {
        // Arrange: the file does not exist.
        let mut stub = Stub::of(&[response(404, r#"{"message":"404 File Not Found"}"#)]);

        // Act.
        let answered = call("readFile", FILE_BODY, &mut stub).unwrap();

        // Assert: a tool error naming the failed step.
        assert_eq!(
            answered,
            ToolResponse::error(
                r#"reading the file failed: the API answered 404: {"message":"404 File Not Found"}"#
            )
        );
    }

    #[test]
    fn reports_file_without_content() {
        // Arrange: a file payload with no content field.
        let answered = combine_read_file(
            &ReadFile {
                project_id: "123".into(),
                file_path: "src/lib.rs".into(),
                ref_: None,
            },
            &response(200, r#"{"file_path":"src/lib.rs"}"#),
        );

        // Assert.
        assert_eq!(
            answered.unwrap_err(),
            ConnectorError::Upstream("the API answered without file content".into())
        );
    }

    #[test]
    fn rejects_invalid_base64_file_content() {
        // Arrange: a file payload whose content is not base64.
        let answered = combine_read_file(
            &ReadFile {
                project_id: "123".into(),
                file_path: "src/lib.rs".into(),
                ref_: None,
            },
            &response(200, &file_fixture("!!!")),
        );

        // Assert.
        assert!(
            matches!(&answered, Err(ConnectorError::Upstream(message)) if message.starts_with("the API answered invalid base64 content")),
            "{answered:?}"
        );
    }

    #[test]
    fn lists_directory_entries() {
        // Arrange: the tree payload with its extra fields.
        let mut stub = Stub::of(&[ok_json(
            r#"[{"id":"a","name":"lib.rs","type":"blob","path":"src/lib.rs","mode":"100644"},{"id":"b","name":"bin","type":"tree","path":"src/bin","mode":"040000"}]"#,
        )]);

        // Act.
        let answered = call(
            "listDirectory",
            r#"{"project_id":"123","path":"src","ref":"main"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: slimmed to name/path/type.
        assert_eq!(
            answered.structured.as_deref(),
            Some(
                r#"[{"name":"lib.rs","path":"src/lib.rs","type":"blob"},{"name":"bin","path":"src/bin","type":"tree"}]"#
            )
        );
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/projects/123/repository/tree?path=src&ref=main")
        );
    }

    #[test]
    fn lists_directory_at_root_without_options() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json("[]")]);

        // Act.
        call("listDirectory", r#"{"project_id":"123"}"#, &mut stub).unwrap();

        // Assert: no query string when path and ref are omitted.
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/projects/123/repository/tree")
        );
    }

    #[test]
    fn reports_directory_listing_failure() {
        // Arrange.
        let mut stub = Stub::of(&[response(403, "forbidden")]);

        // Act.
        let answered = call("listDirectory", r#"{"project_id":"123"}"#, &mut stub).unwrap();

        // Assert.
        assert_eq!(
            answered,
            ToolResponse::error("listing the directory failed: the API answered 403: forbidden")
        );
    }

    #[test]
    fn creates_missing_file() {
        // Arrange: the probe 404s, then the create succeeds.
        let mut stub = Stub::of(&[
            response(404, r#"{"message":"404 File Not Found"}"#),
            response(201, r#"{"file_path":"docs/notes.md","branch":"main"}"#),
        ]);

        // Act.
        let answered = call(
            "writeFile",
            r##"{"project_id":"123","file_path":"docs/notes.md","branch":"main","commit_message":"Add notes","content":"# Notes"}"##,
            &mut stub,
        )
        .unwrap();

        // Assert: two calls — probe, then POST.
        assert_eq!(stub.seen.len(), 2);
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/projects/123/repository/files/docs%2Fnotes.md?ref=main")
        );
        assert_eq!(stub.seen[1].method, Method::Post);
        let body: Value = serde_json::from_slice(&stub.seen[1].body).unwrap();
        assert_eq!(
            body,
            json!({
                "branch": "main",
                "commit_message": "Add notes",
                "content": "# Notes",
                "encoding": "text",
            })
        );
        assert_eq!(
            answered.structured.as_deref(),
            Some(r#"{"file_path":"docs/notes.md"}"#)
        );
    }

    #[test]
    fn updates_existing_file() {
        // Arrange: the probe finds the file, then the update succeeds with a commit.
        let mut stub = Stub::of(&[
            ok_json(&file_fixture("aGVsbG8=")),
            ok_json(r#"{"file_path":"docs/notes.md","branch":"main","commit":{"id":"abc123"}}"#),
        ]);

        // Act.
        let answered = call(
            "writeFile",
            r##"{"project_id":"123","file_path":"docs/notes.md","branch":"main","commit_message":"Tweak notes","content":"# Notes!"}"##,
            &mut stub,
        )
        .unwrap();

        // Assert: the second call is a PUT, and the commit SHA is reported.
        assert_eq!(stub.seen.len(), 2);
        assert_eq!(stub.seen[1].method, Method::Put);
        assert_eq!(
            answered.structured.as_deref(),
            Some(r#"{"commit_sha":"abc123","file_path":"docs/notes.md"}"#)
        );
    }

    #[test]
    fn reports_write_probe_failure() {
        // Arrange: the probe itself is refused.
        let mut stub = Stub::of(&[response(403, "forbidden")]);

        // Act.
        let answered = call(
            "writeFile",
            r#"{"project_id":"123","file_path":"f.md","branch":"main","commit_message":"m","content":"c"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            answered,
            ToolResponse::error(
                "checking whether the file exists failed: the API answered 403: forbidden"
            )
        );
    }

    #[test]
    fn reports_file_create_failure() {
        // Arrange: the probe 404s, then the create is refused.
        let mut stub = Stub::of(&[
            response(404, r#"{"message":"404 File Not Found"}"#),
            response(400, r#"{"message":"branch is missing"}"#),
        ]);

        // Act.
        let answered = call(
            "writeFile",
            r#"{"project_id":"123","file_path":"f.md","branch":"nope","commit_message":"m","content":"c"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            answered,
            ToolResponse::error(
                r#"creating the file failed: the API answered 400: {"message":"branch is missing"}"#
            )
        );
    }

    #[test]
    fn reports_file_update_failure() {
        // Arrange: the probe finds the file, then the update is refused.
        let mut stub = Stub::of(&[
            ok_json(&file_fixture("aGVsbG8=")),
            response(400, r#"{"message":"content is invalid"}"#),
        ]);

        // Act.
        let answered = call(
            "writeFile",
            r#"{"project_id":"123","file_path":"f.md","branch":"main","commit_message":"m","content":"c"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            answered,
            ToolResponse::error(
                r#"updating the file failed: the API answered 400: {"message":"content is invalid"}"#
            )
        );
    }

    #[test]
    fn deletes_file() {
        // Arrange: GitLab answers 204 with an empty body.
        let mut stub = Stub::of(&[response(204, "")]);

        // Act.
        let answered = call(
            "deleteFile",
            r#"{"project_id":"123","file_path":"old.md","branch":"main","commit_message":"Remove"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(stub.seen[0].method, Method::Delete);
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/projects/123/repository/files/old.md")
        );
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            body,
            json!({ "branch": "main", "commit_message": "Remove" })
        );
        assert_eq!(answered.structured.as_deref(), Some(r#"{"deleted":true}"#));
    }

    #[test]
    fn deletes_file_without_commit_message() {
        // Arrange.
        let mut stub = Stub::of(&[response(204, "")]);

        // Act.
        call(
            "deleteFile",
            r#"{"project_id":"123","file_path":"old.md","branch":"main"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the commit message is omitted when not given.
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(body, json!({ "branch": "main" }));
    }

    #[test]
    fn plans_issue_tools() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json("[]")]);

        // Act.
        call(
            "listIssues",
            r#"{"project_id":"123","state":"opened","labels":"bug,ui","per_page":25,"page":1}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/projects/123/issues?state=opened&labels=bug%2Cui&per_page=25&page=1")
        );

        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"iid":1}"#)]);

        // Act.
        call(
            "createIssue",
            r#"{"project_id":"123","title":"Bug","description":"It broke","labels":"bug","assignee_ids":[7]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(stub.seen[0].method, Method::Post);
        assert_eq!(stub.seen[0].url, format!("{API}/projects/123/issues"));
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            body,
            json!({
                "title": "Bug",
                "description": "It broke",
                "labels": "bug",
                "assignee_ids": [7],
            })
        );

        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"iid":7}"#)]);

        // Act.
        call(
            "updateIssue",
            r#"{"project_id":"123","issue_iid":7,"title":"T","description":"B","state_event":"close","labels":"l","assignee_ids":[1,2]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(stub.seen[0].method, Method::Put);
        assert_eq!(stub.seen[0].url, format!("{API}/projects/123/issues/7"));
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            body,
            json!({ "title": "T", "description": "B", "state_event": "close", "labels": "l", "assignee_ids": [1, 2] })
        );

        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"id":1}"#)]);

        // Act.
        call(
            "commentOnIssue",
            r#"{"project_id":"123","issue_iid":7,"body":"Nice work"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(stub.seen[0].method, Method::Post);
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/projects/123/issues/7/notes")
        );
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(body, json!({ "body": "Nice work" }));
    }

    #[test]
    fn gets_issue_with_notes() {
        // Arrange: the issue, then its notes.
        let mut stub = Stub::of(&[
            ok_json(r#"{"iid":42,"title":"Bug"}"#),
            ok_json(r#"[{"id":1,"body":"Looking into it"}]"#),
        ]);

        // Act.
        let answered = call(
            "getIssue",
            r#"{"project_id":"123","issue_iid":42}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: both calls happened, paired in one answer.
        assert_eq!(stub.seen.len(), 2);
        assert_eq!(stub.seen[0].url, format!("{API}/projects/123/issues/42"));
        assert_eq!(
            stub.seen[1].url,
            format!("{API}/projects/123/issues/42/notes")
        );
        assert_eq!(
            answered.structured.as_deref(),
            Some(
                r#"{"issue":{"iid":42,"title":"Bug"},"notes":[{"body":"Looking into it","id":1}]}"#
            )
        );
    }

    #[test]
    fn reports_issue_fetch_failure() {
        // Arrange.
        let mut stub = Stub::of(&[response(404, "not found")]);

        // Act.
        let answered = call(
            "getIssue",
            r#"{"project_id":"123","issue_iid":42}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            answered,
            ToolResponse::error("getting the issue failed: the API answered 404: not found")
        );
    }

    #[test]
    fn reports_issue_notes_failure() {
        // Arrange: the issue loads, its notes do not.
        let mut stub = Stub::of(&[ok_json(r#"{"iid":42}"#), response(403, "forbidden")]);

        // Act.
        let answered = call(
            "getIssue",
            r#"{"project_id":"123","issue_iid":42}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            answered,
            ToolResponse::error("listing the issue notes failed: the API answered 403: forbidden")
        );
    }

    #[test]
    fn plans_merge_request_tools() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json("[]")]);

        // Act.
        call(
            "listMergeRequests",
            r#"{"project_id":"123","state":"opened","source_branch":"feature","target_branch":"main","per_page":20,"page":2}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            stub.seen[0].url,
            format!(
                "{API}/projects/123/merge_requests?state=opened&source_branch=feature&target_branch=main&per_page=20&page=2"
            )
        );

        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"iid":3}"#)]);

        // Act.
        call(
            "createMergeRequest",
            r#"{"project_id":"123","title":"T","source_branch":"feature","target_branch":"main","description":"B"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(stub.seen[0].method, Method::Post);
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/projects/123/merge_requests")
        );
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            body,
            json!({ "title": "T", "source_branch": "feature", "target_branch": "main", "description": "B" })
        );

        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"state":"merged"}"#)]);

        // Act.
        call(
            "acceptMergeRequest",
            r#"{"project_id":"123","merge_request_iid":9,"merge_commit_message":"M","squash":true,"should_remove_source_branch":true}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(stub.seen[0].method, Method::Put);
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/projects/123/merge_requests/9/merge")
        );
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            body,
            json!({ "merge_commit_message": "M", "squash": true, "should_remove_source_branch": true })
        );

        // Arrange: a merge with no options.
        let mut stub = Stub::of(&[ok_json(r#"{"state":"merged"}"#)]);

        // Act.
        call(
            "acceptMergeRequest",
            r#"{"project_id":"123","merge_request_iid":9}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: unset optionals are omitted.
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(body, json!({}));
    }

    #[test]
    fn gets_merge_request_with_notes() {
        // Arrange: the merge request, then its notes.
        let mut stub = Stub::of(&[
            ok_json(r#"{"iid":3,"title":"Feature"}"#),
            ok_json(r#"[{"id":2,"body":"LGTM"}]"#),
        ]);

        // Act.
        let answered = call(
            "getMergeRequest",
            r#"{"project_id":"123","merge_request_iid":3}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(stub.seen.len(), 2);
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/projects/123/merge_requests/3")
        );
        assert_eq!(
            stub.seen[1].url,
            format!("{API}/projects/123/merge_requests/3/notes")
        );
        assert_eq!(
            answered.structured.as_deref(),
            Some(
                r#"{"merge_request":{"iid":3,"title":"Feature"},"notes":[{"body":"LGTM","id":2}]}"#
            )
        );
    }

    #[test]
    fn reports_merge_request_fetch_failure() {
        // Arrange.
        let mut stub = Stub::of(&[response(404, "not found")]);

        // Act.
        let answered = call(
            "getMergeRequest",
            r#"{"project_id":"123","merge_request_iid":3}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            answered,
            ToolResponse::error(
                "getting the merge request failed: the API answered 404: not found"
            )
        );
    }

    #[test]
    fn reports_merge_request_notes_failure() {
        // Arrange: the merge request loads, its notes do not.
        let mut stub = Stub::of(&[ok_json(r#"{"iid":3}"#), response(403, "forbidden")]);

        // Act.
        let answered = call(
            "getMergeRequest",
            r#"{"project_id":"123","merge_request_iid":3}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            answered,
            ToolResponse::error(
                "listing the merge request notes failed: the API answered 403: forbidden"
            )
        );
    }

    #[test]
    fn proposes_changes() {
        // Arrange: branch, a missing file, the commit, the merge request.
        let mut stub = Stub::of(&[
            response(201, r#"{"name":"feature/notes"}"#),
            response(404, r#"{"message":"404 File Not Found"}"#),
            response(201, r#"{"id":"abc"}"#),
            response(
                201,
                r#"{"iid":5,"web_url":"https://gitlab.com/group/p/-/merge_requests/5"}"#,
            ),
        ]);

        // Act.
        let answered = call(
            "proposeChanges",
            r##"{"project_id":"group/p","target_branch":"main","new_branch":"feature/notes","files":[{"path":"docs/notes.md","content":"# Notes"}],"title":"Add notes","description":"First draft"}"##,
            &mut stub,
        )
        .unwrap();

        // Assert: branch, probe, commit, merge request — four calls.
        assert_eq!(stub.seen.len(), 4);
        assert_eq!(stub.seen[0].method, Method::Post);
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/projects/group%2Fp/repository/branches")
        );
        let branch_body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            branch_body,
            json!({ "branch": "feature/notes", "ref": "main" })
        );
        assert_eq!(
            stub.seen[1].url,
            format!(
                "{API}/projects/group%2Fp/repository/files/docs%2Fnotes.md?ref=feature%2Fnotes"
            )
        );
        assert_eq!(stub.seen[2].method, Method::Post);
        assert_eq!(
            stub.seen[2].url,
            format!("{API}/projects/group%2Fp/repository/commits")
        );
        let commit_body: Value = serde_json::from_slice(&stub.seen[2].body).unwrap();
        assert_eq!(
            commit_body,
            json!({
                "branch": "feature/notes",
                "commit_message": "Add notes",
                "actions": [
                    { "action": "create", "file_path": "docs/notes.md", "content": "# Notes", "encoding": "text" },
                ],
            })
        );
        assert_eq!(stub.seen[3].method, Method::Post);
        assert_eq!(
            stub.seen[3].url,
            format!("{API}/projects/group%2Fp/merge_requests")
        );
        let mr_body: Value = serde_json::from_slice(&stub.seen[3].body).unwrap();
        assert_eq!(
            mr_body,
            json!({
                "title": "Add notes",
                "source_branch": "feature/notes",
                "target_branch": "main",
                "description": "First draft",
            })
        );
        assert_eq!(
            answered.structured.as_deref(),
            Some(
                r#"{"merge_request_iid":5,"url":"https://gitlab.com/group/p/-/merge_requests/5"}"#
            )
        );
    }

    #[test]
    fn proposes_changes_updating_existing_files() {
        // Arrange: branch, an existing file, the commit, the merge request.
        let mut stub = Stub::of(&[
            response(201, r#"{"name":"feature/tweak"}"#),
            ok_json(&file_fixture("aGVsbG8=")),
            response(201, r#"{"id":"abc"}"#),
            response(
                201,
                r#"{"iid":6,"web_url":"https://gitlab.com/g/p/-/merge_requests/6"}"#,
            ),
        ]);

        // Act.
        let answered = call(
            "proposeChanges",
            r#"{"project_id":"123","target_branch":"main","new_branch":"feature/tweak","files":[{"path":"f.md","content":"new"}],"title":"Tweak"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the existing file gets an update action, and the optional
        // description is omitted from the merge request body.
        let commit_body: Value = serde_json::from_slice(&stub.seen[2].body).unwrap();
        assert_eq!(
            commit_body["actions"],
            json!([
                { "action": "update", "file_path": "f.md", "content": "new", "encoding": "text" },
            ])
        );
        let mr_body: Value = serde_json::from_slice(&stub.seen[3].body).unwrap();
        assert_eq!(
            mr_body,
            json!({ "title": "Tweak", "source_branch": "feature/tweak", "target_branch": "main" })
        );
        assert_eq!(
            answered.structured.as_deref(),
            Some(r#"{"merge_request_iid":6,"url":"https://gitlab.com/g/p/-/merge_requests/6"}"#)
        );
    }

    #[test]
    fn refuses_propose_changes_without_files() {
        // Arrange.
        let mut stub = Stub::of(&[]);

        // Act.
        let refused = call(
            "proposeChanges",
            r#"{"project_id":"123","target_branch":"main","new_branch":"f","files":[],"title":"T"}"#,
            &mut stub,
        );

        // Assert: invalid input, and nothing was sent.
        assert_eq!(
            refused.unwrap_err(),
            ConnectorError::InvalidInput("proposeChanges needs at least one file".into())
        );
        assert!(stub.seen.is_empty());
    }

    #[test]
    fn reports_branch_creation_failure() {
        // Arrange: the branch already exists.
        let mut stub = Stub::of(&[response(400, r#"{"message":"branch exists"}"#)]);

        // Act.
        let answered = call(
            "proposeChanges",
            r#"{"project_id":"123","target_branch":"main","new_branch":"taken","files":[{"path":"f.md","content":"c"}],"title":"T"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            answered,
            ToolResponse::error(
                r#"creating the branch failed: the API answered 400: {"message":"branch exists"}"#
            )
        );
    }

    #[test]
    fn reports_change_probe_failure() {
        // Arrange: branch ok, then the probe is refused.
        let mut stub = Stub::of(&[response(201, r#"{"name":"f"}"#), response(403, "forbidden")]);

        // Act.
        let answered = call(
            "proposeChanges",
            r#"{"project_id":"123","target_branch":"main","new_branch":"f","files":[{"path":"f.md","content":"c"}],"title":"T"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            answered,
            ToolResponse::error(
                "checking whether the file exists failed: the API answered 403: forbidden"
            )
        );
    }

    #[test]
    fn reports_commit_failure() {
        // Arrange: branch and probe ok, then the commit is refused.
        let mut stub = Stub::of(&[
            response(201, r#"{"name":"f"}"#),
            response(404, r#"{"message":"404 File Not Found"}"#),
            response(400, r#"{"message":"actions are invalid"}"#),
        ]);

        // Act.
        let answered = call(
            "proposeChanges",
            r#"{"project_id":"123","target_branch":"main","new_branch":"f","files":[{"path":"f.md","content":"c"}],"title":"T"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            answered,
            ToolResponse::error(
                r#"committing the changes failed: the API answered 400: {"message":"actions are invalid"}"#
            )
        );
    }

    #[test]
    fn reports_merge_request_creation_failure() {
        // Arrange: everything ok until opening the merge request.
        let mut stub = Stub::of(&[
            response(201, r#"{"name":"f"}"#),
            response(404, r#"{"message":"404 File Not Found"}"#),
            response(201, r#"{"id":"abc"}"#),
            response(400, r#"{"message":"source branch missing"}"#),
        ]);

        // Act.
        let answered = call(
            "proposeChanges",
            r#"{"project_id":"123","target_branch":"main","new_branch":"f","files":[{"path":"f.md","content":"c"}],"title":"T"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            answered,
            ToolResponse::error(
                r#"opening the merge request failed: the API answered 400: {"message":"source branch missing"}"#
            )
        );
    }

    #[test]
    fn reports_merge_request_without_iid() {
        // Arrange: the API opened something without an iid.
        let answered = combine_proposed_changes(&response(
            201,
            r#"{"web_url":"https://gitlab.com/g/p/-/merge_requests/5"}"#,
        ));

        // Assert.
        assert_eq!(
            answered.unwrap_err(),
            ConnectorError::Upstream("the API opened a merge request without an iid".into())
        );
    }

    #[test]
    fn reports_merge_request_without_url() {
        // Arrange: the API opened something without a web_url.
        let answered = combine_proposed_changes(&response(201, r#"{"iid":5}"#));

        // Assert.
        assert_eq!(
            answered.unwrap_err(),
            ConnectorError::Upstream("the API opened a merge request without a web_url".into())
        );
    }

    #[test]
    fn plans_user_and_pipeline_tools() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"username":"octocat"}"#)]);

        // Act.
        let answered = call("getCurrentUser", "{}", &mut stub).unwrap();

        // Assert.
        assert_eq!(
            answered.structured.as_deref(),
            Some(r#"{"username":"octocat"}"#)
        );
        assert_eq!(stub.seen[0].url, format!("{API}/user"));

        // Arrange.
        let mut stub = Stub::of(&[ok_json("[]")]);

        // Act.
        call(
            "listPipelines",
            r#"{"project_id":"123","status":"success","ref":"main","per_page":5,"page":2}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/projects/123/pipelines?status=success&ref=main&per_page=5&page=2")
        );

        // Arrange: no filters.
        let mut stub = Stub::of(&[ok_json("[]")]);

        // Act.
        call("listPipelines", r#"{"project_id":"123"}"#, &mut stub).unwrap();

        // Assert: no query string.
        assert_eq!(stub.seen[0].url, format!("{API}/projects/123/pipelines"));
    }

    #[test]
    fn sets_gitlab_user_agent() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"id":123}"#)]);

        // Act.
        call("getProject", r#"{"project_id":"123"}"#, &mut stub).unwrap();

        // Assert.
        let agent = header(&stub.seen[0], "user-agent").unwrap();
        assert!(
            agent.starts_with("agentrq-gitlab-connector/"),
            "unexpected user-agent {agent}"
        );
    }

    #[test]
    fn refuses_invalid_input_for_every_tool() {
        // Arrange: every tool that parses a JSON body.
        let tools = [
            "getProject",
            "listProjects",
            "createProject",
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
            "listMergeRequests",
            "getMergeRequest",
            "createMergeRequest",
            "acceptMergeRequest",
            "proposeChanges",
            "listPipelines",
        ];
        for tool in tools {
            let mut stub = Stub::of(&[]);

            // Act: a body that is not JSON at all.
            let result = call(tool, "not json", &mut stub);

            // Assert: invalid input, and nothing was sent.
            assert!(
                matches!(result, Err(ConnectorError::InvalidInput(_))),
                "{tool}: {result:?}"
            );
            assert!(stub.seen.is_empty(), "{tool} sent a request");
        }
    }

    #[test]
    fn rejects_unknown_tool() {
        // Arrange.
        let mut stub = Stub::of(&[]);

        // Act.
        let unknown = call("nope", "{}", &mut stub);

        // Assert.
        assert_eq!(
            unknown.unwrap_err(),
            ConnectorError::NotFound("no tool nope".into())
        );
    }

    #[test]
    fn propagates_sender_errors() {
        // Arrange: the sender itself fails.
        let failed = Gitlab::call_with(request("getProject", r#"{"project_id":"123"}"#), |_| {
            Err(ConnectorError::Upstream("network down".into()))
        });

        // Assert.
        assert_eq!(
            failed.unwrap_err(),
            ConnectorError::Upstream("network down".into())
        );

        // Arrange: a stub with no responses queued.
        let mut stub = Stub::of(&[]);

        // Act.
        let exhausted = call("getProject", r#"{"project_id":"123"}"#, &mut stub);

        // Assert.
        assert_eq!(
            exhausted.unwrap_err(),
            ConnectorError::Upstream("stub sender ran out of responses".into())
        );
    }

    #[test]
    fn maps_responses() {
        let ok = answer(&response(200, r#"{"username":"octocat"}"#)).unwrap();
        assert_eq!(ok.structured.as_deref(), Some(r#"{"username":"octocat"}"#));
        let created = answer(&response(201, r#"{"id":1}"#)).unwrap();
        assert_eq!(created.structured.as_deref(), Some(r#"{"id":1}"#));
        let deleted = answer(&response(204, "")).unwrap();
        assert_eq!(deleted.structured.as_deref(), Some(r#"{"deleted":true}"#));
        let missing = answer(&response(404, r#"{"message":"404 Not Found"}"#)).unwrap();
        assert_eq!(
            missing,
            ToolResponse::error(r#"The API answered 404: {"message":"404 Not Found"}"#)
        );
        let unauthorized = answer(&response(401, r#"{"message":"401 Unauthorized"}"#)).unwrap();
        assert_eq!(
            unauthorized,
            ToolResponse::error(r#"The API answered 401: {"message":"401 Unauthorized"}"#)
        );
        let garbled = answer(&response(200, "<html>"));
        assert!(
            matches!(garbled, Err(ConnectorError::Upstream(_))),
            "{garbled:?}"
        );
    }

    #[test]
    fn off_wasm_nothing_is_sent() {
        let sent = Gitlab::call_tool(request("getProject", r#"{"project_id":"123"}"#));
        assert!(matches!(&sent, Err(ConnectorError::Upstream(m)) if m.contains("wasm32-wasip2")));
    }

    #[test]
    fn manifest_is_embedded() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["name"], "gitlab");
        assert_eq!(manifest["auth"]["schemes"][0]["type"], "oauth2");
        assert_eq!(manifest["version"], "0.2.0");
        assert_eq!(
            manifest["permissions"]["http"]["allow"][0],
            "https://gitlab.com"
        );
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
                "getProject",
                "listProjects",
                "createProject",
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
                "listMergeRequests",
                "getMergeRequest",
                "createMergeRequest",
                "acceptMergeRequest",
                "proposeChanges",
                "getCurrentUser",
                "listPipelines",
            ]
        );
    }
}
