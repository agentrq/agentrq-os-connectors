//! GitLab connector: projects, branches, commits, files, issues, merge
//! requests and pipelines through the GitLab REST API (v4). The host runs the
//! OAuth 2.0 sign-in, refreshes the token and adds `Authorization: Bearer …`
//! after each request leaves the guest: this code never sees it.
//!
//! Tool names and parameters follow the GitLab REST API; every call returns
//! the API's JSON as structured content.

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
struct GetFile {
    project_id: String,
    file_path: String,
    #[serde(rename = "ref")]
    ref_: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListRepositoryTree {
    project_id: String,
    path: Option<String>,
    #[serde(rename = "ref")]
    ref_: Option<String>,
    recursive: Option<bool>,
    per_page: Option<u32>,
    page: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct CreateFile {
    project_id: String,
    file_path: String,
    branch: String,
    commit_message: String,
    content: String,
    encoding: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateFile {
    project_id: String,
    file_path: String,
    branch: String,
    commit_message: String,
    content: String,
    encoding: Option<String>,
    last_commit_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeleteFile {
    project_id: String,
    file_path: String,
    branch: String,
    commit_message: String,
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
struct CreateIssueNote {
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
        let base = host::config("api_base").unwrap_or_else(|| DEFAULT_API_BASE.into());
        answer(&outgoing(&base, &request)?.send()?)
    }
}

connectors_sdk::export_connector!(Gitlab);

/// The project segment of a URL: a numeric ID passes through, a
/// `namespace/project` path is URL-encoded the way the API requires.
fn project(input: &str) -> String {
    encode(input)
}

/// The request a tool call makes to the API at `base`.
fn outgoing(base: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let api = base.trim_end_matches('/');
    let outgoing = match request.tool.as_str() {
        "getProject" => {
            let input: ProjectRef = request.json()?;
            Request::get(format!("{api}/projects/{}", project(&input.project_id)))
        }
        "listProjects" => {
            let input: ListProjects = request.json()?;
            Request::get(with_query(
                format!("{api}/projects"),
                &[
                    ("archived", input.archived.map(|b| b.to_string())),
                    ("visibility", input.visibility),
                    ("order_by", input.order_by),
                    ("sort", input.sort),
                    ("search", input.search),
                    ("owned", input.owned.map(|b| b.to_string())),
                    ("membership", input.membership.map(|b| b.to_string())),
                    ("starred", input.starred.map(|b| b.to_string())),
                    ("per_page", input.per_page.map(|n| n.to_string())),
                    ("page", input.page.map(|n| n.to_string())),
                ],
            ))
        }
        "createProject" => {
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
            Request::post(format!("{api}/projects")).json(&body)?
        }
        "listBranches" => {
            let input: ListBranches = request.json()?;
            Request::get(with_query(
                format!(
                    "{api}/projects/{}/repository/branches",
                    project(&input.project_id)
                ),
                &[
                    ("search", input.search),
                    ("per_page", input.per_page.map(|n| n.to_string())),
                    ("page", input.page.map(|n| n.to_string())),
                ],
            ))
        }
        "listCommits" => {
            let input: ListCommits = request.json()?;
            Request::get(with_query(
                format!(
                    "{api}/projects/{}/repository/commits",
                    project(&input.project_id)
                ),
                &[
                    ("ref_name", input.ref_name),
                    ("path", input.path),
                    ("per_page", input.per_page.map(|n| n.to_string())),
                    ("page", input.page.map(|n| n.to_string())),
                ],
            ))
        }
        "getCommit" => {
            let input: GetCommit = request.json()?;
            Request::get(format!(
                "{api}/projects/{}/repository/commits/{}",
                project(&input.project_id),
                encode(&input.sha)
            ))
        }
        "getFile" => {
            let input: GetFile = request.json()?;
            Request::get(with_query(
                format!(
                    "{api}/projects/{}/repository/files/{}",
                    project(&input.project_id),
                    encode(&input.file_path)
                ),
                &[("ref", input.ref_)],
            ))
        }
        "listRepositoryTree" => {
            let input: ListRepositoryTree = request.json()?;
            Request::get(with_query(
                format!(
                    "{api}/projects/{}/repository/tree",
                    project(&input.project_id)
                ),
                &[
                    ("path", input.path),
                    ("ref", input.ref_),
                    ("recursive", input.recursive.map(|b| b.to_string())),
                    ("per_page", input.per_page.map(|n| n.to_string())),
                    ("page", input.page.map(|n| n.to_string())),
                ],
            ))
        }
        "createFile" => {
            let input: CreateFile = request.json()?;
            let body = object(&[
                ("branch", Some(json!(input.branch))),
                ("commit_message", Some(json!(input.commit_message))),
                ("content", Some(json!(input.content))),
                (
                    "encoding",
                    input.encoding.as_ref().map(|encoding| json!(encoding)),
                ),
            ]);
            Request::post(format!(
                "{api}/projects/{}/repository/files/{}",
                project(&input.project_id),
                encode(&input.file_path)
            ))
            .json(&body)?
        }
        "updateFile" => {
            let input: UpdateFile = request.json()?;
            let body = object(&[
                ("branch", Some(json!(input.branch))),
                ("commit_message", Some(json!(input.commit_message))),
                ("content", Some(json!(input.content))),
                (
                    "encoding",
                    input.encoding.as_ref().map(|encoding| json!(encoding)),
                ),
                (
                    "last_commit_id",
                    input.last_commit_id.as_ref().map(|commit| json!(commit)),
                ),
            ]);
            Request::new(
                Method::Put,
                format!(
                    "{api}/projects/{}/repository/files/{}",
                    project(&input.project_id),
                    encode(&input.file_path)
                ),
            )
            .json(&body)?
        }
        "deleteFile" => {
            let input: DeleteFile = request.json()?;
            let body = object(&[
                ("branch", Some(json!(input.branch))),
                ("commit_message", Some(json!(input.commit_message))),
            ]);
            Request::new(
                Method::Delete,
                format!(
                    "{api}/projects/{}/repository/files/{}",
                    project(&input.project_id),
                    encode(&input.file_path)
                ),
            )
            .json(&body)?
        }
        "listIssues" => {
            let input: ListIssues = request.json()?;
            Request::get(with_query(
                format!("{api}/projects/{}/issues", project(&input.project_id)),
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
                "{api}/projects/{}/issues/{}",
                project(&input.project_id),
                input.issue_iid
            ))
        }
        "createIssue" => {
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
            Request::post(format!(
                "{api}/projects/{}/issues",
                project(&input.project_id)
            ))
            .json(&body)?
        }
        "updateIssue" => {
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
                    "{api}/projects/{}/issues/{}",
                    project(&input.project_id),
                    input.issue_iid
                ),
            )
            .json(&body)?
        }
        "createIssueNote" => {
            let input: CreateIssueNote = request.json()?;
            let body = object(&[("body", Some(json!(input.body)))]);
            Request::post(format!(
                "{api}/projects/{}/issues/{}/notes",
                project(&input.project_id),
                input.issue_iid
            ))
            .json(&body)?
        }
        "listMergeRequests" => {
            let input: ListMergeRequests = request.json()?;
            Request::get(with_query(
                format!(
                    "{api}/projects/{}/merge_requests",
                    project(&input.project_id)
                ),
                &[
                    ("state", input.state),
                    ("source_branch", input.source_branch),
                    ("target_branch", input.target_branch),
                    ("per_page", input.per_page.map(|n| n.to_string())),
                    ("page", input.page.map(|n| n.to_string())),
                ],
            ))
        }
        "getMergeRequest" => {
            let input: MergeRequestRef = request.json()?;
            Request::get(format!(
                "{api}/projects/{}/merge_requests/{}",
                project(&input.project_id),
                input.merge_request_iid
            ))
        }
        "createMergeRequest" => {
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
                "{api}/projects/{}/merge_requests",
                project(&input.project_id)
            ))
            .json(&body)?
        }
        "acceptMergeRequest" => {
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
                    "{api}/projects/{}/merge_requests/{}/merge",
                    project(&input.project_id),
                    input.merge_request_iid
                ),
            )
            .json(&body)?
        }
        "getCurrentUser" => Request::get(format!("{api}/user")),
        "listPipelines" => {
            let input: ListPipelines = request.json()?;
            Request::get(with_query(
                format!("{api}/projects/{}/pipelines", project(&input.project_id)),
                &[
                    ("status", input.status),
                    ("ref", input.ref_),
                    ("per_page", input.per_page.map(|n| n.to_string())),
                    ("page", input.page.map(|n| n.to_string())),
                ],
            ))
        }
        other => return Err(ConnectorError::NotFound(format!("no tool {other}"))),
    };
    Ok(outgoing.header(
        "user-agent",
        concat!("agentrq-gitlab-connector/", env!("CARGO_PKG_VERSION")),
    ))
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
/// a wrong project, a 401 an expired or missing token).
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
        outgoing("https://gitlab.com/api/v4/", &request(tool, body)).unwrap()
    }

    fn response(status: u16, body: &str) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    const API: &str = "https://gitlab.com/api/v4";

    fn header(request: &Request, name: &str) -> Option<String> {
        request
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    #[test]
    fn project_ids_encode() {
        assert_eq!(project("123"), "123");
        assert_eq!(project("group/project"), "group%2Fproject");
        assert_eq!(project("group/sub group/p"), "group%2Fsub%20group%2Fp");
    }

    #[test]
    fn plans_project_tools() {
        assert_eq!(
            planned("getProject", r#"{"project_id":"123"}"#).url,
            format!("{API}/projects/123")
        );
        assert_eq!(
            planned("getProject", r#"{"project_id":"group/my project"}"#).url,
            format!("{API}/projects/group%2Fmy%20project")
        );

        let listed = planned(
            "listProjects",
            r#"{"membership":true,"owned":true,"starred":false,"archived":false,"visibility":"private","order_by":"updated_at","sort":"desc","search":"demo","per_page":50,"page":2}"#,
        );
        assert_eq!(
            listed.url,
            format!(
                "{API}/projects?archived=false&visibility=private&order_by=updated_at&sort=desc&search=demo&owned=true&membership=true&starred=false&per_page=50&page=2"
            )
        );
        assert_eq!(planned("listProjects", "{}").url, format!("{API}/projects"));

        let created = planned(
            "createProject",
            r#"{"name":"new-project","path":"new-project","description":"A test","visibility":"private","initialize_with_readme":true}"#,
        );
        assert_eq!(created.url, format!("{API}/projects"));
        assert_eq!(created.method, Method::Post);
        let body: Value = serde_json::from_slice(&created.body).unwrap();
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
        let minimal = planned("createProject", r#"{"name":"new-project"}"#);
        let body: Value = serde_json::from_slice(&minimal.body).unwrap();
        assert_eq!(body, json!({ "name": "new-project" }));

        let branches = planned(
            "listBranches",
            r#"{"project_id":"group/p","search":"feat","per_page":30,"page":3}"#,
        );
        assert_eq!(
            branches.url,
            format!("{API}/projects/group%2Fp/repository/branches?search=feat&per_page=30&page=3")
        );
        assert_eq!(
            planned("listBranches", r#"{"project_id":"123"}"#).url,
            format!("{API}/projects/123/repository/branches")
        );

        let commits = planned(
            "listCommits",
            r#"{"project_id":"123","ref_name":"main","path":"src/lib.rs","per_page":10,"page":1}"#,
        );
        assert_eq!(
            commits.url,
            format!(
                "{API}/projects/123/repository/commits?ref_name=main&path=src%2Flib.rs&per_page=10&page=1"
            )
        );

        assert_eq!(
            planned("getCommit", r#"{"project_id":"123","sha":"feature/cool"}"#).url,
            format!("{API}/projects/123/repository/commits/feature%2Fcool")
        );
    }

    #[test]
    fn plans_file_tools() {
        assert_eq!(
            planned(
                "getFile",
                r#"{"project_id":"123","file_path":"src/lib.rs","ref":"main"}"#
            )
            .url,
            format!("{API}/projects/123/repository/files/src%2Flib.rs?ref=main")
        );
        assert_eq!(
            planned(
                "getFile",
                r#"{"project_id":"group/p","file_path":"docs/a b.md"}"#
            )
            .url,
            format!("{API}/projects/group%2Fp/repository/files/docs%2Fa%20b.md")
        );

        let tree = planned(
            "listRepositoryTree",
            r#"{"project_id":"123","path":"src","ref":"main","recursive":true,"per_page":25,"page":2}"#,
        );
        assert_eq!(
            tree.url,
            format!(
                "{API}/projects/123/repository/tree?path=src&ref=main&recursive=true&per_page=25&page=2"
            )
        );
        assert_eq!(
            planned("listRepositoryTree", r#"{"project_id":"123"}"#).url,
            format!("{API}/projects/123/repository/tree")
        );

        let created = planned(
            "createFile",
            r#"{"project_id":"123","file_path":"docs/a b.md","branch":"main","commit_message":"Add docs","content":"aGVsbG8=","encoding":"base64"}"#,
        );
        assert_eq!(created.method, Method::Post);
        assert_eq!(
            created.url,
            format!("{API}/projects/123/repository/files/docs%2Fa%20b.md")
        );
        let body: Value = serde_json::from_slice(&created.body).unwrap();
        assert_eq!(
            body,
            json!({
                "branch": "main",
                "commit_message": "Add docs",
                "content": "aGVsbG8=",
                "encoding": "base64",
            })
        );

        let updated = planned(
            "updateFile",
            r#"{"project_id":"123","file_path":"f.md","branch":"main","commit_message":"m","content":"Yg==","encoding":"base64","last_commit_id":"abc"}"#,
        );
        assert_eq!(updated.method, Method::Put);
        assert_eq!(
            updated.url,
            format!("{API}/projects/123/repository/files/f.md")
        );
        let body: Value = serde_json::from_slice(&updated.body).unwrap();
        assert_eq!(
            body,
            json!({
                "branch": "main",
                "commit_message": "m",
                "content": "Yg==",
                "encoding": "base64",
                "last_commit_id": "abc",
            })
        );
        let minimal_update = planned(
            "updateFile",
            r#"{"project_id":"123","file_path":"f.md","branch":"main","commit_message":"m","content":"Yg=="}"#,
        );
        let body: Value = serde_json::from_slice(&minimal_update.body).unwrap();
        assert_eq!(
            body,
            json!({ "branch": "main", "commit_message": "m", "content": "Yg==" })
        );

        let deleted = planned(
            "deleteFile",
            r#"{"project_id":"123","file_path":"old.md","branch":"main","commit_message":"Remove"}"#,
        );
        assert_eq!(deleted.method, Method::Delete);
        assert_eq!(
            deleted.url,
            format!("{API}/projects/123/repository/files/old.md")
        );
        let body: Value = serde_json::from_slice(&deleted.body).unwrap();
        assert_eq!(
            body,
            json!({ "branch": "main", "commit_message": "Remove" })
        );
    }

    #[test]
    fn plans_issue_tools() {
        let listed = planned(
            "listIssues",
            r#"{"project_id":"123","state":"opened","labels":"bug,ui","per_page":25,"page":1}"#,
        );
        assert_eq!(
            listed.url,
            format!("{API}/projects/123/issues?state=opened&labels=bug%2Cui&per_page=25&page=1")
        );

        assert_eq!(
            planned("getIssue", r#"{"project_id":"123","issue_iid":42}"#).url,
            format!("{API}/projects/123/issues/42")
        );

        let created = planned(
            "createIssue",
            r#"{"project_id":"123","title":"Bug","description":"It broke","labels":"bug","assignee_ids":[7]}"#,
        );
        assert_eq!(created.method, Method::Post);
        assert_eq!(created.url, format!("{API}/projects/123/issues"));
        let body: Value = serde_json::from_slice(&created.body).unwrap();
        assert_eq!(
            body,
            json!({
                "title": "Bug",
                "description": "It broke",
                "labels": "bug",
                "assignee_ids": [7],
            })
        );

        let updated = planned(
            "updateIssue",
            r#"{"project_id":"123","issue_iid":7,"state_event":"close"}"#,
        );
        assert_eq!(updated.method, Method::Put);
        assert_eq!(updated.url, format!("{API}/projects/123/issues/7"));
        let body: Value = serde_json::from_slice(&updated.body).unwrap();
        assert_eq!(body, json!({ "state_event": "close" }));

        let fully_updated = planned(
            "updateIssue",
            r#"{"project_id":"123","issue_iid":8,"title":"T","description":"B","labels":"l","assignee_ids":[1,2]}"#,
        );
        let body: Value = serde_json::from_slice(&fully_updated.body).unwrap();
        assert_eq!(
            body,
            json!({ "title": "T", "description": "B", "labels": "l", "assignee_ids": [1, 2] })
        );

        let noted = planned(
            "createIssueNote",
            r#"{"project_id":"123","issue_iid":7,"body":"Nice work"}"#,
        );
        assert_eq!(noted.method, Method::Post);
        assert_eq!(noted.url, format!("{API}/projects/123/issues/7/notes"));
        let body: Value = serde_json::from_slice(&noted.body).unwrap();
        assert_eq!(body, json!({ "body": "Nice work" }));
    }

    #[test]
    fn plans_merge_request_and_pipeline_tools() {
        let listed = planned(
            "listMergeRequests",
            r#"{"project_id":"123","state":"opened","source_branch":"feature","target_branch":"main","per_page":20,"page":2}"#,
        );
        assert_eq!(
            listed.url,
            format!(
                "{API}/projects/123/merge_requests?state=opened&source_branch=feature&target_branch=main&per_page=20&page=2"
            )
        );

        assert_eq!(
            planned(
                "getMergeRequest",
                r#"{"project_id":"123","merge_request_iid":3}"#
            )
            .url,
            format!("{API}/projects/123/merge_requests/3")
        );

        let created = planned(
            "createMergeRequest",
            r#"{"project_id":"123","title":"T","source_branch":"feature","target_branch":"main","description":"B"}"#,
        );
        assert_eq!(created.method, Method::Post);
        assert_eq!(created.url, format!("{API}/projects/123/merge_requests"));
        let body: Value = serde_json::from_slice(&created.body).unwrap();
        assert_eq!(
            body,
            json!({ "title": "T", "source_branch": "feature", "target_branch": "main", "description": "B" })
        );

        let merged = planned(
            "acceptMergeRequest",
            r#"{"project_id":"123","merge_request_iid":9,"merge_commit_message":"M","squash":true,"should_remove_source_branch":true}"#,
        );
        assert_eq!(merged.method, Method::Put);
        assert_eq!(
            merged.url,
            format!("{API}/projects/123/merge_requests/9/merge")
        );
        let body: Value = serde_json::from_slice(&merged.body).unwrap();
        assert_eq!(
            body,
            json!({ "merge_commit_message": "M", "squash": true, "should_remove_source_branch": true })
        );
        let plain_merge = planned(
            "acceptMergeRequest",
            r#"{"project_id":"123","merge_request_iid":9}"#,
        );
        let body: Value = serde_json::from_slice(&plain_merge.body).unwrap();
        assert_eq!(body, json!({}));

        assert_eq!(planned("getCurrentUser", "{}").url, format!("{API}/user"));

        let pipelines = planned(
            "listPipelines",
            r#"{"project_id":"123","status":"success","ref":"main","per_page":5,"page":2}"#,
        );
        assert_eq!(
            pipelines.url,
            format!("{API}/projects/123/pipelines?status=success&ref=main&per_page=5&page=2")
        );
    }

    #[test]
    fn sets_gitlab_headers() {
        let request = planned("getProject", r#"{"project_id":"123"}"#);
        let agent = header(&request, "user-agent").unwrap();
        assert!(
            agent.starts_with("agentrq-gitlab-connector/"),
            "unexpected user-agent {agent}"
        );
    }

    #[test]
    fn refuses_bad_input() {
        let missing = outgoing(DEFAULT_API_BASE, &request("getProject", "{}"));
        assert!(
            matches!(missing, Err(ConnectorError::InvalidInput(_))),
            "{missing:?}"
        );
        let missing_iid = outgoing(
            DEFAULT_API_BASE,
            &request("getIssue", r#"{"project_id":"123"}"#),
        );
        assert!(
            matches!(missing_iid, Err(ConnectorError::InvalidInput(_))),
            "{missing_iid:?}"
        );
        let unknown = outgoing(DEFAULT_API_BASE, &request("nope", "{}"));
        assert_eq!(
            unknown.unwrap_err(),
            ConnectorError::NotFound("no tool nope".into())
        );
    }

    #[test]
    fn maps_responses() {
        let ok = answer(&response(200, r#"{"username":"octocat"}"#)).unwrap();
        assert_eq!(ok.structured.as_deref(), Some(r#"{"username":"octocat"}"#));
        let created = answer(&response(201, r#"{"id":1}"#)).unwrap();
        assert_eq!(created.structured.as_deref(), Some(r#"{"id":1}"#));
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
        assert_eq!(
            manifest["permissions"]["http"]["allow"][0],
            "https://gitlab.com"
        );
        assert_eq!(manifest["tools"].as_array().unwrap().len(), 22);
    }
}
