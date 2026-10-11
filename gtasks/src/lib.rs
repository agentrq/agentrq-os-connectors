//! Google Tasks connector: task-shaped tools that finish in one call.
//!
//! This is deliberately not a 1:1 mapping of the Tasks API. Lists and tasks
//! are found by ID *or* title, so the connector resolves names internally
//! instead of pushing lookup choreography onto the agent. One tool may
//! therefore make several HTTP calls; see [`Api`].
//!
//! [`Transport`] is what makes those multi-call tools unit-testable: the
//! guest sends requests over HTTP, while tests answer from canned
//! responses.
//!
//! The host runs the OAuth 2.0 sign-in, refreshes the token and adds
//! `Authorization: Bearer …` after each request leaves the guest: this code
//! never sees it.
//!
//! A task's `due` is date-only: it is passed through as given, and the API
//! discards any time portion.

use connectors_sdk::http::{Method, Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::Deserialize;
use serde_json::{Value, json};

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://tasks.googleapis.com/tasks/v1";

/// The connector.
#[derive(Debug)]
pub struct Gtasks;

// ---------------------------------------------------------------------------
// Tool inputs. Field names are the API's camelCase parameter names; `list`
// and task references also accept human titles, resolved internally.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateTaskList {
    title: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameTaskList {
    list: Option<String>,
    title: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListRef {
    list: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListTasks {
    list: Option<String>,
    due_min: Option<String>,
    due_max: Option<String>,
    completed_min: Option<String>,
    completed_max: Option<String>,
    updated_min: Option<String>,
    show_completed: Option<bool>,
    show_hidden: Option<bool>,
    max_results: Option<u32>,
    page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewTask {
    list: Option<String>,
    title: String,
    notes: Option<String>,
    due: Option<String>,
    parent_task: Option<String>,
    after_task: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditTask {
    list: Option<String>,
    task: String,
    title: Option<String>,
    notes: Option<String>,
    due: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskRef {
    list: Option<String>,
    task: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MoveTask {
    list: Option<String>,
    task: String,
    to_list: Option<String>,
    parent_task: Option<String>,
    after_task: Option<String>,
}

// ---------------------------------------------------------------------------
// Failures
// ---------------------------------------------------------------------------

/// Why a tool call failed. Rendered as the tool's error message so the
/// agent can react (a 403 usually means a missing scope).
#[derive(Debug)]
struct ToolError {
    /// The API's HTTP status, when the failure is an API answer.
    status: Option<u16>,
    message: String,
}

impl ToolError {
    fn api(status: u16, body: &str) -> Self {
        Self {
            status: Some(status),
            message: format!("The API answered {status}: {body}"),
        }
    }

    fn input(message: impl Into<String>) -> Self {
        Self {
            status: None,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<ConnectorError> for ToolError {
    fn from(error: ConnectorError) -> Self {
        Self::input(error.to_string())
    }
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// Sends planned requests. The guest sends them over HTTP; tests answer
/// from canned responses.
trait Transport {
    fn send(&self, request: Request) -> Result<Response, ConnectorError>;
}

/// The real transport: HTTP inside the wasm32-wasip2 guest.
struct Guest;

impl Transport for Guest {
    fn send(&self, request: Request) -> Result<Response, ConnectorError> {
        request.send()
    }
}

// ---------------------------------------------------------------------------
// The API client
// ---------------------------------------------------------------------------

/// The Tasks API at one base URL. A tool method plans whatever requests its
/// outcome needs and sends them through the transport, resolving list and
/// task names to IDs along the way.
struct Api<T: Transport = Guest> {
    /// `{base}/lists`, the root of every endpoint.
    lists: String,
    transport: T,
}

impl<T: Transport> Api<T> {
    fn new(base: &str, transport: T) -> Self {
        Self {
            lists: format!("{}/lists", base.trim_end_matches('/')),
            transport,
        }
    }

    /// Sends one request and answers the parsed JSON body. An empty body
    /// (the API's 204 answers) becomes `Value::Null`.
    fn send(&self, request: Request) -> Result<Value, ToolError> {
        let response = self.transport.send(request)?;
        if !(200..300).contains(&response.status) {
            let body = String::from_utf8_lossy(&response.body);
            return Err(ToolError::api(response.status, &body));
        }
        if response.body.is_empty() {
            return Ok(Value::Null);
        }
        Ok(response.json()?)
    }

    /// GETs a URL, asking for JSON back.
    fn get(&self, url: &str) -> Result<Value, ToolError> {
        self.send(Request::get(url).header("accept", "application/json"))
    }

    /// POSTs a JSON body.
    fn post_json(&self, url: &str, body: &Value) -> Result<Value, ToolError> {
        self.send(
            Request::post(url)
                .header("accept", "application/json")
                .json(body)?,
        )
    }

    /// `PATCH`es a JSON body.
    fn patch_json(&self, url: &str, body: &Value) -> Result<Value, ToolError> {
        self.send(
            Request::new(Method::Patch, url)
                .header("accept", "application/json")
                .json(body)?,
        )
    }

    /// POSTs with no body (the move and clear endpoints take none).
    fn post_empty(&self, url: &str) -> Result<Value, ToolError> {
        self.send(Request::post(url).header("accept", "application/json"))
    }

    /// DELETEs a URL.
    fn delete(&self, url: &str) -> Result<Value, ToolError> {
        self.send(Request::new(Method::Delete, url).header("accept", "application/json"))
    }

    /// All task lists, for resolving a title to an ID.
    fn task_lists(&self) -> Result<Value, ToolError> {
        self.get(&with_query(
            &self.lists,
            &[("maxResults", Some(String::from("1000")))],
        ))
    }

    /// Resolves a task list given as an ID, a title, or nothing (the
    /// default list) to its ID. A title that matches nothing fails with
    /// the lists the user does have, so the agent can pick one.
    fn resolve_list(&self, list: Option<&str>) -> Result<String, ToolError> {
        match list {
            None | Some("@default") => Ok(String::from("@default")),
            Some(list) => {
                let body = self.task_lists()?;
                if let Some(items) = body.get("items").and_then(Value::as_array) {
                    for item in items {
                        let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                        let title = item
                            .get("title")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if id == list || title.eq_ignore_ascii_case(list) {
                            return Ok(String::from(id));
                        }
                    }
                    let names: Vec<&str> = items
                        .iter()
                        .filter_map(|item| item.get("title").and_then(Value::as_str))
                        .collect();
                    return Err(ToolError::input(format!(
                        "No task list named {list:?}; your lists are: {}",
                        names.join(", ")
                    )));
                }
                Err(ToolError::input(format!("No task list named {list:?}")))
            }
        }
    }

    /// Finds one task on a list given as an ID or a title: a direct fetch
    /// first (one call in the common case), then a title search.
    fn locate_task(&self, list_id: &str, task: &str) -> Result<Value, ToolError> {
        match self.get(&task_url(&self.lists, list_id, task)) {
            Ok(found) => Ok(found),
            Err(error) if error.status != Some(404) => Err(error),
            Err(_) => self.find_task_by_title(list_id, task),
        }
    }

    /// Finds one task on a list by title (case-insensitive), looking
    /// through open, completed, and hidden tasks.
    fn find_task_by_title(&self, list_id: &str, title: &str) -> Result<Value, ToolError> {
        let url = with_query(
            &format!("{}/{}/tasks", self.lists, list_segment(list_id)),
            &[
                ("showCompleted", Some(String::from("true"))),
                ("showHidden", Some(String::from("true"))),
                ("maxResults", Some(String::from("100"))),
            ],
        );
        let body = self.get(&url)?;
        let mut candidates = Vec::new();
        if let Some(items) = body.get("items").and_then(Value::as_array) {
            for item in items {
                if item
                    .get("title")
                    .and_then(Value::as_str)
                    .is_some_and(|t| t.eq_ignore_ascii_case(title))
                {
                    candidates.push(item.clone());
                }
            }
        }
        match candidates.as_slice() {
            [found] => Ok(found.clone()),
            [] => Err(ToolError::input(format!(
                "No task titled {title:?} on that list"
            ))),
            many => {
                let ids: Vec<&str> = many
                    .iter()
                    .filter_map(|task| task.get("id").and_then(Value::as_str))
                    .collect();
                Err(ToolError::input(format!(
                    "Several tasks are titled {title:?}; use an ID: {}",
                    ids.join(", ")
                )))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

impl Connector for Gtasks {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        let base = host::config("api_base").unwrap_or_else(|| DEFAULT_API_BASE.into());
        let api = Api::new(&base, Guest);
        // A failed tool call is still a tool answer: the agent sees what
        // happened and can react, instead of the call itself erroring.
        run(&api, &request).or_else(|error| Ok(ToolResponse::error(error.to_string())))
    }
}

connectors_sdk::export_connector!(Gtasks);

/// Runs one tool call: plans whatever requests the outcome needs, sends
/// them, and maps the answers to a tool result.
fn run<T: Transport>(api: &Api<T>, request: &ToolRequest) -> Result<ToolResponse, ToolError> {
    Ok(match request.tool.as_str() {
        "listTaskLists" => run_list_task_lists(api)?,
        "createTaskList" => run_create_task_list(api, request)?,
        "renameTaskList" => run_rename_task_list(api, request)?,
        "deleteTaskList" => run_delete_task_list(api, request)?,
        "listTasks" => run_list_tasks(api, request)?,
        "addTask" => run_add_task(api, request)?,
        "updateTask" => run_update_task(api, request)?,
        "completeTask" | "reopenTask" => run_set_task_status(api, request)?,
        "moveTask" => run_move_task(api, request)?,
        "deleteTask" => run_delete_task(api, request)?,
        "clearCompletedTasks" => run_clear_completed_tasks(api, request)?,
        other => return Err(ToolError::input(format!("no tool {other}"))),
    })
}

fn run_list_task_lists<T: Transport>(api: &Api<T>) -> Result<ToolResponse, ToolError> {
    let body = api.task_lists()?;
    Ok(ToolResponse::structured(&items_of(&body))?)
}

fn run_create_task_list<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: CreateTaskList = request.json()?;
    let body = api.post_json(&api.lists, &object(&[("title", Some(json!(input.title)))]))?;
    Ok(ToolResponse::structured(&body)?)
}

fn run_rename_task_list<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: RenameTaskList = request.json()?;
    let id = api.resolve_list(input.list.as_deref())?;
    let body = api.patch_json(
        &format!("{}/{}", api.lists, list_segment(&id)),
        &object(&[("title", Some(json!(input.title)))]),
    )?;
    Ok(ToolResponse::structured(&body)?)
}

fn run_delete_task_list<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: ListRef = request.json()?;
    let id = api.resolve_list(input.list.as_deref())?;
    api.delete(&format!("{}/{}", api.lists, list_segment(&id)))?;
    Ok(ToolResponse::structured(&json!({"deleted": true}))?)
}

fn run_list_tasks<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: ListTasks = request.json()?;
    let id = api.resolve_list(input.list.as_deref())?;
    // Open tasks are the useful default; asking for completed ones also
    // shows the ones first-party clients hide on clear.
    let show_completed = input.show_completed.unwrap_or(false);
    let show_hidden = input.show_hidden.unwrap_or(show_completed);
    let url = with_query(
        &format!("{}/{}/tasks", api.lists, list_segment(&id)),
        &[
            ("dueMin", input.due_min),
            ("dueMax", input.due_max),
            ("completedMin", input.completed_min),
            ("completedMax", input.completed_max),
            ("updatedMin", input.updated_min),
            ("showCompleted", Some(show_completed.to_string())),
            ("showHidden", Some(show_hidden.to_string())),
            ("maxResults", query(input.max_results)),
            ("pageToken", input.page_token),
        ],
    );
    let body = api.get(&url)?;
    Ok(ToolResponse::structured(&items_of(&body))?)
}

fn run_add_task<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: NewTask = request.json()?;
    let list_id = api.resolve_list(input.list.as_deref())?;
    let parent = optional_task_id(api, &list_id, input.parent_task.as_deref())?;
    let previous = optional_task_id(api, &list_id, input.after_task.as_deref())?;
    let url = with_query(
        &format!("{}/{}/tasks", api.lists, list_segment(&list_id)),
        &[("parent", parent), ("previous", previous)],
    );
    let body = api.post_json(
        &url,
        &object(&[
            ("title", Some(json!(input.title))),
            ("notes", field(input.notes)),
            ("due", field(input.due)),
        ]),
    )?;
    Ok(ToolResponse::structured(&body)?)
}

fn run_update_task<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: EditTask = request.json()?;
    if input.title.is_none() && input.notes.is_none() && input.due.is_none() {
        return Err(ToolError::input(
            "Nothing to update: give at least one of title, notes, due",
        ));
    }
    let list_id = api.resolve_list(input.list.as_deref())?;
    let task = api.locate_task(&list_id, &input.task)?;
    let body = api.patch_json(
        &task_url(&api.lists, &list_id, task_id(&task)),
        &object(&[
            ("title", field(input.title)),
            ("notes", field(input.notes)),
            ("due", field(input.due)),
        ]),
    )?;
    Ok(ToolResponse::structured(&body)?)
}

fn run_set_task_status<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: TaskRef = request.json()?;
    let list_id = api.resolve_list(input.list.as_deref())?;
    let task = api.locate_task(&list_id, &input.task)?;
    let status = if request.tool == "completeTask" {
        "completed"
    } else {
        "needsAction"
    };
    let body = api.patch_json(
        &task_url(&api.lists, &list_id, task_id(&task)),
        &object(&[("status", Some(json!(status)))]),
    )?;
    Ok(ToolResponse::structured(&body)?)
}

fn run_move_task<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: MoveTask = request.json()?;
    let list_id = api.resolve_list(input.list.as_deref())?;
    let task = api.locate_task(&list_id, &input.task)?;
    let dest_id = match input.to_list.as_deref() {
        Some(name) => api.resolve_list(Some(name))?,
        None => list_id.clone(),
    };
    let parent = optional_task_id(api, &dest_id, input.parent_task.as_deref())?;
    let previous = optional_task_id(api, &dest_id, input.after_task.as_deref())?;
    let destination = if dest_id == list_id {
        None
    } else {
        Some(dest_id)
    };
    let url = with_query(
        &format!("{}/move", task_url(&api.lists, &list_id, task_id(&task))),
        &[
            ("destinationTasklist", destination),
            ("parent", parent),
            ("previous", previous),
        ],
    );
    let body = api.post_empty(&url)?;
    Ok(ToolResponse::structured(&body)?)
}

fn run_delete_task<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: TaskRef = request.json()?;
    let list_id = api.resolve_list(input.list.as_deref())?;
    let task = api.locate_task(&list_id, &input.task)?;
    api.delete(&task_url(&api.lists, &list_id, task_id(&task)))?;
    Ok(ToolResponse::structured(&json!({"deleted": true}))?)
}

fn run_clear_completed_tasks<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: ListRef = request.json()?;
    let list_id = api.resolve_list(input.list.as_deref())?;
    api.post_empty(&format!("{}/{}/clear", api.lists, list_segment(&list_id)))?;
    Ok(ToolResponse::structured(&json!({"cleared": true}))?)
}

/// Resolves an optional task given as an ID or title to its ID.
fn optional_task_id<T: Transport>(
    api: &Api<T>,
    list_id: &str,
    task: Option<&str>,
) -> Result<Option<String>, ToolError> {
    match task {
        None => Ok(None),
        Some(task) => {
            let found = api.locate_task(list_id, task)?;
            Ok(Some(String::from(task_id(&found))))
        }
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// A list ID as a URL path segment: `@default` stays literal, anything else
/// is percent-encoded.
fn list_segment(list_id: &str) -> String {
    if list_id == "@default" {
        String::from("@default")
    } else {
        encode(list_id)
    }
}

/// The URL of one task inside its list.
fn task_url(lists: &str, list_id: &str, task_id: &str) -> String {
    format!(
        "{lists}/{}/tasks/{}",
        list_segment(list_id),
        encode(task_id)
    )
}

/// A task's ID from its JSON; the API always returns one.
fn task_id(task: &Value) -> &str {
    task.get("id").and_then(Value::as_str).unwrap_or_default()
}

/// The `items` collection of a list answer, or an empty array.
fn items_of(body: &Value) -> Value {
    body.get("items")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()))
}

/// A JSON body field that is present only when the input was.
fn field<T: Into<Value>>(value: Option<T>) -> Option<Value> {
    value.map(Into::into)
}

/// A query parameter that is present only when the input was.
fn query<T: ToString>(value: Option<T>) -> Option<String> {
    value.map(|value| value.to_string())
}

/// Appends the present query parameters to `url`, percent-encoded.
fn with_query(url: &str, query: &[(&str, Option<String>)]) -> String {
    let parts: Vec<String> = query
        .iter()
        .filter_map(|(name, value)| {
            value
                .as_ref()
                .map(|value| format!("{name}={}", encode(value)))
        })
        .collect();
    if parts.is_empty() {
        url.to_string()
    } else {
        format!("{url}?{}", parts.join("&"))
    }
}

/// Builds a JSON object from the fields that are present.
fn object(fields: &[(&str, Option<Value>)]) -> Value {
    let mut map = serde_json::Map::with_capacity(fields.len());
    for (name, value) in fields {
        if let Some(value) = value {
            map.insert((*name).to_string(), value.clone());
        }
    }
    Value::Object(map)
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
    use std::cell::RefCell;

    const BASE: &str = "https://tasks.test/tasks/v1";
    const LISTS: &str = "https://tasks.test/tasks/v1/lists";
    const LISTS_Q: &str = "https://tasks.test/tasks/v1/lists?maxResults=1000";

    const LISTS_BODY: &str =
        r#"{"items":[{"id":"abc","title":"Groceries"},{"id":"def","title":"Work"}]}"#;

    /// A canned API answer: the request's method and URL, then the status
    /// and body to answer with.
    type CannedAnswer = ((Method, String), (u16, &'static str));

    /// Canned API answers, keyed by method and URL, plus every request
    /// seen, so tests can assert on planned bodies.
    struct Fake {
        answers: Vec<CannedAnswer>,
        fail: bool,
        seen: RefCell<Vec<Request>>,
    }

    impl Fake {
        fn answering(answers: &[((Method, &str), u16, &'static str)]) -> Self {
            Self {
                answers: answers
                    .iter()
                    .map(|((method, url), status, body)| {
                        ((*method, String::from(*url)), (*status, *body))
                    })
                    .collect(),
                fail: false,
                seen: RefCell::new(Vec::new()),
            }
        }

        fn failing() -> Self {
            Self {
                answers: Vec::new(),
                fail: true,
                seen: RefCell::new(Vec::new()),
            }
        }

        fn answer_for(&self, method: Method, url: &str) -> Option<(u16, &'static str)> {
            self.answers
                .iter()
                .find(|((m, u), _)| *m == method && u == url)
                .map(|(_, answer)| *answer)
        }
    }

    impl Transport for Fake {
        fn send(&self, request: Request) -> Result<Response, ConnectorError> {
            if self.fail {
                return Err(ConnectorError::Upstream("transport boom".into()));
            }
            let (status, body) =
                self.answer_for(request.method, &request.url)
                    .ok_or_else(|| {
                        ConnectorError::Upstream(format!(
                            "no canned answer for {:?} {}",
                            request.method, request.url
                        ))
                    })?;
            self.seen.borrow_mut().push(request);
            Ok(Response {
                status,
                headers: Vec::new(),
                body: body.as_bytes().to_vec(),
            })
        }
    }

    fn client(answers: &[((Method, &str), u16, &'static str)]) -> Api<Fake> {
        Api::new(BASE, Fake::answering(answers))
    }

    fn lists_answer() -> ((Method, &'static str), u16, &'static str) {
        ((Method::Get, LISTS_Q), 200, LISTS_BODY)
    }

    fn request(tool: &str, body: &str) -> ToolRequest {
        ToolRequest {
            tool: tool.into(),
            body: body.into(),
        }
    }

    fn run_tool(api: &Api<Fake>, tool: &str, body: &str) -> Result<ToolResponse, ToolError> {
        run(api, &request(tool, body))
    }

    fn structured(response: &ToolResponse) -> &str {
        response.structured.as_deref().unwrap()
    }

    fn last_body(api: &Api<Fake>) -> Value {
        let seen = api.transport.seen.borrow();
        serde_json::from_slice(&seen.last().unwrap().body).unwrap()
    }

    #[test]
    fn lists_task_lists() {
        let api = client(&[lists_answer()]);
        let response = run_tool(&api, "listTaskLists", "{}").unwrap();
        assert_eq!(
            structured(&response),
            r#"[{"id":"abc","title":"Groceries"},{"id":"def","title":"Work"}]"#
        );
    }

    #[test]
    fn creates_renames_and_deletes_task_lists() {
        let api = client(&[
            lists_answer(),
            ((Method::Post, LISTS), 200, r#"{"id":"xyz","title":"Home"}"#),
            (
                (Method::Patch, "https://tasks.test/tasks/v1/lists/abc"),
                200,
                r#"{"id":"abc","title":"Food"}"#,
            ),
            (
                (Method::Delete, "https://tasks.test/tasks/v1/lists/def"),
                204,
                "",
            ),
        ]);

        let created = run_tool(&api, "createTaskList", r#"{"title":"Home"}"#).unwrap();
        assert_eq!(structured(&created), r#"{"id":"xyz","title":"Home"}"#);

        let renamed = run_tool(
            &api,
            "renameTaskList",
            r#"{"list":"Groceries","title":"Food"}"#,
        )
        .unwrap();
        assert_eq!(structured(&renamed), r#"{"id":"abc","title":"Food"}"#);
        assert_eq!(last_body(&api), json!({"title": "Food"}));

        let deleted = run_tool(&api, "deleteTaskList", r#"{"list":"def"}"#).unwrap();
        assert_eq!(structured(&deleted), r#"{"deleted":true}"#);
    }

    #[test]
    fn lists_tasks_with_defaults_and_filters() {
        let api = client(&[
            lists_answer(),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/@default/tasks?showCompleted=false&showHidden=false",
                ),
                200,
                r#"{"items":[{"id":"t1","title":"Buy milk"}]}"#,
            ),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/abc/tasks?dueMin=2026-10-01T00%3A00%3A00Z&showCompleted=true&showHidden=true&maxResults=50",
                ),
                200,
                r#"{"kind":"tasks#tasks"}"#,
            ),
        ]);

        let open = run_tool(&api, "listTasks", "{}").unwrap();
        assert_eq!(structured(&open), r#"[{"id":"t1","title":"Buy milk"}]"#);

        let filtered = run_tool(
            &api,
            "listTasks",
            r#"{"list":"groceries","dueMin":"2026-10-01T00:00:00Z","showCompleted":true,"maxResults":50}"#,
        )
        .unwrap();
        assert_eq!(structured(&filtered), "[]");
    }

    #[test]
    fn lists_default_list_without_a_lookup() {
        // No lists answer is canned: resolving "@default" must not fetch.
        let api = client(&[(
            (
                Method::Get,
                "https://tasks.test/tasks/v1/lists/@default/tasks?showCompleted=false&showHidden=false",
            ),
            200,
            r#"{"items":[]}"#,
        )]);
        let response = run_tool(&api, "listTasks", r#"{"list":"@default"}"#).unwrap();
        assert_eq!(structured(&response), "[]");
    }

    #[test]
    fn adds_tasks() {
        let api = client(&[
            lists_answer(),
            (
                (Method::Post, "https://tasks.test/tasks/v1/lists/abc/tasks"),
                200,
                r#"{"id":"t1","title":"Buy milk"}"#,
            ),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/abc/tasks/Dinner",
                ),
                404,
                "",
            ),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/abc/tasks?showCompleted=true&showHidden=true&maxResults=100",
                ),
                200,
                r#"{"items":[{"id":"p1","title":"Dinner"}]}"#,
            ),
            (
                (
                    Method::Post,
                    "https://tasks.test/tasks/v1/lists/abc/tasks?parent=p1",
                ),
                200,
                r#"{"id":"t2","title":"Dessert","parent":"p1"}"#,
            ),
        ]);

        let minimal = run_tool(&api, "addTask", r#"{"list":"abc","title":"Buy milk"}"#).unwrap();
        assert_eq!(structured(&minimal), r#"{"id":"t1","title":"Buy milk"}"#);
        assert_eq!(last_body(&api), json!({"title": "Buy milk"}));

        let subtask = run_tool(
            &api,
            "addTask",
            r#"{"list":"Groceries","title":"Dessert","notes":"with berries","due":"2026-10-12T00:00:00Z","parentTask":"Dinner"}"#,
        )
        .unwrap();
        assert_eq!(
            structured(&subtask),
            r#"{"id":"t2","parent":"p1","title":"Dessert"}"#
        );
        assert_eq!(
            last_body(&api),
            json!({
                "title": "Dessert",
                "notes": "with berries",
                "due": "2026-10-12T00:00:00Z",
            })
        );
    }

    #[test]
    fn updates_completes_and_reopens_tasks() {
        let api = client(&[
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/@default/tasks/t1",
                ),
                200,
                r#"{"id":"t1","title":"Buy milk"}"#,
            ),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/@default/tasks/Buy%20milk",
                ),
                404,
                "",
            ),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/@default/tasks?showCompleted=true&showHidden=true&maxResults=100",
                ),
                200,
                r#"{"items":[{"id":"t1","title":"Buy milk"}]}"#,
            ),
            (
                (
                    Method::Patch,
                    "https://tasks.test/tasks/v1/lists/@default/tasks/t1",
                ),
                200,
                r#"{"id":"t1","title":"Buy oat milk"}"#,
            ),
        ]);

        let updated = run_tool(
            &api,
            "updateTask",
            r#"{"task":"t1","title":"Buy oat milk","due":"2026-10-13T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(
            structured(&updated),
            r#"{"id":"t1","title":"Buy oat milk"}"#
        );
        assert_eq!(
            last_body(&api),
            json!({"title": "Buy oat milk", "due": "2026-10-13T00:00:00Z"})
        );

        let completed = run_tool(&api, "completeTask", r#"{"task":"Buy milk"}"#).unwrap();
        assert_eq!(
            structured(&completed),
            r#"{"id":"t1","title":"Buy oat milk"}"#
        );
        assert_eq!(last_body(&api), json!({"status": "completed"}));

        let reopened = run_tool(&api, "reopenTask", r#"{"task":"t1"}"#).unwrap();
        assert_eq!(
            structured(&reopened),
            r#"{"id":"t1","title":"Buy oat milk"}"#
        );
        assert_eq!(last_body(&api), json!({"status": "needsAction"}));

        let nothing = run_tool(&api, "updateTask", r#"{"task":"t1"}"#);
        assert!(
            matches!(&nothing, Err(error) if error.message.contains("Nothing to update")),
            "{nothing:?}"
        );
    }

    #[test]
    fn moves_tasks() {
        let api = client(&[
            lists_answer(),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/abc/tasks/Report",
                ),
                404,
                "",
            ),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/abc/tasks?showCompleted=true&showHidden=true&maxResults=100",
                ),
                200,
                r#"{"items":[{"id":"t9","title":"Report"}]}"#,
            ),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/def/tasks/Q3",
                ),
                404,
                "",
            ),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/def/tasks?showCompleted=true&showHidden=true&maxResults=100",
                ),
                200,
                r#"{"items":[{"id":"p7","title":"Q3"}]}"#,
            ),
            (
                (
                    Method::Post,
                    "https://tasks.test/tasks/v1/lists/abc/tasks/t9/move?destinationTasklist=def&parent=p7",
                ),
                200,
                r#"{"id":"t9","title":"Report"}"#,
            ),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/@default/tasks/t1",
                ),
                200,
                r#"{"id":"t1","title":"One"}"#,
            ),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/@default/tasks/t2",
                ),
                200,
                r#"{"id":"t2","title":"Two"}"#,
            ),
            (
                (
                    Method::Post,
                    "https://tasks.test/tasks/v1/lists/@default/tasks/t1/move?previous=t2",
                ),
                200,
                r#"{"id":"t1","title":"One"}"#,
            ),
            (
                (
                    Method::Post,
                    "https://tasks.test/tasks/v1/lists/@default/tasks/t1/move",
                ),
                200,
                r#"{"id":"t1","title":"One"}"#,
            ),
        ]);

        let moved = run_tool(
            &api,
            "moveTask",
            r#"{"list":"Groceries","task":"Report","toList":"Work","parentTask":"Q3"}"#,
        )
        .unwrap();
        assert_eq!(structured(&moved), r#"{"id":"t9","title":"Report"}"#);

        let reordered = run_tool(&api, "moveTask", r#"{"task":"t1","afterTask":"t2"}"#).unwrap();
        assert_eq!(structured(&reordered), r#"{"id":"t1","title":"One"}"#);

        let noop = run_tool(&api, "moveTask", r#"{"task":"t1"}"#).unwrap();
        assert_eq!(structured(&noop), r#"{"id":"t1","title":"One"}"#);
    }

    #[test]
    fn deletes_tasks_and_clears_completed() {
        let api = client(&[
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/@default/tasks/Buy%20milk",
                ),
                404,
                "",
            ),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/@default/tasks?showCompleted=true&showHidden=true&maxResults=100",
                ),
                200,
                r#"{"items":[{"id":"t1","title":"Buy milk"}]}"#,
            ),
            (
                (
                    Method::Delete,
                    "https://tasks.test/tasks/v1/lists/@default/tasks/t1",
                ),
                204,
                "",
            ),
            (
                (
                    Method::Post,
                    "https://tasks.test/tasks/v1/lists/@default/clear",
                ),
                204,
                "",
            ),
        ]);

        let deleted = run_tool(&api, "deleteTask", r#"{"task":"Buy milk"}"#).unwrap();
        assert_eq!(structured(&deleted), r#"{"deleted":true}"#);

        let cleared = run_tool(&api, "clearCompletedTasks", "{}").unwrap();
        assert_eq!(structured(&cleared), r#"{"cleared":true}"#);
    }

    #[test]
    fn reports_unknown_lists_by_name() {
        let api = client(&[lists_answer()]);
        let missing = run_tool(&api, "renameTaskList", r#"{"list":"Nope","title":"x"}"#);
        assert!(
            matches!(&missing, Err(error) if error.message.contains(r#"No task list named "Nope""#)
                && error.message.contains("Groceries, Work")),
            "{missing:?}"
        );

        let empty = client(&[((Method::Get, LISTS_Q), 200, "{}")]);
        let missing = run_tool(&empty, "renameTaskList", r#"{"list":"Nope","title":"x"}"#);
        assert!(
            matches!(&missing, Err(error) if error.message == r#"No task list named "Nope""#),
            "{missing:?}"
        );
    }

    #[test]
    fn reports_unknown_or_ambiguous_tasks() {
        let api = client(&[
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/@default/tasks/Missing",
                ),
                404,
                "",
            ),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/@default/tasks/Dupe",
                ),
                404,
                "",
            ),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/@default/tasks?showCompleted=true&showHidden=true&maxResults=100",
                ),
                200,
                r#"{"kind":"tasks#tasks"}"#,
            ),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/@default/tasks/t1",
                ),
                403,
                "forbidden",
            ),
        ]);

        let missing = run_tool(&api, "completeTask", r#"{"task":"Missing"}"#);
        assert!(
            matches!(&missing, Err(error) if error.message.contains(r#"No task titled "Missing""#)),
            "{missing:?}"
        );

        let dupe_api = client(&[
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/@default/tasks/Dupe",
                ),
                404,
                "",
            ),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/@default/tasks?showCompleted=true&showHidden=true&maxResults=100",
                ),
                200,
                r#"{"items":[{"id":"t1","title":"Dupe"},{"id":"t2","title":"Dupe"}]}"#,
            ),
        ]);
        let dupe = run_tool(&dupe_api, "completeTask", r#"{"task":"Dupe"}"#);
        assert!(
            matches!(&dupe, Err(error) if error.message.contains("Several tasks are titled")
                && error.message.contains("t1, t2")),
            "{dupe:?}"
        );

        let forbidden = run_tool(&api, "completeTask", r#"{"task":"t1"}"#);
        assert!(
            matches!(&forbidden, Err(error) if error.message == "The API answered 403: forbidden"),
            "{forbidden:?}"
        );
    }

    #[test]
    fn surfaces_write_failures() {
        let api = client(&[
            lists_answer(),
            (
                (Method::Patch, "https://tasks.test/tasks/v1/lists/abc"),
                403,
                "forbidden",
            ),
            (
                (Method::Post, "https://tasks.test/tasks/v1/lists/abc/tasks"),
                403,
                "forbidden",
            ),
            (
                (
                    Method::Get,
                    "https://tasks.test/tasks/v1/lists/@default/tasks/t1",
                ),
                200,
                r#"{"id":"t1"}"#,
            ),
            (
                (
                    Method::Patch,
                    "https://tasks.test/tasks/v1/lists/@default/tasks/t1",
                ),
                403,
                "forbidden",
            ),
        ]);

        for (tool, body) in [
            ("renameTaskList", r#"{"list":"abc","title":"x"}"#),
            ("addTask", r#"{"list":"abc","title":"x"}"#),
            ("updateTask", r#"{"task":"t1","title":"x"}"#),
            ("completeTask", r#"{"task":"t1"}"#),
        ] {
            let failed = run_tool(&api, tool, body);
            assert!(
                matches!(&failed, Err(error) if error.message == "The API answered 403: forbidden"),
                "{tool}: {failed:?}"
            );
        }
    }

    #[test]
    fn surfaces_transport_and_api_failures() {
        let failing = Api::new(BASE, Fake::failing());
        let failed = run(&failing, &request("listTaskLists", "{}"));
        assert!(
            matches!(&failed, Err(error) if error.message == "transport boom" && error.status.is_none()),
            "{failed:?}"
        );

        let garbled = client(&[((Method::Get, LISTS_Q), 200, "<html>")]);
        let parsed = run(&garbled, &request("listTaskLists", "{}"));
        assert!(
            matches!(&parsed, Err(error) if error.status.is_none()),
            "{parsed:?}"
        );

        let uncanned = client(&[]);
        let missing = uncanned.get("https://tasks.test/tasks/v1/nope");
        assert!(
            matches!(&missing, Err(error) if error.message.contains("no canned answer")),
            "{missing:?}"
        );
    }

    #[test]
    fn refuses_bad_input() {
        let api = client(&[]);
        let malformed = run(&api, &request("addTask", "{oops"));
        assert!(
            matches!(&malformed, Err(error) if error.status.is_none()),
            "{malformed:?}"
        );

        let missing = run(&api, &request("addTask", "{}"));
        assert!(missing.is_err(), "{missing:?}");

        let unknown = run(&api, &request("nope", "{}"));
        assert!(
            matches!(&unknown, Err(error) if error.message == "no tool nope"),
            "{unknown:?}"
        );
    }

    #[test]
    fn maps_tool_failures_through_call_tool() {
        let unknown = Gtasks::call_tool(request("nope", "{}")).unwrap();
        assert_eq!(unknown.structured, None);
    }

    #[test]
    fn off_wasm_nothing_is_sent() {
        // Failures surface as tool errors, not call errors.
        let sent = Gtasks::call_tool(request("listTaskLists", "{}")).unwrap();
        assert_eq!(sent.structured, None);

        let guest = Guest.send(Request::get("https://tasks.test/"));
        assert!(matches!(&guest, Err(ConnectorError::Upstream(m)) if m.contains("wasm32-wasip2")));
    }

    #[test]
    fn trims_a_trailing_slash_off_the_base() {
        let api = Api::new("https://tasks.test/tasks/v1/", Fake::answering(&[]));
        assert_eq!(api.lists, LISTS);
    }

    #[test]
    fn manifest_is_embedded() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["name"], "gtasks");
        assert_eq!(manifest["auth"]["schemes"][0]["type"], "oauth2");
        assert_eq!(
            manifest["auth"]["schemes"][0]["scopes"][0],
            "https://www.googleapis.com/auth/tasks"
        );
        assert_eq!(
            manifest["permissions"]["http"]["allow"][0],
            "https://tasks.googleapis.com"
        );
        let tools: Vec<&str> = manifest["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            tools,
            [
                "listTaskLists",
                "createTaskList",
                "renameTaskList",
                "deleteTaskList",
                "listTasks",
                "addTask",
                "updateTask",
                "completeTask",
                "reopenTask",
                "moveTask",
                "deleteTask",
                "clearCompletedTasks",
            ]
        );
    }
}
