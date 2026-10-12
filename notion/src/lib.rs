//! Notion connector: outcome-oriented tools that finish in one call.
//!
//! This is deliberately not a 1:1 mapping of the Notion API. Each tool
//! completes an outcome in a single call: creating a page with its properties
//! and Markdown content, querying a data source by title with plain-value
//! filters, or reading a page's properties as simple values. Pages and data
//! sources are found by title *or* id, resolved inside the connector instead
//! of pushing lookups onto the agent. One tool may therefore make several
//! HTTP calls; see the plan functions below.
//!
//! [`Sender`] is what makes those multi-call tools unit-testable: the guest
//! sends requests over HTTP, while tests answer from canned responses. It is
//! `&mut dyn FnMut` (not `impl FnMut`) so every tool shares one instantiation
//! and the 100% function-coverage gate stays reachable.
//!
//! The host runs the OAuth 2.0 sign-in, refreshes the token and adds
//! `Authorization: Bearer …` after each request leaves the guest: this code
//! never sees it. Every request pins `Notion-Version: 2026-03-11`, the
//! current API version: databases are queried as data sources, trashed pages
//! report `in_trash`, and block appends default to the end.

use connectors_sdk::http::{Method, Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::Deserialize;
use serde_json::{Map, Value, json};

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://api.notion.com";

/// The pinned Notion API version (current as of 2026-03-11).
pub const NOTION_VERSION: &str = "2026-03-11";

/// The connector.
#[derive(Debug)]
pub struct Notion;

/// Sends planned requests: the real `Request::send` in production, a stub in
/// tests.
type Sender<'a> = &'a mut dyn FnMut(&Request) -> Result<Response, ConnectorError>;

/// Why a tool call failed. Surfaced to the agent as the tool's error message,
/// so a failed tool call is still an answer the agent can react to.
#[derive(Debug, PartialEq, Eq)]
struct ToolError(String);

impl From<ConnectorError> for ToolError {
    fn from(error: ConnectorError) -> Self {
        Self(error.to_string())
    }
}

// ---------------------------------------------------------------------------
// Tool inputs. Field names are the tools' camelCase parameter names.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Search {
    query: String,
    filter: Option<String>,
    page_size: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetPage {
    page_id: String,
    include_content: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParentRef {
    data_source: Option<String>,
    page: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatePage {
    parent: ParentRef,
    properties: Option<Value>,
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdatePage {
    page_id: String,
    properties: Option<Value>,
    trash: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MovePage {
    page_id: String,
    parent: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueryDataSource {
    data_source: String,
    filter: Option<Value>,
    sorts: Option<Value>,
    page_size: Option<u32>,
    start_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DataSourceRef {
    data_source: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PropertyDef {
    name: String,
    #[serde(rename = "type")]
    property_type: String,
    options: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateDataSource {
    parent: String,
    title: String,
    properties: Vec<PropertyDef>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppendContent {
    page_id: String,
    markdown: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListComments {
    page_id: String,
    page_size: Option<u32>,
    start_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateComment {
    page_id: String,
    text: String,
    discussion_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

impl Connector for Notion {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        Self::call_with(request, &mut |request| Ok(request.clone().send()?))
    }
}

impl Notion {
    /// Dispatches a tool call, sending requests through `send`: the real
    /// `Request::send` in production, a stub in tests so multi-call tools are
    /// covered end to end without a network.
    fn call_with(request: ToolRequest, send: Sender<'_>) -> Result<ToolResponse, ConnectorError> {
        let base = host::config("api_base").unwrap_or_else(|| DEFAULT_API_BASE.into());
        let api = base.trim_end_matches('/');
        // Every request carries the pinned API version; the host adds auth.
        let mut send = |planned: &Request| send(&notion_headers(planned));
        // Every arm answers `Result<ToolResponse, ToolError>`: a failure
        // becomes a tool error the agent can react to, not a failed call.
        let outcome: Result<ToolResponse, ToolError> = match request.tool.as_str() {
            "searchWorkspace" => search(api, &request, &mut send),
            "getPage" => get_page(api, &request, &mut send),
            "createPage" => create_page(api, &request, &mut send),
            "updatePage" => update_page(api, &request, &mut send),
            "movePage" => move_page(api, &request, &mut send),
            "queryDataSource" => query_data_source(api, &request, &mut send),
            "getDataSource" => get_data_source(api, &request, &mut send),
            "createDataSource" => create_data_source(api, &request, &mut send),
            "appendContent" => append_content(api, &request, &mut send),
            "listComments" => list_comments(api, &request, &mut send),
            "createComment" => create_comment(api, &request, &mut send),
            "getCurrentUser" => get_current_user(api, &request, &mut send),
            other => return Err(ConnectorError::NotFound(format!("no tool {other}"))),
        };
        Ok(outcome.unwrap_or_else(|error| ToolResponse::error(error.0)))
    }
}

connectors_sdk::export_connector!(Notion);

/// The headers every Notion request carries: the pinned API version, a JSON
/// accept header, and a user agent naming this connector.
fn notion_headers(planned: &Request) -> Request {
    planned
        .clone()
        .header("notion-version", NOTION_VERSION)
        .header("accept", "application/json")
        .header(
            "user-agent",
            concat!("agentrq-notion-connector/", env!("CARGO_PKG_VERSION")),
        )
}

/// The response of one step of a multi-call tool: 2xx continues the tool,
/// anything else becomes a failure message naming the failed step.
fn step_ok(response: &Response, step: &str) -> Result<(), ToolError> {
    if (200..300).contains(&response.status) {
        Ok(())
    } else {
        let body = String::from_utf8_lossy(&response.body);
        Err(ToolError(format!(
            "{step} failed: the API answered {}: {body}",
            response.status
        )))
    }
}

/// Sends one planned request, checks the step succeeded, and answers the
/// parsed JSON body.
fn call_api(send: Sender<'_>, request: &Request, step: &str) -> Result<Value, ToolError> {
    let response = send(request)?;
    step_ok(&response, step)?;
    Ok(response.json().map_err(ConnectorError::from)?)
}

/// Plans a JSON POST.
fn post_json(url: &str, body: &Value) -> Request {
    json_request(Method::Post, url, body)
}

/// Plans a JSON PATCH.
fn patch_json(url: &str, body: &Value) -> Request {
    json_request(Method::Patch, url, body)
}

/// A request with a JSON body, set directly instead of going through
/// `.json()`: serializing a `Value` cannot fail, so there is no fallible
/// step for the coverage gate to trip on.
fn json_request(method: Method, url: &str, body: &Value) -> Request {
    let mut planned = Request::new(method, url);
    planned
        .headers
        .push(("content-type".to_string(), "application/json".to_string()));
    planned.body = serde_json::to_vec(body).unwrap_or_default();
    planned
}

/// Percent-encodes one query-string or path value.
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

/// Appends the `Some` query parameters to `url`, percent-encoding each value.
fn with_query(url: &str, params: &[(&str, Option<String>)]) -> String {
    let query: Vec<String> = params
        .iter()
        .filter_map(|(name, value)| {
            value
                .as_deref()
                .map(|value| format!("{name}={}", encode(value)))
        })
        .collect();
    if query.is_empty() {
        url.to_string()
    } else {
        format!("{url}?{}", query.join("&"))
    }
}

/// A JSON object from `Some` fields; `None` fields are left out.
fn object(fields: &[(&str, Option<Value>)]) -> Value {
    let mut map = Map::with_capacity(fields.len());
    for (name, value) in fields {
        if let Some(value) = value {
            map.insert((*name).to_string(), value.clone());
        }
    }
    Value::Object(map)
}

// ---------------------------------------------------------------------------
// Search and title resolution
// ---------------------------------------------------------------------------

/// Whether `text` already looks like a Notion id (32 hex digits, dashes
/// optional), in which case it needs no resolution.
fn is_id(text: &str) -> bool {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    hex.len() == 32 && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// The display title of a search hit: a page's title property, or a data
/// source's title rich text.
fn hit_title(hit: &Value) -> Option<String> {
    match hit.get("object").and_then(Value::as_str) {
        Some("page") => hit
            .get("properties")
            .and_then(Value::as_object)
            .and_then(page_title),
        Some("data_source") => {
            let title = plain_text(hit.get("title").unwrap_or(&Value::Null));
            (!title.is_empty()).then_some(title)
        }
        _ => None,
    }
}

/// Searches the workspace, answering the raw hits.
fn search_raw(
    api: &str,
    send: Sender<'_>,
    query: &str,
    filter: Option<&str>,
    page_size: Option<u32>,
) -> Result<Vec<Value>, ToolError> {
    let filter_value = match filter {
        Some("page") => Some("page"),
        Some("dataSource") => Some("data_source"),
        _ => None,
    };
    let body = object(&[
        ("query", Some(json!(query))),
        (
            "filter",
            filter_value.map(|value| json!({"property": "object", "value": value})),
        ),
        ("page_size", page_size.map(|n| json!(n))),
        (
            "sort",
            Some(json!({"timestamp": "last_edited_time", "direction": "descending"})),
        ),
    ]);
    let answered = call_api(
        send,
        &post_json(&format!("{api}/v1/search"), &body),
        "searching the workspace",
    )?;
    Ok(answered
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// Picks the one hit a title refers to: an exact case-insensitive title match
/// wins; an id passes through untouched. No match fails listing what the
/// search found; several fail listing the candidates so the agent can pick.
fn pick_hit(hits: &[Value], name: &str, kind: &str) -> Result<Value, ToolError> {
    if is_id(name) {
        return Ok(json!({"id": name}));
    }
    let mut exact = Vec::new();
    for hit in hits {
        if hit_title(hit).is_some_and(|title| title.eq_ignore_ascii_case(name)) {
            exact.push(hit);
        }
    }
    match exact.as_slice() {
        [hit] => Ok((*hit).clone()),
        [] => {
            let titles: Vec<String> = hits.iter().filter_map(hit_title).collect();
            Err(ToolError(if titles.is_empty() {
                format!("No {kind} matches {name:?}")
            } else {
                format!(
                    "No {kind} is titled {name:?}; the search found: {}",
                    titles.join(", ")
                )
            }))
        }
        many => {
            let ids: Vec<&str> = many
                .iter()
                .filter_map(|hit| hit.get("id").and_then(Value::as_str))
                .collect();
            Err(ToolError(format!(
                "Several {kind}s are titled {name:?}; use an id: {}",
                ids.join(", ")
            )))
        }
    }
}

/// Resolves a data source given as a title or an id, answering its id.
fn resolve_data_source(api: &str, send: Sender<'_>, name: &str) -> Result<String, ToolError> {
    if is_id(name) {
        return Ok(name.to_string());
    }
    let hits = search_raw(api, send, name, Some("dataSource"), Some(10))?;
    let hit = pick_hit(&hits, name, "data source")?;
    hit.get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ToolError("the API returned a data source without an id".to_string()))
}

/// Resolves a page given as a title or an id, answering its id.
fn resolve_page(api: &str, send: Sender<'_>, name: &str) -> Result<String, ToolError> {
    if is_id(name) {
        return Ok(name.to_string());
    }
    let hits = search_raw(api, send, name, Some("page"), Some(10))?;
    let hit = pick_hit(&hits, name, "page")?;
    hit.get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ToolError("the API returned a page without an id".to_string()))
}

/// A search hit in the shape agents read.
fn readable_hit(hit: &Value) -> Value {
    let object_type = hit.get("object").and_then(Value::as_str).unwrap_or("");
    let kind = match object_type {
        "data_source" => "dataSource",
        _ => "page",
    };
    object(&[
        ("id", hit.get("id").cloned()),
        ("title", hit_title(hit).map(Value::String)),
        ("type", Some(json!(kind))),
        ("url", hit.get("url").cloned()),
        ("lastEditedTime", hit.get("last_edited_time").cloned()),
    ])
}

/// Runs `search`: one workspace search mapped to compact hits.
fn search(api: &str, request: &ToolRequest, send: Sender<'_>) -> Result<ToolResponse, ToolError> {
    let input: Search = request.json()?;
    let filter = input.filter.as_deref().filter(|f| *f != "all");
    let hits = search_raw(api, send, &input.query, filter, input.page_size)?;
    ToolResponse::structured(&json!({
        "results": hits.iter().map(readable_hit).collect::<Vec<_>>(),
    }))
    .map_err(ToolError::from)
}

// ---------------------------------------------------------------------------
// Properties: plain values in, plain values out
// ---------------------------------------------------------------------------

/// The plain text of a rich-text array.
fn plain_text(rich_text: &Value) -> String {
    rich_text
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("plain_text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// A page's title: the plain text of its `title` property.
fn page_title(properties: &Map<String, Value>) -> Option<String> {
    properties
        .values()
        .find(|prop| prop.get("type").and_then(Value::as_str) == Some("title"))
        .map(|prop| plain_text(prop.get("title").unwrap_or(&Value::Null)))
        .filter(|title| !title.is_empty())
}

/// One property in the shape agents read: the API's typed wrapper collapsed
/// to a plain value.
fn simplify_property(prop: &Value) -> Value {
    let prop_type = prop.get("type").and_then(Value::as_str).unwrap_or("");
    simplify_value(prop_type, prop.get(prop_type).unwrap_or(&Value::Null))
}

/// A property value collapsed to a plain value: text to strings, selects to
/// their option names, dates to date strings, people to id/name pairs.
/// Unknown types pass through untouched.
fn simplify_value(prop_type: &str, value: &Value) -> Value {
    match prop_type {
        "title" | "rich_text" => json!(plain_text(value)),
        "number" | "checkbox" | "url" | "email" | "phone_number" => value.clone(),
        "select" | "status" => value.get("name").cloned().unwrap_or(Value::Null),
        "multi_select" => json!(names_of(value)),
        "date" => match (
            value.get("start"),
            value.get("end").filter(|end| !end.is_null()),
        ) {
            (Some(start), Some(end)) => json!({"start": start, "end": end}),
            (Some(start), None) => start.clone(),
            _ => Value::Null,
        },
        "people" => json!(simplify_users(value)),
        "relation" => json!(ids_of(value)),
        "files" => json!(
            value
                .as_array()
                .map(|files| files.iter().map(simplify_file).collect::<Vec<_>>())
                .unwrap_or_default()
        ),
        "formula" => {
            let inner = value.get("type").and_then(Value::as_str).unwrap_or("");
            simplify_value(inner, value.get(inner).unwrap_or(&Value::Null))
        }
        "rollup" => {
            let inner = value.get("type").and_then(Value::as_str).unwrap_or("");
            if inner == "array" {
                json!(
                    value
                        .get("array")
                        .and_then(Value::as_array)
                        .map(|items| items.iter().map(simplify_property).collect::<Vec<_>>())
                        .unwrap_or_default()
                )
            } else {
                simplify_value(inner, value.get(inner).unwrap_or(&Value::Null))
            }
        }
        "created_time" | "last_edited_time" => value.clone(),
        "created_by" | "last_edited_by" => simplify_user(value),
        "unique_id" => match (value.get("prefix"), value.get("number")) {
            (Some(prefix), Some(number)) => {
                json!(format!("{}-{number}", prefix.as_str().unwrap_or("")))
            }
            _ => Value::Null,
        },
        _ => value.clone(),
    }
}

/// The option names of a multi-select value.
fn names_of(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|options| {
            options
                .iter()
                .filter_map(|option| option.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The page ids of a relation value.
fn ids_of(value: &Value) -> Vec<Value> {
    value
        .as_array()
        .map(|pages| {
            pages
                .iter()
                .filter_map(|page| page.get("id").cloned())
                .collect()
        })
        .unwrap_or_default()
}

/// The id/name pairs of a people value.
fn simplify_users(value: &Value) -> Vec<Value> {
    value
        .as_array()
        .map(|users| users.iter().map(simplify_user).collect())
        .unwrap_or_default()
}

/// One user in the shape agents read.
fn simplify_user(user: &Value) -> Value {
    object(&[
        ("id", user.get("id").cloned()),
        ("name", user.get("name").cloned()),
    ])
}

/// One file in the shape agents read.
fn simplify_file(file: &Value) -> Value {
    let url = file
        .get("external")
        .and_then(|external| external.get("url"))
        .or_else(|| file.get("file").and_then(|file| file.get("url")));
    object(&[("name", file.get("name").cloned()), ("url", url.cloned())])
}

/// A page in the shape agents read: plain-value properties plus the metadata
/// agents actually use.
fn simplify_page(page: &Value) -> Value {
    let properties = page.get("properties").and_then(Value::as_object);
    let simplified = properties
        .map(|props| {
            props
                .iter()
                .map(|(name, prop)| (name.clone(), simplify_property(prop)))
                .collect::<Map<String, Value>>()
        })
        .unwrap_or_default();
    object(&[
        ("id", page.get("id").cloned()),
        ("title", properties.and_then(page_title).map(Value::String)),
        ("url", page.get("url").cloned()),
        ("parent", page.get("parent").cloned()),
        ("createdTime", page.get("created_time").cloned()),
        ("lastEditedTime", page.get("last_edited_time").cloned()),
        ("inTrash", page.get("in_trash").cloned()),
        ("properties", Some(Value::Object(simplified))),
    ])
}

/// Property name → schema type for a data source response.
fn schema_types(schema: &Value) -> HashMap<String, String> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| {
            props
                .iter()
                .filter_map(|(name, prop)| {
                    prop.get("type")
                        .and_then(Value::as_str)
                        .map(|kind| (name.clone(), kind.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The API's `properties` payload for agent-given plain values. `schema` is
/// the data source response when the page lives in one; without it only the
/// page `title` can be written. `None` values are skipped.
fn map_properties(schema: Option<&Value>, values: &Value) -> Result<Value, ToolError> {
    let values = values
        .as_object()
        .ok_or_else(|| ToolError(format!("properties must be an object, got {values}")))?;
    let types = schema.map(schema_types).unwrap_or_default();
    let mut mapped = Map::with_capacity(values.len());
    for (name, value) in values {
        if value.is_null() {
            continue;
        }
        let prop_type = match schema {
            Some(_) => types.get(name).map(String::as_str).ok_or_else(|| {
                ToolError({
                    let known: Vec<&str> = types.keys().map(String::as_str).collect();
                    format!(
                        "no property {name:?} on this data source; its properties are: {}",
                        known.join(", ")
                    )
                })
            })?,
            None => {
                if name == "title" {
                    "title"
                } else {
                    return Err(ToolError(
                        "only the title can be written on a page outside a data source".to_string(),
                    ));
                }
            }
        };
        mapped.insert(name.clone(), write_property(prop_type, value)?);
    }
    Ok(Value::Object(mapped))
}

/// The API's property payload for one plain value, guided by the property's
/// schema type.
fn write_property(prop_type: &str, value: &Value) -> Result<Value, ToolError> {
    match prop_type {
        "title" | "rich_text" => rich_text_value(prop_type, value),
        "number" => number_value(value),
        "checkbox" => bool_value(value),
        "select" | "status" => named_option(prop_type, value),
        "multi_select" => multi_options(value),
        "date" => date_value(value),
        "url" | "email" | "phone_number" => string_value(prop_type, value),
        "people" | "relation" => id_list(prop_type, value),
        other => Err(ToolError(format!(
            "writing {other} properties is not supported in this version"
        ))),
    }
}

/// A single-key JSON object.
fn single(key: &str, value: Value) -> Value {
    let mut map = Map::with_capacity(1);
    map.insert(key.to_string(), value);
    Value::Object(map)
}

fn rich_text_value(kind: &str, value: &Value) -> Result<Value, ToolError> {
    match value.as_str() {
        Some(text) => Ok(single(kind, json!([{"text": {"content": text}}]))),
        None => Err(ToolError(format!("{kind} needs a string, got {value}"))),
    }
}

fn number_value(value: &Value) -> Result<Value, ToolError> {
    if value.is_number() {
        Ok(json!({"number": value}))
    } else {
        Err(ToolError(format!("number needs a number, got {value}")))
    }
}

fn bool_value(value: &Value) -> Result<Value, ToolError> {
    match value.as_bool() {
        Some(checked) => Ok(json!({"checkbox": checked})),
        None => Err(ToolError(format!("checkbox needs a boolean, got {value}"))),
    }
}

fn named_option(kind: &str, value: &Value) -> Result<Value, ToolError> {
    match value.as_str() {
        Some(name) => Ok(single(kind, json!({"name": name}))),
        None => Err(ToolError(format!(
            "{kind} needs an option name string, got {value}"
        ))),
    }
}

fn multi_options(value: &Value) -> Result<Value, ToolError> {
    match value.as_array() {
        Some(options) => {
            let mut names = Vec::with_capacity(options.len());
            for option in options {
                match option.as_str() {
                    Some(name) => names.push(json!({"name": name})),
                    None => {
                        return Err(ToolError(format!(
                            "multi_select needs an array of option name strings, got {value}"
                        )));
                    }
                }
            }
            Ok(json!({"multi_select": names}))
        }
        None => Err(ToolError(format!(
            "multi_select needs an array of option name strings, got {value}"
        ))),
    }
}

fn date_value(value: &Value) -> Result<Value, ToolError> {
    if let Some(start) = value.as_str() {
        return Ok(json!({"date": {"start": start}}));
    }
    match value.as_object() {
        Some(range) => {
            let start = range
                .get("start")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError(format!("date needs a start string, got {value}")))?;
            Ok(object(&[(
                "date",
                Some(object(&[
                    ("start", Some(json!(start))),
                    (
                        "end",
                        range
                            .get("end")
                            .and_then(Value::as_str)
                            .map(|end| Value::String(end.to_string())),
                    ),
                ])),
            )]))
        }
        None => Err(ToolError(format!(
            "date needs a YYYY-MM-DD string or a {{start, end}} object, got {value}"
        ))),
    }
}

fn string_value(kind: &str, value: &Value) -> Result<Value, ToolError> {
    match value.as_str() {
        Some(text) => Ok(single(kind, json!(text))),
        None => Err(ToolError(format!("{kind} needs a string, got {value}"))),
    }
}

fn id_list(kind: &str, value: &Value) -> Result<Value, ToolError> {
    match value.as_array() {
        Some(ids) => {
            let mut items = Vec::with_capacity(ids.len());
            for id in ids {
                match id.as_str() {
                    Some(id) => items.push(json!({"id": id})),
                    None => {
                        return Err(ToolError(format!(
                            "{kind} needs an array of id strings, got {value}"
                        )));
                    }
                }
            }
            Ok(single(kind, json!(items)))
        }
        None => Err(ToolError(format!(
            "{kind} needs an array of id strings, got {value}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Filters and sorts
// ---------------------------------------------------------------------------

/// Builds the API's `filter` from the agent's plain form:
/// `{"Status": "Done"}` (equals), `{"Due": {"before": "2026-12-01"}}`
/// (operator), `{"and": [...]}` / `{"or": [...]}` groups. Property names are
/// checked against the schema, and operators against the property type.
fn build_filter(schema: &Value, filter: &Value) -> Result<Value, ToolError> {
    filter_value(&schema_types(schema), filter)
}

fn filter_value(types: &HashMap<String, String>, filter: &Value) -> Result<Value, ToolError> {
    let obj = filter
        .as_object()
        .ok_or_else(|| ToolError(format!("filter must be an object, got {filter}")))?;
    if let Some(and) = obj.get("and") {
        return group_filter(types, "and", and);
    }
    if let Some(or) = obj.get("or") {
        return group_filter(types, "or", or);
    }
    let mut conditions = Vec::with_capacity(obj.len());
    for (name, value) in obj {
        let prop_type = types.get(name).map(String::as_str).ok_or_else(|| {
            ToolError({
                let known: Vec<&str> = types.keys().map(String::as_str).collect();
                format!(
                    "no property {name:?} on this data source; its properties are: {}",
                    known.join(", ")
                )
            })
        })?;
        conditions.push(property_condition(name, prop_type, value)?);
    }
    if conditions.is_empty() {
        return Err(ToolError("filter must not be empty".to_string()));
    }
    if conditions.len() == 1 {
        return Ok(conditions.swap_remove(0));
    }
    Ok(json!({"and": conditions}))
}

fn group_filter(
    types: &HashMap<String, String>,
    op: &str,
    group: &Value,
) -> Result<Value, ToolError> {
    let items = group
        .as_array()
        .ok_or_else(|| ToolError(format!("{op} needs an array of conditions, got {group}")))?;
    if items.is_empty() {
        return Err(ToolError(format!("{op} needs at least one condition")));
    }
    let mut conditions = Vec::with_capacity(items.len());
    for item in items {
        conditions.push(filter_value(types, item)?);
    }
    Ok(single(op, Value::Array(conditions)))
}

/// One `{"property": name, <type>: {<operator>: operand}}` condition.
fn property_condition(name: &str, prop_type: &str, value: &Value) -> Result<Value, ToolError> {
    let supported = supported_operators(prop_type).ok_or_else(|| {
        ToolError(format!(
            "filtering on {prop_type} properties is not supported"
        ))
    })?;
    let (operator, operand) = match value.as_object() {
        Some(spec) => {
            let mut entries = spec.iter();
            match (entries.next(), entries.next()) {
                (Some((op, operand)), None) => (op.as_str(), operand),
                _ => {
                    return Err(ToolError(format!(
                        "use exactly one operator per property, got {value}"
                    )));
                }
            }
        }
        None => ("equals", value),
    };
    if !supported.contains(&operator) {
        return Err(ToolError(format!(
            "{prop_type} properties do not support the {operator} operator"
        )));
    }
    let mut condition = Map::with_capacity(2);
    condition.insert("property".to_string(), json!(name));
    condition.insert(
        prop_type.to_string(),
        Value::Object({
            let mut op = Map::with_capacity(1);
            op.insert(operator.to_string(), operand.clone());
            op
        }),
    );
    Ok(Value::Object(condition))
}

/// The query operators each property type supports.
fn supported_operators(prop_type: &str) -> Option<&'static [&'static str]> {
    match prop_type {
        "title" | "rich_text" | "url" | "email" | "phone_number" => Some(&[
            "equals",
            "does_not_equal",
            "contains",
            "does_not_contain",
            "starts_with",
            "ends_with",
            "is_empty",
            "is_not_empty",
        ]),
        "number" | "unique_id" => Some(&[
            "equals",
            "does_not_equal",
            "greater_than",
            "less_than",
            "greater_than_or_equal_to",
            "less_than_or_equal_to",
            "is_empty",
            "is_not_empty",
        ]),
        "checkbox" => Some(&["equals", "does_not_equal"]),
        "select" | "status" => Some(&["equals", "does_not_equal", "is_empty", "is_not_empty"]),
        "multi_select" => Some(&["contains", "does_not_contain", "is_empty", "is_not_empty"]),
        "date" | "created_time" | "last_edited_time" => Some(&[
            "equals",
            "before",
            "after",
            "on_or_before",
            "on_or_after",
            "past_week",
            "past_month",
            "past_year",
            "this_week",
            "is_empty",
            "is_not_empty",
        ]),
        "people" | "created_by" | "last_edited_by" | "relation" => {
            Some(&["contains", "does_not_contain", "is_empty", "is_not_empty"])
        }
        "files" => Some(&["is_empty", "is_not_empty"]),
        _ => None,
    }
}

/// The API's `sorts` from the agent's plain form:
/// `[{"property": "Due", "direction": "ascending"}]`, direction defaulting to
/// ascending, or `[{"timestamp": "created_time", ...}]`.
fn build_sorts(sorts: &Value) -> Result<Value, ToolError> {
    let sorts = sorts
        .as_array()
        .ok_or_else(|| ToolError(format!("sorts must be an array, got {sorts}")))?;
    let mut built = Vec::with_capacity(sorts.len());
    for sort in sorts {
        let obj = sort
            .as_object()
            .ok_or_else(|| ToolError(format!("each sort must be an object, got {sort}")))?;
        let direction = obj
            .get("direction")
            .and_then(Value::as_str)
            .unwrap_or("ascending");
        if direction != "ascending" && direction != "descending" {
            return Err(ToolError(format!(
                "sort direction must be ascending or descending, got {direction:?}"
            )));
        }
        if let Some(property) = obj.get("property").and_then(Value::as_str) {
            built.push(json!({"property": property, "direction": direction}));
        } else if let Some(timestamp) = obj.get("timestamp").and_then(Value::as_str) {
            if timestamp != "created_time" && timestamp != "last_edited_time" {
                return Err(ToolError(format!(
                    "sort timestamp must be created_time or last_edited_time, got {timestamp:?}"
                )));
            }
            built.push(json!({"timestamp": timestamp, "direction": direction}));
        } else {
            return Err(ToolError(format!(
                "each sort needs a property or a timestamp, got {sort}"
            )));
        }
    }
    Ok(Value::Array(built))
}

// ---------------------------------------------------------------------------
// Markdown to blocks
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::iter::Peekable;

/// Inline annotations on one rich-text span.
#[derive(Clone, Default)]
struct Annotations {
    bold: bool,
    italic: bool,
    strikethrough: bool,
    code: bool,
}

impl Annotations {
    fn to_json(&self) -> Value {
        json!({
            "bold": self.bold,
            "italic": self.italic,
            "strikethrough": self.strikethrough,
            "code": self.code,
        })
    }
}

/// One parsed rich-text span.
struct Span {
    text: String,
    annotations: Annotations,
    link: Option<String>,
}

/// Converts the supported Markdown subset to Notion block objects:
/// `#`/`##`/`###` headings, `-`/`*` bullets, `1.` numbered items,
/// `- [ ]`/`- [x]` todos, `>` quotes, fenced code, `---` dividers, and
/// paragraphs. Consecutive plain lines join into one paragraph.
fn markdown_to_blocks(markdown: &str) -> Vec<Value> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut lines = markdown.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            flush_paragraph(&mut paragraph, &mut blocks);
        } else if let Some(code) = parse_code_fence(trimmed, &mut lines) {
            flush_paragraph(&mut paragraph, &mut blocks);
            blocks.push(code);
        } else if let Some(block) = parse_block_line(trimmed) {
            flush_paragraph(&mut paragraph, &mut blocks);
            blocks.push(block);
        } else {
            paragraph.push(trimmed.to_string());
        }
    }
    flush_paragraph(&mut paragraph, &mut blocks);
    blocks
}

/// Pushes buffered paragraph lines as one paragraph block.
fn flush_paragraph(paragraph: &mut Vec<String>, blocks: &mut Vec<Value>) {
    if !paragraph.is_empty() {
        blocks.push(rich_text_block("paragraph", &paragraph.join("\n")));
        paragraph.clear();
    }
}

/// A fenced code block opening at `trimmed`, consuming lines through the
/// closing fence. `None` when the line does not open a fence. Code content
/// stays literal: no inline Markdown is parsed inside.
fn parse_code_fence<'a, I>(trimmed: &str, lines: &mut Peekable<I>) -> Option<Value>
where
    I: Iterator<Item = &'a str>,
{
    let tag = trimmed.strip_prefix("```")?;
    let mut code: Vec<&str> = Vec::new();
    for next in lines.by_ref() {
        if next.trim() == "```" {
            break;
        }
        code.push(next);
    }
    let mut field = Map::with_capacity(2);
    field.insert(
        "rich_text".to_string(),
        json!([{"type": "text", "text": {"content": code.join("\n")}}]),
    );
    field.insert("language".to_string(), json!(code_language(tag)));
    let mut block = Map::with_capacity(3);
    block.insert("object".to_string(), json!("block"));
    block.insert("type".to_string(), json!("code"));
    block.insert("code".to_string(), Value::Object(field));
    Some(Value::Object(block))
}

/// Maps a fence's language tag to a Notion code language.
fn code_language(tag: &str) -> &str {
    match tag.trim().to_ascii_lowercase().as_str() {
        "js" => "javascript",
        "ts" => "typescript",
        "py" => "python",
        "sh" | "shell" | "zsh" => "bash",
        "yml" => "yaml",
        "md" => "markdown",
        "" => "plain text",
        _ => tag.trim(),
    }
}

/// One line that maps to a single block: headings, list items, todos, quotes
/// and dividers. `None` for paragraph text.
fn parse_block_line(trimmed: &str) -> Option<Value> {
    if let Some(heading) = trimmed.strip_prefix("### ") {
        return Some(rich_text_block("heading_3", heading));
    }
    if let Some(heading) = trimmed.strip_prefix("## ") {
        return Some(rich_text_block("heading_2", heading));
    }
    if let Some(heading) = trimmed.strip_prefix("# ") {
        return Some(rich_text_block("heading_1", heading));
    }
    if trimmed == "---" || trimmed == "***" || trimmed == "___" {
        return Some(divider_block());
    }
    if let Some(quote) = trimmed.strip_prefix("> ") {
        return Some(rich_text_block("quote", quote));
    }
    if let Some(item) = trimmed.strip_prefix("- [ ] ") {
        return Some(todo_block(item, false));
    }
    if let Some(item) = trimmed
        .strip_prefix("- [x] ")
        .or_else(|| trimmed.strip_prefix("- [X] "))
    {
        return Some(todo_block(item, true));
    }
    if let Some(item) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        return Some(rich_text_block("bulleted_list_item", item));
    }
    if let Some(item) = numbered_item(trimmed) {
        return Some(rich_text_block("numbered_list_item", item));
    }
    None
}

/// The item text of a `1. ` numbered-list line.
fn numbered_item(trimmed: &str) -> Option<&str> {
    let (digits, rest) = trimmed.split_once(". ")?;
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        Some(rest)
    } else {
        None
    }
}

/// A block whose typed field shares the block's type name (paragraph,
/// headings, list items, quotes), with inline-parsed rich text.
fn rich_text_block(kind: &str, text: &str) -> Value {
    let mut field = Map::with_capacity(1);
    field.insert("rich_text".to_string(), rich_text_of(text));
    let mut block = Map::with_capacity(3);
    block.insert("object".to_string(), json!("block"));
    block.insert("type".to_string(), json!(kind));
    block.insert(kind.to_string(), Value::Object(field));
    Value::Object(block)
}

/// A to-do block.
fn todo_block(text: &str, checked: bool) -> Value {
    let mut field = Map::with_capacity(2);
    field.insert("rich_text".to_string(), rich_text_of(text));
    field.insert("checked".to_string(), json!(checked));
    let mut block = Map::with_capacity(3);
    block.insert("object".to_string(), json!("block"));
    block.insert("type".to_string(), json!("to_do"));
    block.insert("to_do".to_string(), Value::Object(field));
    Value::Object(block)
}

fn divider_block() -> Value {
    let mut block = Map::with_capacity(3);
    block.insert("object".to_string(), json!("block"));
    block.insert("type".to_string(), json!("divider"));
    block.insert("divider".to_string(), json!({}));
    Value::Object(block)
}

/// Parses inline Markdown (`**bold**`, `*italic*`, `` `code` ``,
/// `~~strike~~`, `[text](url)`) into a Notion rich-text array.
fn rich_text_of(text: &str) -> Value {
    Value::Array(parse_spans(text).iter().map(span_json).collect())
}

/// Pushes `current` as one span when non-empty.
fn push_span(
    spans: &mut Vec<Span>,
    current: &mut String,
    annotations: &Annotations,
    link: Option<String>,
) {
    if !current.is_empty() {
        spans.push(Span {
            text: std::mem::take(current),
            annotations: annotations.clone(),
            link,
        });
    }
}

fn parse_spans(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut current = String::new();
    let mut annotations = Annotations::default();
    let mut i = 0;
    // `i` always sits on a char boundary: it starts at 0 and advances by
    // whole chars or whole ASCII markers, so `&text[i..]` is always valid.
    while i < text.len() {
        let rest = &text[i..];
        if rest.starts_with('[') {
            match parse_link(rest) {
                Some((label, url, len)) => {
                    push_span(&mut spans, &mut current, &annotations, None);
                    spans.push(Span {
                        text: label,
                        annotations: Annotations::default(),
                        link: Some(url),
                    });
                    i += len;
                }
                None => {
                    current.push('[');
                    i += 1;
                }
            }
        } else if rest.starts_with("**") {
            push_span(&mut spans, &mut current, &annotations, None);
            annotations.bold = !annotations.bold;
            i += 2;
        } else if rest.starts_with("~~") {
            push_span(&mut spans, &mut current, &annotations, None);
            annotations.strikethrough = !annotations.strikethrough;
            i += 2;
        } else if rest.starts_with('*') {
            push_span(&mut spans, &mut current, &annotations, None);
            annotations.italic = !annotations.italic;
            i += 1;
        } else if rest.starts_with('`') {
            push_span(&mut spans, &mut current, &annotations, None);
            annotations.code = !annotations.code;
            i += 1;
        } else {
            let ch = rest.chars().next().unwrap_or_default();
            current.push(ch);
            i += ch.len_utf8();
        }
    }
    push_span(&mut spans, &mut current, &annotations, None);
    spans
}

/// Parses `[label](url)` at the start of `text`: the label, the url, and the
/// matched byte length. The length lands on a char boundary.
fn parse_link(text: &str) -> Option<(String, String, usize)> {
    let label_end = text.find("](")?;
    let url_end = text[label_end + 2..].find(')')?;
    Some((
        text[1..label_end].to_string(),
        text[label_end + 2..label_end + 2 + url_end].to_string(),
        label_end + 2 + url_end + 1,
    ))
}

fn span_json(span: &Span) -> Value {
    let mut text = Map::with_capacity(2);
    text.insert("content".to_string(), json!(&span.text));
    if let Some(url) = &span.link {
        text.insert("link".to_string(), json!({"url": url}));
    }
    let mut item = Map::with_capacity(3);
    item.insert("type".to_string(), json!("text"));
    item.insert("text".to_string(), Value::Object(text));
    item.insert("annotations".to_string(), span.annotations.to_json());
    Value::Object(item)
}

// ---------------------------------------------------------------------------
// Tool runners
// ---------------------------------------------------------------------------

/// Runs `getPage`: one page with plain-value properties, plus the body as
/// Markdown when asked.
fn get_page(api: &str, request: &ToolRequest, send: Sender<'_>) -> Result<ToolResponse, ToolError> {
    let input: GetPage = request.json()?;
    let page = call_api(
        send,
        &Request::get(format!("{api}/v1/pages/{}", encode(&input.page_id))),
        "reading the page",
    )?;
    let mut answered = simplify_page(&page);
    if input.include_content.unwrap_or(false) {
        let markdown = call_api(
            send,
            &Request::get(format!(
                "{}/v1/pages/{}/markdown",
                api,
                encode(&input.page_id)
            )),
            "reading the page content",
        )?;
        let content = markdown
            .get("markdown")
            .and_then(Value::as_str)
            .or_else(|| markdown.as_str())
            .ok_or_else(|| {
                ToolError("the API returned page content in an unexpected shape".to_string())
            })?;
        answered
            .as_object_mut()
            .map(|page| page.insert("content".to_string(), json!(content)));
    }
    ToolResponse::structured(&answered).map_err(ToolError::from)
}

/// The page-creation `parent` object plus the data source schema when the
/// parent is a data source (needed to map plain property values).
fn plan_parent(
    api: &str,
    send: Sender<'_>,
    parent: &ParentRef,
) -> Result<(Value, Option<Value>), ToolError> {
    match (&parent.data_source, &parent.page) {
        (Some(_), Some(_)) => Err(ToolError(
            "parent takes either dataSource or page, not both".to_string(),
        )),
        (Some(data_source), None) => {
            let id = resolve_data_source(api, send, data_source)?;
            let schema = call_api(
                send,
                &Request::get(format!("{api}/v1/data_sources/{}", encode(&id))),
                "reading the data source",
            )?;
            Ok((
                json!({"type": "data_source_id", "data_source_id": id}),
                Some(schema),
            ))
        }
        (None, Some(page)) => {
            let id = resolve_page(api, send, page)?;
            Ok((json!({"type": "page_id", "page_id": id}), None))
        }
        (None, None) => Err(ToolError("parent needs a dataSource or a page".to_string())),
    }
}

/// Appends Markdown as blocks to a page (at most 100 blocks per call),
/// answering the appended count.
fn append_blocks(
    api: &str,
    send: Sender<'_>,
    page_id: &str,
    markdown: &str,
) -> Result<usize, ToolError> {
    let blocks = markdown_to_blocks(markdown);
    if blocks.is_empty() {
        return Err(ToolError("there is no content to append".to_string()));
    }
    let mut appended = 0;
    for chunk in blocks.chunks(100) {
        let planned = patch_json(
            &format!("{}/v1/blocks/{}/children", api, encode(page_id)),
            &json!({"children": chunk}),
        );
        call_api(send, &planned, "appending the page content")?;
        appended += chunk.len();
    }
    Ok(appended)
}

/// Runs `createPage`: resolves the parent, maps plain property values through
/// the data source schema, creates the page, and appends Markdown content —
/// all in one tool call.
fn create_page(
    api: &str,
    request: &ToolRequest,
    send: Sender<'_>,
) -> Result<ToolResponse, ToolError> {
    let input: CreatePage = request.json()?;
    let (parent, schema) = plan_parent(api, send, &input.parent)?;
    let mut body = Map::with_capacity(2);
    body.insert("parent".to_string(), parent);
    if let Some(properties) = &input.properties {
        body.insert(
            "properties".to_string(),
            map_properties(schema.as_ref(), properties)?,
        );
    }
    let page = call_api(
        send,
        &post_json(&format!("{api}/v1/pages"), &Value::Object(body)),
        "creating the page",
    )?;
    let page_id = page
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError("the API returned a page without an id".to_string()))?;
    if let Some(content) = &input.content {
        append_blocks(api, send, page_id, content)?;
    }
    ToolResponse::structured(&simplify_page(&page)).map_err(ToolError::from)
}

/// The data source schema for a page's parent, when the page lives in one.
fn data_source_schema_for_page(
    api: &str,
    send: Sender<'_>,
    page_id: &str,
) -> Result<Option<Value>, ToolError> {
    let page = call_api(
        send,
        &Request::get(format!("{api}/v1/pages/{}", encode(page_id))),
        "reading the page",
    )?;
    let data_source_id = page
        .get("parent")
        .and_then(|parent| parent.get("data_source_id"))
        .and_then(Value::as_str);
    match data_source_id {
        Some(id) => call_api(
            send,
            &Request::get(format!("{api}/v1/data_sources/{}", encode(id))),
            "reading the data source",
        )
        .map(Some),
        None => Ok(None),
    }
}

/// Runs `updatePage`: plain-value property updates mapped through the data
/// source schema, and/or trash/restore.
fn update_page(
    api: &str,
    request: &ToolRequest,
    send: Sender<'_>,
) -> Result<ToolResponse, ToolError> {
    let input: UpdatePage = request.json()?;
    let mut body = Map::new();
    if let Some(properties) = &input.properties {
        let schema = data_source_schema_for_page(api, send, &input.page_id)?;
        body.insert(
            "properties".to_string(),
            map_properties(schema.as_ref(), properties)?,
        );
    }
    if let Some(trash) = input.trash {
        body.insert("in_trash".to_string(), json!(trash));
    }
    if body.is_empty() {
        return Err(ToolError(
            "nothing to update: give properties or trash".to_string(),
        ));
    }
    let page = call_api(
        send,
        &patch_json(
            &format!("{}/v1/pages/{}", api, encode(&input.page_id)),
            &Value::Object(body),
        ),
        "updating the page",
    )?;
    ToolResponse::structured(&simplify_page(&page)).map_err(ToolError::from)
}

/// Runs `movePage`: moves a page under a new parent page.
fn move_page(
    api: &str,
    request: &ToolRequest,
    send: Sender<'_>,
) -> Result<ToolResponse, ToolError> {
    let input: MovePage = request.json()?;
    let parent_id = resolve_page(api, send, &input.parent)?;
    let page = call_api(
        send,
        &patch_json(
            &format!("{}/v1/pages/{}", api, encode(&input.page_id)),
            &json!({"parent": {"type": "page_id", "page_id": parent_id}}),
        ),
        "moving the page",
    )?;
    ToolResponse::structured(&simplify_page(&page)).map_err(ToolError::from)
}

/// Runs `queryDataSource`: resolves the title, reads the schema for plain
/// filters and sorts, and answers rows with plain-value properties.
fn query_data_source(
    api: &str,
    request: &ToolRequest,
    send: Sender<'_>,
) -> Result<ToolResponse, ToolError> {
    let input: QueryDataSource = request.json()?;
    let id = resolve_data_source(api, send, &input.data_source)?;
    let schema = call_api(
        send,
        &Request::get(format!("{api}/v1/data_sources/{}", encode(&id))),
        "reading the data source",
    )?;
    let mut body = Map::new();
    if let Some(filter) = &input.filter {
        body.insert("filter".to_string(), build_filter(&schema, filter)?);
    }
    if let Some(sorts) = &input.sorts {
        body.insert("sorts".to_string(), build_sorts(sorts)?);
    }
    if let Some(page_size) = input.page_size {
        body.insert("page_size".to_string(), json!(page_size));
    }
    if let Some(cursor) = &input.start_cursor {
        body.insert("start_cursor".to_string(), json!(cursor));
    }
    let answered = call_api(
        send,
        &post_json(
            &format!("{api}/v1/data_sources/{}/query", encode(&id)),
            &Value::Object(body),
        ),
        "querying the data source",
    )?;
    ToolResponse::structured(&paged_answer(&answered, simplify_page)).map_err(ToolError::from)
}

/// A paged list answer: `results` mapped through `simplify`, plus the
/// pagination cursor.
fn paged_answer(answered: &Value, simplify: impl Fn(&Value) -> Value) -> Value {
    let results = answered
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    json!({
        "results": results.iter().map(simplify).collect::<Vec<_>>(),
        "hasMore": answered.get("has_more").cloned().unwrap_or(json!(false)),
        "nextCursor": answered.get("next_cursor").cloned().unwrap_or(Value::Null),
    })
}

/// A data source's schema in the shape agents read.
fn simplify_data_source(schema: &Value) -> Value {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| {
            props
                .iter()
                .map(|(name, prop)| {
                    let prop_type = prop.get("type").and_then(Value::as_str).unwrap_or("");
                    let mut field = Map::with_capacity(3);
                    field.insert("name".to_string(), json!(name));
                    field.insert("type".to_string(), json!(prop_type));
                    if matches!(prop_type, "select" | "multi_select" | "status") {
                        let options: Vec<Value> = prop
                            .get(prop_type)
                            .and_then(|typed| typed.get("options"))
                            .and_then(Value::as_array)
                            .map(|options| {
                                options
                                    .iter()
                                    .filter_map(|option| option.get("name").cloned())
                                    .collect()
                            })
                            .unwrap_or_default();
                        field.insert("options".to_string(), Value::Array(options));
                    }
                    (name.clone(), Value::Object(field))
                })
                .collect::<Map<String, Value>>()
        })
        .unwrap_or_default();
    object(&[
        ("id", schema.get("id").cloned()),
        (
            "title",
            Some(json!(plain_text(
                schema.get("title").unwrap_or(&Value::Null)
            ))),
        ),
        ("properties", Some(Value::Object(properties))),
    ])
}

/// Runs `getDataSource`: the schema in the shape agents read.
fn get_data_source(
    api: &str,
    request: &ToolRequest,
    send: Sender<'_>,
) -> Result<ToolResponse, ToolError> {
    let input: DataSourceRef = request.json()?;
    let id = resolve_data_source(api, send, &input.data_source)?;
    let schema = call_api(
        send,
        &Request::get(format!("{api}/v1/data_sources/{}", encode(&id))),
        "reading the data source",
    )?;
    ToolResponse::structured(&simplify_data_source(&schema)).map_err(ToolError::from)
}

/// The API's property schema for one agent-described property definition.
fn schema_property(def: &PropertyDef) -> Result<Value, ToolError> {
    match def.property_type.as_str() {
        "title" => Ok(json!({"title": {}})),
        "rich_text" => Ok(json!({"rich_text": {}})),
        "number" => Ok(json!({"number": {"format": "number"}})),
        "checkbox" => Ok(json!({"checkbox": {}})),
        "date" => Ok(json!({"date": {}})),
        "url" => Ok(json!({"url": {}})),
        "email" => Ok(json!({"email": {}})),
        "phone_number" => Ok(json!({"phone_number": {}})),
        "people" => Ok(json!({"people": {}})),
        "select" | "multi_select" | "status" => {
            let options = def.options.as_deref().unwrap_or_default();
            if options.is_empty() {
                return Err(ToolError(format!(
                    "{} needs at least one option",
                    def.property_type
                )));
            }
            let mut inner = Map::with_capacity(1);
            inner.insert(
                "options".to_string(),
                json!(
                    options
                        .iter()
                        .map(|name| json!({"name": name}))
                        .collect::<Vec<_>>()
                ),
            );
            Ok(single(&def.property_type, Value::Object(inner)))
        }
        other => Err(ToolError(format!(
            "creating {other} properties is not supported in this version"
        ))),
    }
}

/// Runs `createDataSource`: resolves the parent page and creates the data
/// source with its simple property schema in one call.
fn create_data_source(
    api: &str,
    request: &ToolRequest,
    send: Sender<'_>,
) -> Result<ToolResponse, ToolError> {
    let input: CreateDataSource = request.json()?;
    if input.properties.is_empty() {
        return Err(ToolError(
            "createDataSource needs at least one property".to_string(),
        ));
    }
    let parent_id = resolve_page(api, send, &input.parent)?;
    let mut properties = Map::with_capacity(input.properties.len());
    for def in &input.properties {
        properties.insert(def.name.clone(), schema_property(def)?);
    }
    let body = json!({
        "parent": {"type": "page_id", "page_id": parent_id},
        "title": [{"text": {"content": input.title}}],
        "properties": properties,
    });
    let schema = call_api(
        send,
        &post_json(&format!("{api}/v1/data_sources"), &body),
        "creating the data source",
    )?;
    ToolResponse::structured(&simplify_data_source(&schema)).map_err(ToolError::from)
}

/// Runs `appendContent`: Markdown converted to blocks and appended.
fn append_content(
    api: &str,
    request: &ToolRequest,
    send: Sender<'_>,
) -> Result<ToolResponse, ToolError> {
    let input: AppendContent = request.json()?;
    let appended = append_blocks(api, send, &input.page_id, &input.markdown)?;
    ToolResponse::structured(&json!({
        "pageId": input.page_id,
        "appendedBlocks": appended,
    }))
    .map_err(ToolError::from)
}

/// One comment in the shape agents read.
fn simplify_comment(comment: &Value) -> Value {
    object(&[
        ("id", comment.get("id").cloned()),
        ("discussionId", comment.get("discussion_id").cloned()),
        (
            "text",
            Some(json!(plain_text(
                comment.get("rich_text").unwrap_or(&Value::Null)
            ))),
        ),
        ("createdTime", comment.get("created_time").cloned()),
        ("author", comment.get("created_by").map(simplify_user)),
    ])
}

/// Runs `listComments`: the comments on a page, oldest first.
fn list_comments(
    api: &str,
    request: &ToolRequest,
    send: Sender<'_>,
) -> Result<ToolResponse, ToolError> {
    let input: ListComments = request.json()?;
    let url = with_query(
        &format!("{api}/v1/comments"),
        &[
            ("block_id", Some(input.page_id.clone())),
            ("page_size", input.page_size.map(|n| n.to_string())),
            ("start_cursor", input.start_cursor.clone()),
        ],
    );
    let answered = call_api(send, &Request::get(url), "listing the comments")?;
    ToolResponse::structured(&paged_answer(&answered, simplify_comment)).map_err(ToolError::from)
}

/// Runs `createComment`: a top-level comment, or a reply with `discussionId`.
fn create_comment(
    api: &str,
    request: &ToolRequest,
    send: Sender<'_>,
) -> Result<ToolResponse, ToolError> {
    let input: CreateComment = request.json()?;
    let body = object(&[
        ("parent", Some(json!({"page_id": input.page_id}))),
        (
            "discussion_id",
            input
                .discussion_id
                .as_deref()
                .map(|id| Value::String(id.to_string())),
        ),
        (
            "rich_text",
            Some(json!([{"text": {"content": input.text}}])),
        ),
    ]);
    let comment = call_api(
        send,
        &post_json(&format!("{api}/v1/comments"), &body),
        "creating the comment",
    )?;
    ToolResponse::structured(&simplify_comment(&comment)).map_err(ToolError::from)
}

/// Runs `getCurrentUser`: the authenticated user or bot.
fn get_current_user(
    api: &str,
    _request: &ToolRequest,
    send: Sender<'_>,
) -> Result<ToolResponse, ToolError> {
    let user = call_api(
        send,
        &Request::get(format!("{api}/v1/users/me")),
        "reading the current user",
    )?;
    ToolResponse::structured(&object(&[
        ("id", user.get("id").cloned()),
        ("name", user.get("name").cloned()),
        ("type", user.get("type").cloned()),
        (
            "email",
            user.get("person")
                .and_then(|person| person.get("email"))
                .cloned(),
        ),
        (
            "workspace",
            user.get("bot")
                .and_then(|bot| bot.get("workspace_name"))
                .cloned(),
        ),
    ]))
    .map_err(ToolError::from)
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
        Notion::call_with(request(tool, body), &mut |request| stub.send(request))
    }

    /// The tool answered with an error (the shim discards the message, so
    /// this only distinguishes errors from successful answers).
    fn is_tool_error(result: &Result<ToolResponse, ConnectorError>) -> bool {
        matches!(result, Ok(response) if response.structured.is_none())
    }

    fn answered(result: Result<ToolResponse, ConnectorError>) -> Value {
        let response = result.unwrap();
        serde_json::from_str(response.structured.as_deref().unwrap()).unwrap()
    }

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

    const API: &str = "https://api.notion.com";

    #[test]
    fn headers_carry_the_pinned_api_version() {
        let mut stub = Stub::of(&[ok_json(r#"{"results": []}"#)]);
        let _ = call("searchWorkspace", r#"{"query": "x"}"#, &mut stub);
        assert_eq!(stub.seen.len(), 1);
        assert_eq!(
            header(&stub.seen[0], "notion-version"),
            Some(NOTION_VERSION.into())
        );
        assert_eq!(
            header(&stub.seen[0], "accept"),
            Some("application/json".into())
        );
        assert!(
            header(&stub.seen[0], "user-agent")
                .unwrap_or_default()
                .starts_with("agentrq-notion-connector/")
        );
    }

    #[test]
    fn encodes_and_builds_queries() {
        assert_eq!(encode("a b/c?d"), "a%20b%2Fc%3Fd");
        assert_eq!(encode("abc-._~09AZaz"), "abc-._~09AZaz");
        assert_eq!(
            with_query(
                "https://a.test/x",
                &[
                    ("q", Some("a b".into())),
                    ("n", None),
                    ("p", Some("1".into()))
                ]
            ),
            "https://a.test/x?q=a%20b&p=1"
        );
        assert_eq!(
            with_query("https://a.test/x", &[("n", None)]),
            "https://a.test/x"
        );
        assert_eq!(
            object(&[("a", Some(json!(1))), ("b", None)]),
            json!({"a": 1})
        );
        assert_eq!(single("k", json!("v")), json!({"k": "v"}));
    }

    #[test]
    fn recognizes_ids() {
        assert!(is_id("12345678-1234-1234-1234-123456789abc"));
        assert!(is_id("12345678123412341234123456789abc"));
        assert!(is_id("ABCDEFAB-CDEF-ABCD-EFAB-CDEFABCDEFAB"));
        assert!(!is_id("Tasks"));
        assert!(!is_id("12345678-1234-1234-1234-123456789abz"));
        assert!(!is_id("1234"));
        assert!(!is_id(""));
    }

    #[test]
    fn step_ok_reports_api_failures() {
        assert!(step_ok(&ok_json("{}"), "x").is_ok());
        assert_eq!(
            step_ok(&response(404, "nope"), "reading the thing")
                .unwrap_err()
                .0,
            "reading the thing failed: the API answered 404: nope"
        );
    }

    #[test]
    fn call_api_surfaces_send_and_parse_failures() {
        let mut stub = Stub::of(&[]);
        let mut send = |request: &Request| stub.send(request);
        assert!(call_api(&mut send, &Request::get("https://a.test"), "x").is_err());
        let mut stub = Stub::of(&[response(200, "not json")]);
        let mut send = |request: &Request| stub.send(request);
        assert!(call_api(&mut send, &Request::get("https://a.test"), "x").is_err());
    }

    #[test]
    fn plain_text_joins_rich_text() {
        assert_eq!(
            plain_text(&json!([{"plain_text": "a"}, {"plain_text": "b"}, {"nope": 1}])),
            "ab"
        );
        assert_eq!(plain_text(&json!([])), "");
        assert_eq!(plain_text(&json!(null)), "");
        assert_eq!(plain_text(&json!({"plain_text": "x"})), "");
    }

    #[test]
    fn page_title_finds_the_title_property() {
        let props: Map<String, Value> =
            serde_json::from_str(r#"{"Name": {"type": "title", "title": [{"plain_text": "Hi"}]}}"#)
                .unwrap();
        assert_eq!(page_title(&props).as_deref(), Some("Hi"));
        let props: Map<String, Value> =
            serde_json::from_str(r#"{"Name": {"type": "rich_text", "rich_text": []}}"#).unwrap();
        assert_eq!(page_title(&props), None);
        let props: Map<String, Value> =
            serde_json::from_str(r#"{"Name": {"type": "title", "title": []}}"#).unwrap();
        assert_eq!(page_title(&props), None);
    }

    #[test]
    fn hit_title_reads_pages_and_data_sources() {
        let page = json!({
            "object": "page",
            "properties": {"Name": {"type": "title", "title": [{"plain_text": "P"}]}}
        });
        assert_eq!(hit_title(&page).as_deref(), Some("P"));
        let untitled = json!({
            "object": "page",
            "properties": {"Name": {"type": "rich_text", "rich_text": []}}
        });
        assert_eq!(hit_title(&untitled), None);
        let source = json!({"object": "data_source", "title": [{"plain_text": "D"}]});
        assert_eq!(hit_title(&source).as_deref(), Some("D"));
        let empty = json!({"object": "data_source", "title": []});
        assert_eq!(hit_title(&empty), None);
        assert_eq!(hit_title(&json!({"object": "block"})), None);
    }

    #[test]
    fn pick_hit_matches_titles_and_passes_ids_through() {
        let hits = vec![
            json!({
                "object": "page", "id": "id-1",
                "properties": {"Name": {"type": "title", "title": [{"plain_text": "Tasks"}]}}
            }),
            json!({
                "object": "page", "id": "id-2",
                "properties": {"Name": {"type": "title", "title": [{"plain_text": "Notes"}]}}
            }),
        ];
        assert_eq!(
            pick_hit(&hits, "tasks", "page").unwrap()["id"],
            json!("id-1")
        );
        assert_eq!(
            pick_hit(&hits, "12345678-1234-1234-1234-123456789abc", "page").unwrap()["id"],
            json!("12345678-1234-1234-1234-123456789abc")
        );
        assert!(
            pick_hit(&hits, "Missing", "page")
                .unwrap_err()
                .0
                .contains("Notes")
        );
        assert!(
            pick_hit(&[], "Missing", "page")
                .unwrap_err()
                .0
                .contains("No page matches")
        );
        let dupes = vec![
            json!({
                "object": "page", "id": "id-1",
                "properties": {"Name": {"type": "title", "title": [{"plain_text": "Tasks"}]}}
            }),
            json!({
                "object": "page", "id": "id-2",
                "properties": {"Name": {"type": "title", "title": [{"plain_text": "TASKS"}]}}
            }),
        ];
        let error = pick_hit(&dupes, "tasks", "page").unwrap_err().0;
        assert!(error.contains("id-1") && error.contains("id-2"));
    }

    #[test]
    fn search_plans_the_query_and_maps_hits() {
        let mut stub = Stub::of(&[ok_json(
            r#"{"results": [
                {"object": "page", "id": "page-1", "url": "https://u/1", "last_edited_time": "2026-10-11T00:00:00.000Z",
                 "properties": {"Name": {"type": "title", "title": [{"plain_text": "Tasks"}]}}},
                {"object": "data_source", "id": "ds-1", "url": "https://u/2",
                 "title": [{"plain_text": "Tasks DB"}]}
            ]}"#,
        )]);
        let result = answered(call(
            "searchWorkspace",
            r#"{"query": "tasks", "filter": "page", "pageSize": 5}"#,
            &mut stub,
        ));
        assert_eq!(stub.seen[0].url, format!("{API}/v1/search"));
        let body = body_json(&stub.seen[0]);
        assert_eq!(body["query"], json!("tasks"));
        assert_eq!(
            body["filter"],
            json!({"property": "object", "value": "page"})
        );
        assert_eq!(body["page_size"], json!(5));
        assert_eq!(result["results"][0]["type"], json!("page"));
        assert_eq!(result["results"][0]["title"], json!("Tasks"));
        assert_eq!(result["results"][1]["type"], json!("dataSource"));

        let mut stub = Stub::of(&[ok_json(r#"{"results": []}"#)]);
        let _ = call(
            "searchWorkspace",
            r#"{"query": "x", "filter": "dataSource"}"#,
            &mut stub,
        );
        assert_eq!(
            body_json(&stub.seen[0])["filter"],
            json!({"property": "object", "value": "data_source"})
        );
        let mut stub = Stub::of(&[ok_json(r#"{"results": []}"#)]);
        let _ = call(
            "searchWorkspace",
            r#"{"query": "x", "filter": "all"}"#,
            &mut stub,
        );
        assert!(body_json(&stub.seen[0]).get("filter").is_none());
        let mut stub = Stub::of(&[ok_json(r#"{"results": []}"#)]);
        let _ = call("searchWorkspace", r#"{"query": "x"}"#, &mut stub);
        assert!(body_json(&stub.seen[0]).get("filter").is_none());
        assert!(body_json(&stub.seen[0]).get("page_size").is_none());
    }

    #[test]
    fn search_surfaces_api_errors() {
        let mut stub = Stub::of(&[response(401, "unauthorized")]);
        assert!(is_tool_error(&call(
            "searchWorkspace",
            r#"{"query": "x"}"#,
            &mut stub
        )));
        let mut stub = Stub::of(&[response(200, "not json")]);
        assert!(is_tool_error(&call(
            "searchWorkspace",
            r#"{"query": "x"}"#,
            &mut stub
        )));
    }

    #[test]
    fn unknown_tools_are_not_found() {
        let mut stub = Stub::of(&[]);
        let result = call("nope", "{}", &mut stub);
        assert!(matches!(result, Err(ConnectorError::NotFound(_)))); // coverage: exclude — matches! false arm is unreachable in a passing test
    }

    #[test]
    fn refuses_bad_input() {
        // Every tool parses its input before anything else (getCurrentUser
        // takes no input); malformed JSON must fail fast as a tool error
        // and send nothing.
        for tool in [
            "search",
            "getPage",
            "createPage",
            "updatePage",
            "movePage",
            "queryDataSource",
            "getDataSource",
            "createDataSource",
            "appendContent",
            "listComments",
            "createComment",
        ] {
            let mut stub = Stub::of(&[]);
            let result = call(tool, "{oops", &mut stub);
            assert!(is_tool_error(&result), "{tool}: {result:?}");
            assert!(stub.seen.is_empty(), "{tool}: sent a request for bad input");
        }
    }

    #[test]
    fn rejects_unresolvable_titles() {
        // A title that is not an id triggers a workspace search; when the
        // search itself fails, or finds nothing to pick, resolution errors.
        let empty_search = ok_json(r#"{"results": []}"#);

        // queryDataSource: search failure, then empty search results.
        for stub in [Stub::of(&[]), Stub::of(&[empty_search.clone()])] {
            let mut stub = stub;
            let result = call(
                "queryDataSource",
                r#"{"dataSource": "No Such Source"}"#,
                &mut stub,
            );
            assert!(is_tool_error(&result), "{result:?}");
        }

        // getDataSource: search failure.
        let mut stub = Stub::of(&[]);
        let result = call(
            "getDataSource",
            r#"{"dataSource": "No Such Source"}"#,
            &mut stub,
        );
        assert!(is_tool_error(&result), "{result:?}");

        // createDataSource: parent page search failure, then empty results.
        let body = r#"{"parent": "No Such Page", "title": "T", "properties": [{"name": "N", "type": "title"}]}"#;
        for stub in [Stub::of(&[]), Stub::of(&[empty_search.clone()])] {
            let mut stub = stub;
            let result = call("createDataSource", body, &mut stub);
            assert!(is_tool_error(&result), "{result:?}");
        }

        // createPage: parent page search failure.
        let mut stub = Stub::of(&[]);
        let result = call(
            "createPage",
            r#"{"parent": {"page": "No Such Page"}}"#,
            &mut stub,
        );
        assert!(is_tool_error(&result), "{result:?}");
    }

    #[test]
    fn rejects_bad_query_filters_and_sorts() {
        // build_filter and build_sorts fail through queryDataSource once the
        // data source resolves by id and its schema reads back.
        let id = "12345678-1234-1234-1234-123456789abc";
        let schema = ok_json(r#"{"properties": {"Name": {"type": "title"}}}"#);

        let mut stub = Stub::of(&[schema.clone()]);
        let result = call(
            "queryDataSource",
            &format!(r#"{{"dataSource": "{id}", "filter": {{"Bogus": "x"}}}}"#),
            &mut stub,
        );
        assert!(is_tool_error(&result), "{result:?}");

        let mut stub = Stub::of(&[schema]);
        let result = call(
            "queryDataSource",
            &format!(r#"{{"dataSource": "{id}", "sorts": [{{"direction": "sideways"}}]}}"#),
            &mut stub,
        );
        assert!(is_tool_error(&result), "{result:?}");
    }

    #[test]
    fn rejects_unknown_properties() {
        // map_properties fails through createPage and updatePage when a
        // property is not writable there.
        let id = "12345678-1234-1234-1234-123456789abc";

        let mut stub = Stub::of(&[]);
        let result = call(
            "createPage",
            &format!(r#"{{"parent": {{"page": "{id}"}}, "properties": {{"Bogus": "x"}}}}"#),
            &mut stub,
        );
        assert!(is_tool_error(&result), "{result:?}");

        // The page lives outside a data source, so only the title writes.
        let mut stub = Stub::of(&[ok_json(&format!(
            r#"{{"id": "{id}", "parent": {{"type": "page_id"}}}}"#
        ))]);
        let result = call(
            "updatePage",
            &format!(r#"{{"pageId": "{id}", "properties": {{"Bogus": "x"}}}}"#),
            &mut stub,
        );
        assert!(is_tool_error(&result), "{result:?}");
    }

    #[test]
    fn surfaces_failing_content_append() {
        // createPage creates the page, then the content append fails.
        let id = "12345678-1234-1234-1234-123456789abc";
        let mut stub = Stub::of(&[ok_json(&format!(r#"{{"id": "{id}"}}"#))]);
        let result = call(
            "createPage",
            &format!(r#"{{"parent": {{"page": "{id}"}}, "content": "hello"}}"#),
            &mut stub,
        );
        assert!(is_tool_error(&result), "{result:?}");
    }

    #[test]
    fn call_tool_sends_through_the_guest() {
        // The guest has no network off-host: every tool errors at send time,
        // which also covers the production sender closure. Ids pass
        // resolution untouched, so each tool makes exactly one call.
        let id = "12345678-1234-1234-1234-123456789abc";
        let calls = [
            ("searchWorkspace", r#"{"query": "x"}"#.to_string()),
            ("getPage", format!(r#"{{"pageId": "{id}"}}"#)),
            ("createPage", format!(r#"{{"parent": {{"page": "{id}"}}}}"#)),
            (
                "updatePage",
                format!(r#"{{"pageId": "{id}", "trash": true}}"#),
            ),
            (
                "movePage",
                format!(r#"{{"pageId": "{id}", "parent": "{id}"}}"#),
            ),
            ("queryDataSource", format!(r#"{{"dataSource": "{id}"}}"#)),
            ("getDataSource", format!(r#"{{"dataSource": "{id}"}}"#)),
            (
                "createDataSource",
                format!(
                    r#"{{"parent": "{id}", "title": "T", "properties": [{{"name": "N", "type": "title"}}]}}"#
                ),
            ),
            (
                "appendContent",
                format!(r#"{{"pageId": "{id}", "markdown": "x"}}"#),
            ),
            ("listComments", format!(r#"{{"pageId": "{id}"}}"#)),
            (
                "createComment",
                format!(r#"{{"pageId": "{id}", "text": "x"}}"#),
            ),
            ("getCurrentUser", "{}".to_string()),
        ];
        for (tool, body) in calls {
            let result = Notion::call_tool(request(tool, &body));
            assert!(is_tool_error(&result), "{tool}: {result:?}");
        }
    }

    #[test]
    fn manifest_is_embedded() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["name"], "notion");
        assert_eq!(manifest["tools"].as_array().unwrap().len(), 12);
    }
}

#[cfg(test)]
mod property_tests {
    use super::*;

    fn prop(prop_type: &str, value: Value) -> Value {
        let mut prop = Map::new();
        prop.insert("type".to_string(), json!(prop_type));
        prop.insert(prop_type.to_string(), value);
        Value::Object(prop)
    }

    #[test]
    fn simplifies_every_property_type() {
        let cases: &[(&str, Value, Value)] = &[
            ("title", json!([{"plain_text": "T"}]), json!("T")),
            ("rich_text", json!([{"plain_text": "R"}]), json!("R")),
            ("number", json!(3), json!(3)),
            ("number", json!(null), json!(null)),
            ("checkbox", json!(true), json!(true)),
            ("url", json!("https://a.test"), json!("https://a.test")),
            ("email", json!("a@test"), json!("a@test")),
            ("phone_number", json!("1"), json!("1")),
            ("select", json!({"name": "Done"}), json!("Done")),
            ("select", json!(null), json!(null)),
            ("status", json!({"name": "Todo"}), json!("Todo")),
            (
                "multi_select",
                json!([{"name": "a"}, {"name": "b"}, {"nope": 1}]),
                json!(["a", "b"]),
            ),
            ("multi_select", json!(null), json!([])),
            (
                "date",
                json!({"start": "2026-10-11", "end": "2026-10-12"}),
                json!({"start": "2026-10-11", "end": "2026-10-12"}),
            ),
            (
                "date",
                json!({"start": "2026-10-11", "end": null}),
                json!("2026-10-11"),
            ),
            ("date", json!(null), json!(null)),
            (
                "people",
                json!([{"id": "u1", "name": "Ann"}, {"id": "u2"}]),
                json!([{"id": "u1", "name": "Ann"}, {"id": "u2"}]),
            ),
            ("people", json!(null), json!([])),
            (
                "relation",
                json!([{"id": "p1"}, {"id": "p2"}, {"nope": 1}]),
                json!(["p1", "p2"]),
            ),
            (
                "files",
                json!([
                    {"name": "a", "external": {"url": "https://a.test/a"}},
                    {"name": "b", "file": {"url": "https://b.test/b"}},
                    {"name": "c"},
                ]),
                json!([
                    {"name": "a", "url": "https://a.test/a"},
                    {"name": "b", "url": "https://b.test/b"},
                    {"name": "c"},
                ]),
            ),
            ("formula", json!({"type": "number", "number": 7}), json!(7)),
            (
                "formula",
                json!({"type": "string", "string": "x"}),
                json!("x"),
            ),
            (
                "rollup",
                json!({
                    "type": "array",
                    "array": [
                        {"type": "title", "title": [{"plain_text": "a"}]},
                        {"type": "number", "number": 1},
                    ]
                }),
                json!(["a", 1]),
            ),
            ("rollup", json!({"type": "number", "number": 9}), json!(9)),
            ("created_time", json!("2026-01-01"), json!("2026-01-01")),
            ("last_edited_time", json!("2026-01-02"), json!("2026-01-02")),
            (
                "created_by",
                json!({"id": "u1", "name": "Ann"}),
                json!({"id": "u1", "name": "Ann"}),
            ),
            ("last_edited_by", json!({"id": "u1"}), json!({"id": "u1"})),
            (
                "unique_id",
                json!({"prefix": "TASK", "number": 12}),
                json!("TASK-12"),
            ),
            ("unique_id", json!({"prefix": "TASK"}), json!(null)),
            ("button", json!({"x": 1}), json!({"x": 1})),
        ];
        for (prop_type, value, expected) in cases {
            assert_eq!(
                simplify_property(&prop(prop_type, value.clone())),
                *expected,
                "type {prop_type}"
            );
        }
        assert_eq!(simplify_property(&json!({"nope": 1})), json!(null));
    }

    #[test]
    fn simplifies_pages() {
        let page = json!({
            "id": "page-1",
            "url": "https://u/1",
            "created_time": "2026-10-01T00:00:00.000Z",
            "last_edited_time": "2026-10-11T00:00:00.000Z",
            "in_trash": false,
            "parent": {"type": "page_id", "page_id": "p0"},
            "properties": {
                "Name": {"type": "title", "title": [{"plain_text": "T"}]},
                "Done": {"type": "checkbox", "checkbox": true},
            }
        });
        let simplified = simplify_page(&page);
        assert_eq!(simplified["id"], json!("page-1"));
        assert_eq!(simplified["title"], json!("T"));
        assert_eq!(simplified["properties"]["Done"], json!(true));
        assert_eq!(simplified["inTrash"], json!(false));
        let bare = simplify_page(&json!({"id": "x"}));
        assert_eq!(bare["properties"], json!({}));
        assert!(bare.get("title").is_none());
    }

    #[test]
    fn reads_schema_types() {
        let schema = json!({
            "properties": {
                "Name": {"type": "title"},
                "Status": {"type": "status"},
                "Weird": {"nope": 1},
            }
        });
        let types = schema_types(&schema);
        assert_eq!(types.get("Name").map(String::as_str), Some("title"));
        assert_eq!(types.get("Status").map(String::as_str), Some("status"));
        assert_eq!(types.get("Weird"), None);
        assert!(schema_types(&json!({})).is_empty());
    }

    #[test]
    fn maps_plain_values_through_the_schema() {
        let schema = json!({
            "properties": {
                "Name": {"type": "title"},
                "Notes": {"type": "rich_text"},
                "Score": {"type": "number"},
                "Done": {"type": "checkbox"},
                "Status": {"type": "status"},
                "Tags": {"type": "multi_select"},
                "Due": {"type": "date"},
                "Link": {"type": "url"},
                "Mail": {"type": "email"},
                "Phone": {"type": "phone_number"},
                "Owner": {"type": "people"},
                "Ref": {"type": "relation"},
            }
        });
        let mapped = map_properties(
            Some(&schema),
            &json!({
                "Name": "T", "Notes": "N", "Score": 3, "Done": true,
                "Status": "Done", "Tags": ["a", "b"], "Due": "2026-10-11",
                "Link": "https://a.test", "Mail": "a@test", "Phone": "1",
                "Owner": ["u1"], "Ref": ["p1"], "Skipped": null,
            }),
        )
        .unwrap();
        assert_eq!(
            mapped["Name"],
            json!({"title": [{"text": {"content": "T"}}]})
        );
        assert_eq!(mapped["Score"], json!({"number": 3}));
        assert_eq!(mapped["Done"], json!({"checkbox": true}));
        assert_eq!(mapped["Status"], json!({"status": {"name": "Done"}}));
        assert_eq!(
            mapped["Tags"],
            json!({"multi_select": [{"name": "a"}, {"name": "b"}]})
        );
        assert_eq!(mapped["Due"], json!({"date": {"start": "2026-10-11"}}));
        assert_eq!(mapped["Owner"], json!({"people": [{"id": "u1"}]}));
        assert!(mapped.get("Skipped").is_none());

        let error = map_properties(Some(&schema), &json!({"Missing": "x"}))
            .unwrap_err()
            .0;
        assert!(error.contains("Missing") && error.contains("Status"));
        let error = map_properties(Some(&schema), &json!("nope")).unwrap_err().0;
        assert!(error.contains("must be an object"));

        // Outside a data source only the title is writable.
        let mapped = map_properties(None, &json!({"title": "T"})).unwrap();
        assert_eq!(
            mapped["title"],
            json!({"title": [{"text": {"content": "T"}}]})
        );
        assert!(map_properties(None, &json!({"Name": "T"})).is_err());
    }

    #[test]
    fn rejects_bad_plain_values() {
        let schema = json!({
            "properties": {
                "Name": {"type": "title"},
                "Score": {"type": "number"},
                "Done": {"type": "checkbox"},
                "Status": {"type": "select"},
                "Tags": {"type": "multi_select"},
                "Due": {"type": "date"},
                "Link": {"type": "url"},
                "Owner": {"type": "people"},
                "Calc": {"type": "formula"},
            }
        });
        let bad = [
            (r#"{"Name": 1}"#, "title needs a string"),
            (r#"{"Score": "x"}"#, "number needs a number"),
            (r#"{"Done": "x"}"#, "checkbox needs a boolean"),
            (r#"{"Status": 1}"#, "select needs an option name string"),
            (r#"{"Tags": "x"}"#, "multi_select needs an array"),
            (
                r#"{"Tags": [1]}"#,
                "multi_select needs an array of option name strings",
            ),
            (r#"{"Due": 1}"#, "date needs a YYYY-MM-DD string"),
            (
                r#"{"Due": {"end": "2026-10-12"}}"#,
                "date needs a start string",
            ),
            (r#"{"Link": 1}"#, "url needs a string"),
            (r#"{"Owner": "x"}"#, "people needs an array of id strings"),
            (r#"{"Owner": [1]}"#, "people needs an array of id strings"),
            (
                r#"{"Calc": 1}"#,
                "writing formula properties is not supported",
            ),
        ];
        for (properties, message) in bad {
            let error = map_properties(Some(&schema), &serde_json::from_str(properties).unwrap())
                .unwrap_err()
                .0;
            assert!(error.contains(message), "{properties}: {error}");
        }
        // Date ranges map start/end.
        let mapped = map_properties(
            Some(&schema),
            &json!({"Due": {"start": "2026-10-11", "end": "2026-10-12"}}),
        )
        .unwrap();
        assert_eq!(
            mapped["Due"],
            json!({"date": {"start": "2026-10-11", "end": "2026-10-12"}})
        );
        let mapped =
            map_properties(Some(&schema), &json!({"Due": {"start": "2026-10-11"}})).unwrap();
        assert_eq!(mapped["Due"]["date"]["end"], json!(null));
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    fn schema() -> Value {
        json!({
            "properties": {
                "Name": {"type": "title"},
                "Status": {"type": "status"},
                "Tags": {"type": "multi_select"},
                "Due": {"type": "date"},
                "Score": {"type": "number"},
                "Done": {"type": "checkbox"},
                "Owner": {"type": "people"},
                "Files": {"type": "files"},
                "Calc": {"type": "formula"},
            }
        })
    }

    #[test]
    fn builds_plain_filters() {
        let schema = schema();
        assert_eq!(
            build_filter(&schema, &json!({"Status": "Done"})).unwrap(),
            json!({"property": "Status", "status": {"equals": "Done"}})
        );
        assert_eq!(
            build_filter(&schema, &json!({"Due": {"before": "2026-12-01"}})).unwrap(),
            json!({"property": "Due", "date": {"before": "2026-12-01"}})
        );
        assert_eq!(
            build_filter(&schema, &json!({"Status": "Done", "Done": true})).unwrap(),
            json!({"and": [
                {"property": "Done", "checkbox": {"equals": true}},
                {"property": "Status", "status": {"equals": "Done"}},
            ]})
        );
        assert_eq!(
            build_filter(
                &schema,
                &json!({"or": [{"Status": "Done"}, {"Status": "Todo"}]})
            )
            .unwrap(),
            json!({"or": [
                {"property": "Status", "status": {"equals": "Done"}},
                {"property": "Status", "status": {"equals": "Todo"}},
            ]})
        );
        assert_eq!(
            build_filter(&schema, &json!({"and": [{"or": [{"Status": "Done"}]}]})).unwrap(),
            json!({"and": [{"or": [{"property": "Status", "status": {"equals": "Done"}}]}]})
        );
    }

    #[test]
    fn rejects_bad_filters() {
        let schema = schema();
        let bad = [
            (r#"{"Missing": "x"}"#, "no property \"Missing\""),
            (r#""nope""#, "filter must be an object"),
            (r#"{}"#, "filter must not be empty"),
            (
                r#"{"Status": {"equals": "x", "contains": "y"}}"#,
                "exactly one operator",
            ),
            (
                r#"{"Status": {"bogus": "x"}}"#,
                "do not support the bogus operator",
            ),
            (
                r#"{"Status": {"before": "x"}}"#,
                "do not support the before operator",
            ),
            (
                r#"{"Calc": "x"}"#,
                "filtering on formula properties is not supported",
            ),
            (r#"{"and": "x"}"#, "and needs an array"),
            (r#"{"and": []}"#, "and needs at least one condition"),
            (r#"{"or": []}"#, "or needs at least one condition"),
            (r#"{"and": [{"Missing": "x"}]}"#, "no property \"Missing\""),
        ];
        for (filter, message) in bad {
            let error = build_filter(&schema, &serde_json::from_str(filter).unwrap())
                .unwrap_err()
                .0;
            assert!(error.contains(message), "{filter}: {error}");
        }
    }

    #[test]
    fn every_property_type_has_operators() {
        for prop_type in [
            "title",
            "rich_text",
            "url",
            "email",
            "phone_number",
            "number",
            "unique_id",
            "checkbox",
            "select",
            "status",
            "multi_select",
            "date",
            "created_time",
            "last_edited_time",
            "people",
            "created_by",
            "last_edited_by",
            "relation",
            "files",
        ] {
            assert!(
                supported_operators(prop_type).is_some_and(|ops| !ops.is_empty()),
                "{prop_type}"
            );
        }
        assert_eq!(supported_operators("button"), None);
        assert_eq!(supported_operators(""), None);
    }

    #[test]
    fn builds_sorts() {
        assert_eq!(
            build_sorts(&json!([{"property": "Due"}])).unwrap(),
            json!([{"property": "Due", "direction": "ascending"}])
        );
        assert_eq!(
            build_sorts(&json!([{"property": "Due", "direction": "descending"}])).unwrap(),
            json!([{"property": "Due", "direction": "descending"}])
        );
        assert_eq!(
            build_sorts(&json!([{"timestamp": "created_time", "direction": "descending"}]))
                .unwrap(),
            json!([{"timestamp": "created_time", "direction": "descending"}])
        );
        let bad = [
            (r#""nope""#, "sorts must be an array"),
            (r#"["x"]"#, "each sort must be an object"),
            (
                r#"[{"property": "Due", "direction": "sideways"}]"#,
                "sort direction must be ascending or descending",
            ),
            (
                r#"[{"timestamp": "edited"}]"#,
                "sort timestamp must be created_time or last_edited_time",
            ),
            (r#"[{}]"#, "each sort needs a property or a timestamp"),
        ];
        for (sorts, message) in bad {
            let error = build_sorts(&serde_json::from_str(sorts).unwrap())
                .unwrap_err()
                .0;
            assert!(error.contains(message), "{sorts}: {error}");
        }
    }
}

#[cfg(test)]
mod markdown_tests {
    use super::*;

    fn block_type(block: &Value) -> &str {
        block.get("type").and_then(Value::as_str).unwrap_or("")
    }

    fn rich_text(block: &Value) -> &Value {
        &block[block_type(block)]["rich_text"]
    }

    #[test]
    fn converts_block_kinds() {
        let blocks = markdown_to_blocks(
            "# H1\n## H2\n### H3\n\nA paragraph\nwith two lines.\n\n- bullet\n* star\n1. first\n2. second\n- [ ] todo\n- [x] done\n- [X] done2\n> quote\n\n---\n***\n___\n",
        );
        let kinds: Vec<&str> = blocks.iter().map(block_type).collect();
        assert_eq!(
            kinds,
            [
                "heading_1",
                "heading_2",
                "heading_3",
                "paragraph",
                "bulleted_list_item",
                "bulleted_list_item",
                "numbered_list_item",
                "numbered_list_item",
                "to_do",
                "to_do",
                "to_do",
                "quote",
                "divider",
                "divider",
                "divider",
            ]
        );
        assert_eq!(rich_text(&blocks[0])[0]["text"]["content"], json!("H1"));
        assert_eq!(
            blocks[3]["paragraph"]["rich_text"][0]["text"]["content"],
            json!("A paragraph\nwith two lines.")
        );
        assert_eq!(blocks[8]["to_do"]["checked"], json!(false));
        assert_eq!(blocks[9]["to_do"]["checked"], json!(true));
        assert_eq!(blocks[10]["to_do"]["checked"], json!(true));
    }

    #[test]
    fn converts_code_fences() {
        let blocks =
            markdown_to_blocks("```rust\nfn main() {}\n```\n\n```\nplain\n```\n\n```unclosed\nx\n");
        assert_eq!(block_type(&blocks[0]), "code");
        assert_eq!(blocks[0]["code"]["language"], json!("rust"));
        assert_eq!(
            blocks[0]["code"]["rich_text"][0]["text"]["content"],
            json!("fn main() {}")
        );
        assert_eq!(blocks[1]["code"]["language"], json!("plain text"));
        assert_eq!(blocks[2]["code"]["language"], json!("unclosed"));
        // Inline code stays literal inside fences.
        let blocks = markdown_to_blocks("```\n**not bold**\n```");
        assert_eq!(
            blocks[0]["code"]["rich_text"][0]["text"]["content"],
            json!("**not bold**")
        );
    }

    #[test]
    fn maps_fence_languages() {
        for (tag, language) in [
            ("js", "javascript"),
            ("ts", "typescript"),
            ("py", "python"),
            ("sh", "bash"),
            ("shell", "bash"),
            ("zsh", "bash"),
            ("yml", "yaml"),
            ("md", "markdown"),
            ("", "plain text"),
            ("   ", "plain text"),
            ("Rust", "Rust"),
            ("haskell", "haskell"),
        ] {
            assert_eq!(code_language(tag), language, "{tag}");
        }
    }

    #[test]
    fn numbered_item_needs_digits_and_a_dot() {
        assert_eq!(numbered_item("1. x"), Some("x"));
        assert_eq!(numbered_item("12. x"), Some("x"));
        assert_eq!(numbered_item("a. x"), None);
        assert_eq!(numbered_item(". x"), None);
        assert_eq!(numbered_item("1.x"), None);
        assert_eq!(numbered_item("1. "), Some(""));
    }

    #[test]
    fn parses_inline_formatting() {
        let items = rich_text_of("**bold** and *italic* and `code` and ~~strike~~");
        let texts: Vec<&str> = items
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["text"]["content"].as_str().unwrap())
            .collect();
        assert_eq!(
            texts,
            [
                "bold", " and ", "italic", " and ", "code", " and ", "strike"
            ]
        );
        let annotations: Vec<&Value> = items
            .as_array()
            .unwrap()
            .iter()
            .map(|item| &item["annotations"])
            .collect();
        assert_eq!(annotations[0]["bold"], json!(true));
        assert_eq!(annotations[2]["italic"], json!(true));
        assert_eq!(annotations[4]["code"], json!(true));
        assert_eq!(annotations[6]["strikethrough"], json!(true));
        assert_eq!(annotations[1]["bold"], json!(false));

        let items = rich_text_of("a [link](https://a.test/x) b");
        assert_eq!(items[1]["text"]["content"], json!("link"));
        assert_eq!(items[1]["text"]["link"], json!({"url": "https://a.test/x"}));
        assert_eq!(items[0]["text"]["content"], json!("a "));
        assert!(items[0].get("text").unwrap().get("link").is_none());

        // A '[' that does not open a link stays literal.
        let items = rich_text_of("a [b c");
        assert_eq!(items.as_array().unwrap().len(), 1);
        assert_eq!(items[0]["text"]["content"], json!("a [b c"));
        let items = rich_text_of("a [b](c");
        assert_eq!(items[0]["text"]["content"], json!("a [b](c"));

        assert_eq!(rich_text_of(""), json!([]));
        assert_eq!(
            rich_text_of("plain").as_array().unwrap()[0]["text"]["content"],
            json!("plain")
        );
    }

    #[test]
    fn parse_link_needs_brackets_and_parens() {
        assert_eq!(
            parse_link("[a](b)").unwrap(),
            ("a".to_string(), "b".to_string(), 6)
        );
        assert_eq!(parse_link("[a]b"), None);
        assert_eq!(parse_link("[a](b"), None);
        assert_eq!(parse_link("[](x)").unwrap().0, "");
    }

    #[test]
    fn empty_markdown_has_no_blocks() {
        assert!(markdown_to_blocks("").is_empty());
        assert!(markdown_to_blocks("\n\n  \n").is_empty());
    }

    #[test]
    fn annotations_serialize() {
        let annotations = Annotations {
            bold: true,
            italic: true,
            strikethrough: true,
            code: true,
        };
        assert_eq!(
            annotations.to_json(),
            json!({"bold": true, "italic": true, "strikethrough": true, "code": true})
        );
        assert_eq!(
            Annotations::default().to_json(),
            json!({"bold": false, "italic": false, "strikethrough": false, "code": false})
        );
    }
}

#[cfg(test)]
mod runner_tests {
    use super::*;
    use std::collections::VecDeque;

    const PAGE_ID: &str = "12345678-1234-1234-1234-123456789abc";
    const DS_ID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

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
        Notion::call_with(request(tool, body), &mut |request| stub.send(request))
    }

    fn is_tool_error(result: &Result<ToolResponse, ConnectorError>) -> bool {
        matches!(result, Ok(response) if response.structured.is_none())
    }

    fn answered(result: Result<ToolResponse, ConnectorError>) -> Value {
        let response = result.unwrap();
        serde_json::from_str(response.structured.as_deref().unwrap()).unwrap()
    }

    fn body_json(request: &Request) -> Value {
        serde_json::from_slice(&request.body).unwrap()
    }

    fn page_json() -> String {
        format!(
            r#"{{
            "object": "page", "id": "{PAGE_ID}",
            "created_time": "2026-10-01T10:00:00.000Z",
            "last_edited_time": "2026-10-11T10:00:00.000Z",
            "in_trash": false,
            "url": "https://www.notion.so/x",
            "parent": {{"type": "data_source_id", "data_source_id": "{DS_ID}"}},
            "properties": {{
                "Name": {{"type": "title", "title": [{{"plain_text": "Write the post"}}]}},
                "Status": {{"type": "status", "status": {{"name": "In progress"}}}}
            }}
        }}"#
        )
    }

    fn schema_json() -> String {
        format!(
            r#"{{
            "object": "data_source", "id": "{DS_ID}",
            "title": [{{"plain_text": "Tasks"}}],
            "properties": {{
                "Name": {{"type": "title", "title": {{}}}},
                "Status": {{"type": "status", "status": {{"options": [{{"name": "Todo"}}, {{"name": "Done"}}]}}}},
                "Score": {{"type": "number", "number": {{"format": "number"}}}},
                "Due": {{"type": "date", "date": {{}}}}
            }}
        }}"#
        )
    }

    fn search_data_source_json() -> String {
        format!(
            r#"{{"results": [{{
            "object": "data_source", "id": "{DS_ID}",
            "title": [{{"plain_text": "Tasks"}}]
        }}]}}"#
        )
    }

    #[test]
    fn get_page_reads_plain_properties() {
        let mut stub = Stub::of(&[ok_json(&page_json())]);
        let result = answered(call(
            "getPage",
            &format!(r#"{{"pageId": "{PAGE_ID}"}}"#),
            &mut stub,
        ));
        assert_eq!(stub.seen.len(), 1);
        assert!(stub.seen[0].url.ends_with(&format!("/v1/pages/{PAGE_ID}")));
        assert_eq!(result["title"], json!("Write the post"));
        assert_eq!(result["properties"]["Status"], json!("In progress"));
        assert!(result.get("content").is_none());
    }

    #[test]
    fn get_page_includes_markdown_content() {
        let mut stub = Stub::of(&[ok_json(&page_json()), ok_json(r##"{"markdown": "# Hi"}"##)]);
        let result = answered(call(
            "getPage",
            &format!(r#"{{"pageId": "{PAGE_ID}", "includeContent": true}}"#),
            &mut stub,
        ));
        assert_eq!(stub.seen.len(), 2);
        assert!(stub.seen[1].url.ends_with("/markdown"));
        assert_eq!(result["content"], json!("# Hi"));

        let mut stub = Stub::of(&[ok_json(&page_json()), ok_json(r##""# Raw""##)]);
        let result = answered(call(
            "getPage",
            &format!(r#"{{"pageId": "{PAGE_ID}", "includeContent": true}}"#),
            &mut stub,
        ));
        assert_eq!(result["content"], json!("# Raw"));

        let mut stub = Stub::of(&[ok_json(&page_json()), ok_json(r#"{"x": 1}"#)]);
        assert!(is_tool_error(&call(
            "getPage",
            &format!(r#"{{"pageId": "{PAGE_ID}", "includeContent": true}}"#),
            &mut stub,
        )));

        let mut stub = Stub::of(&[ok_json(&page_json()), response(404, "gone")]);
        assert!(is_tool_error(&call(
            "getPage",
            &format!(r#"{{"pageId": "{PAGE_ID}", "includeContent": true}}"#),
            &mut stub,
        )));
    }

    #[test]
    fn create_page_resolves_titles_and_appends_content() {
        let mut stub = Stub::of(&[
            ok_json(&search_data_source_json()),
            ok_json(&schema_json()),
            ok_json(&page_json()),
            ok_json(r#"{"results": []}"#),
        ]);
        let result = answered(call(
            "createPage",
            r##"{"parent": {"dataSource": "Tasks"}, "properties": {"Name": "T", "Status": "Done"}, "content": "# Hi"}"##,
            &mut stub,
        ));
        assert_eq!(stub.seen.len(), 4);
        assert_eq!(stub.seen[0].url, "https://api.notion.com/v1/search");
        assert!(
            stub.seen[1]
                .url
                .ends_with(&format!("/v1/data_sources/{DS_ID}"))
        );
        let created = body_json(&stub.seen[2]);
        assert_eq!(created["parent"]["data_source_id"], json!(DS_ID));
        assert_eq!(
            created["properties"]["Name"],
            json!({"title": [{"text": {"content": "T"}}]})
        );
        assert_eq!(
            created["properties"]["Status"],
            json!({"status": {"name": "Done"}})
        );
        let appended = body_json(&stub.seen[3]);
        assert_eq!(appended["children"][0]["type"], json!("heading_1"));
        assert_eq!(result["title"], json!("Write the post"));
    }

    #[test]
    fn running_out_of_stubbed_responses_fails_the_tool() {
        let mut stub = Stub::of(&[ok_json(&search_data_source_json())]);
        assert!(is_tool_error(&call(
            "createPage",
            r#"{"parent": {"dataSource": "Tasks"}}"#,
            &mut stub,
        )));
    }

    #[test]
    fn create_page_accepts_a_parent_page_and_skips_extras() {
        let mut stub = Stub::of(&[ok_json(&page_json())]);
        let result = answered(call(
            "createPage",
            &format!(r#"{{"parent": {{"page": "{PAGE_ID}"}}}}"#),
            &mut stub,
        ));
        assert_eq!(stub.seen.len(), 1);
        let created = body_json(&stub.seen[0]);
        assert_eq!(created["parent"]["page_id"], json!(PAGE_ID));
        assert!(created.get("properties").is_none());
        assert_eq!(result["id"], json!(PAGE_ID));
    }

    #[test]
    fn create_page_rejects_bad_parents_and_idless_answers() {
        let mut stub = Stub::of(&[]);
        assert!(is_tool_error(&call(
            "createPage",
            r#"{"parent": {}}"#,
            &mut stub
        )));
        assert_eq!(stub.seen.len(), 0);
        let mut stub = Stub::of(&[]);
        assert!(is_tool_error(&call(
            "createPage",
            r#"{"parent": {"dataSource": "x", "page": "y"}}"#,
            &mut stub,
        )));
        // A search hit without an id cannot be resolved.
        let mut stub = Stub::of(&[ok_json(
            r#"{"results": [{"object": "data_source", "title": [{"plain_text": "Tasks"}]}]}"#,
        )]);
        assert!(is_tool_error(&call(
            "createPage",
            r#"{"parent": {"dataSource": "Tasks"}}"#,
            &mut stub,
        )));
        // A failing schema fetch fails the tool.
        let mut stub = Stub::of(&[ok_json(&search_data_source_json()), response(404, "gone")]);
        assert!(is_tool_error(&call(
            "createPage",
            r#"{"parent": {"dataSource": "Tasks"}}"#,
            &mut stub,
        )));

        // A created page without an id cannot take content.
        let mut stub = Stub::of(&[
            ok_json(&search_data_source_json()),
            ok_json(&schema_json()),
            ok_json(r#"{"object": "page"}"#),
        ]);
        assert!(is_tool_error(&call(
            "createPage",
            r##"{"parent": {"dataSource": "Tasks"}, "content": "# Hi"}"##,
            &mut stub,
        )));
    }

    #[test]
    fn update_page_writes_properties_and_trashes() {
        let mut stub = Stub::of(&[
            ok_json(&page_json()),
            ok_json(&schema_json()),
            ok_json(&page_json()),
        ]);
        let result = answered(call(
            "updatePage",
            &format!(
                r#"{{"pageId": "{PAGE_ID}", "properties": {{"Status": "Done"}}, "trash": true}}"#
            ),
            &mut stub,
        ));
        assert_eq!(stub.seen.len(), 3);
        let updated = body_json(&stub.seen[2]);
        assert_eq!(
            updated["properties"]["Status"],
            json!({"status": {"name": "Done"}})
        );
        assert_eq!(updated["in_trash"], json!(true));
        assert_eq!(result["id"], json!(PAGE_ID));

        // Trash alone needs no schema lookup.
        let mut stub = Stub::of(&[ok_json(&page_json())]);
        let _ = answered(call(
            "updatePage",
            &format!(r#"{{"pageId": "{PAGE_ID}", "trash": false}}"#),
            &mut stub,
        ));
        assert_eq!(stub.seen.len(), 1);
        assert_eq!(body_json(&stub.seen[0])["in_trash"], json!(false));

        // A page outside a data source only takes a title.
        let plain = page_json().replace(
            r#""parent": {"type": "data_source_id", "data_source_id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"}"#,
            r#""parent": {"type": "page_id", "page_id": "parent-page"}"#,
        );
        let mut stub = Stub::of(&[ok_json(&plain), ok_json(&plain)]);
        let _ = answered(call(
            "updatePage",
            &format!(r#"{{"pageId": "{PAGE_ID}", "properties": {{"title": "T"}}}}"#),
            &mut stub,
        ));
        assert_eq!(stub.seen.len(), 2);

        // A failing page fetch fails the tool.
        let mut stub = Stub::of(&[response(404, "gone")]);
        assert!(is_tool_error(&call(
            "updatePage",
            &format!(r#"{{"pageId": "{PAGE_ID}", "properties": {{"Status": "Done"}}}}"#),
            &mut stub,
        )));

        // Nothing to update fails before any call.
        let mut stub = Stub::of(&[]);
        assert!(is_tool_error(&call(
            "updatePage",
            &format!(r#"{{"pageId": "{PAGE_ID}"}}"#),
            &mut stub,
        )));
        assert!(stub.seen.is_empty());
    }

    #[test]
    fn move_page_fails_when_the_parent_hit_has_no_id() {
        let mut stub = Stub::of(&[ok_json(
            r#"{"results": [{"object": "page", "properties": {"Name": {"type": "title", "title": [{"plain_text": "Home"}]}}}]}"#,
        )]);
        assert!(is_tool_error(&call(
            "movePage",
            &format!(r#"{{"pageId": "{PAGE_ID}", "parent": "Home"}}"#),
            &mut stub,
        )));
    }

    #[test]
    fn move_page_resolves_the_new_parent() {
        let mut stub = Stub::of(&[
            ok_json(
                r#"{"results": [{"object": "page", "id": "parent-1", "properties": {"Name": {"type": "title", "title": [{"plain_text": "Home"}]}}}]}"#,
            ),
            ok_json(&page_json()),
        ]);
        let _ = answered(call(
            "movePage",
            &format!(r#"{{"pageId": "{PAGE_ID}", "parent": "Home"}}"#),
            &mut stub,
        ));
        assert_eq!(stub.seen.len(), 2);
        assert_eq!(
            body_json(&stub.seen[1])["parent"],
            json!({"type": "page_id", "page_id": "parent-1"})
        );
    }

    #[test]
    fn query_data_source_filters_sorts_and_pages() {
        let mut stub = Stub::of(&[
            ok_json(&search_data_source_json()),
            ok_json(&schema_json()),
            ok_json(&format!(
                r#"{{"results": [{0}], "has_more": true, "next_cursor": "cursor-1"}}"#,
                page_json()
            )),
        ]);
        let result = answered(call(
            "queryDataSource",
            r#"{"dataSource": "Tasks", "filter": {"Status": "Done"}, "sorts": [{"property": "Due", "direction": "descending"}], "pageSize": 10, "startCursor": "c0"}"#,
            &mut stub,
        ));
        assert_eq!(stub.seen.len(), 3);
        let queried = body_json(&stub.seen[2]);
        assert!(
            stub.seen[2]
                .url
                .ends_with(&format!("/v1/data_sources/{DS_ID}/query"))
        );
        assert_eq!(
            queried["filter"],
            json!({"property": "Status", "status": {"equals": "Done"}})
        );
        assert_eq!(
            queried["sorts"],
            json!([{"property": "Due", "direction": "descending"}])
        );
        assert_eq!(queried["page_size"], json!(10));
        assert_eq!(queried["start_cursor"], json!("c0"));
        assert_eq!(result["results"][0]["title"], json!("Write the post"));
        assert_eq!(result["hasMore"], json!(true));
        assert_eq!(result["nextCursor"], json!("cursor-1"));

        // A failing query fails the tool.
        let mut stub = Stub::of(&[
            ok_json(&search_data_source_json()),
            ok_json(&schema_json()),
            response(400, "bad"),
        ]);
        assert!(is_tool_error(&call(
            "queryDataSource",
            r#"{"dataSource": "Tasks"}"#,
            &mut stub,
        )));

        // Without filters the body stays minimal.
        let mut stub = Stub::of(&[
            ok_json(&search_data_source_json()),
            ok_json(&schema_json()),
            ok_json(r#"{"results": []}"#),
        ]);
        let result = answered(call(
            "queryDataSource",
            r#"{"dataSource": "Tasks"}"#,
            &mut stub,
        ));
        assert!(body_json(&stub.seen[2]).get("filter").is_none());
        assert_eq!(result["results"], json!([]));
        assert_eq!(result["hasMore"], json!(false));
        assert_eq!(result["nextCursor"], json!(null));
    }

    #[test]
    fn get_data_source_reads_the_schema() {
        let mut stub = Stub::of(&[ok_json(&search_data_source_json()), ok_json(&schema_json())]);
        let result = answered(call(
            "getDataSource",
            r#"{"dataSource": "Tasks"}"#,
            &mut stub,
        ));
        assert_eq!(result["id"], json!(DS_ID));
        assert_eq!(result["title"], json!("Tasks"));
        assert_eq!(result["properties"]["Name"]["type"], json!("title"));
        assert_eq!(
            result["properties"]["Status"]["options"],
            json!(["Todo", "Done"])
        );
        assert!(result["properties"]["Score"].get("options").is_none());
    }

    #[test]
    fn create_data_source_builds_the_schema() {
        let mut stub = Stub::of(&[
            ok_json(
                r#"{"results": [{"object": "page", "id": "parent-1", "properties": {"Name": {"type": "title", "title": [{"plain_text": "Home"}]}}}]}"#,
            ),
            ok_json(&schema_json()),
        ]);
        let _ = answered(call(
            "createDataSource",
            r#"{"parent": "Home", "title": "Tasks", "properties": [
                {"name": "Name", "type": "title"},
                {"name": "Notes", "type": "rich_text"},
                {"name": "Score", "type": "number"},
                {"name": "Done", "type": "checkbox"},
                {"name": "Due", "type": "date"},
                {"name": "Link", "type": "url"},
                {"name": "Mail", "type": "email"},
                {"name": "Phone", "type": "phone_number"},
                {"name": "Owner", "type": "people"},
                {"name": "Status", "type": "status", "options": ["Todo", "Done"]},
                {"name": "Tags", "type": "multi_select", "options": ["a"]},
                {"name": "Pick", "type": "select", "options": ["x"]}
            ]}"#,
            &mut stub,
        ));
        assert_eq!(stub.seen.len(), 2);
        let created = body_json(&stub.seen[1]);
        assert_eq!(created["parent"]["page_id"], json!("parent-1"));
        assert_eq!(created["title"][0]["text"]["content"], json!("Tasks"));
        assert_eq!(created["properties"]["Name"], json!({"title": {}}));
        assert_eq!(
            created["properties"]["Score"],
            json!({"number": {"format": "number"}})
        );
        assert_eq!(
            created["properties"]["Status"]["status"]["options"],
            json!([{"name": "Todo"}, {"name": "Done"}])
        );

        let mut stub = Stub::of(&[]);
        assert!(is_tool_error(&call(
            "createDataSource",
            r#"{"parent": "Home", "title": "T", "properties": []}"#,
            &mut stub,
        )));
        let mut stub = Stub::of(&[]);
        assert!(is_tool_error(&call(
            "createDataSource",
            &format!(
                r#"{{"parent": "{PAGE_ID}", "title": "T", "properties": [{{"name": "S", "type": "status"}}]}}"#
            ),
            &mut stub,
        )));
        let mut stub = Stub::of(&[]);
        assert!(is_tool_error(&call(
            "createDataSource",
            &format!(
                r#"{{"parent": "{PAGE_ID}", "title": "T", "properties": [{{"name": "R", "type": "relation"}}]}}"#
            ),
            &mut stub,
        )));
    }

    #[test]
    fn append_content_appends_blocks() {
        let mut stub = Stub::of(&[ok_json(r#"{"results": []}"#)]);
        let result = answered(call(
            "appendContent",
            &format!(r##"{{"pageId": "{PAGE_ID}", "markdown": "# Hi\n\nSome text"}}"##),
            &mut stub,
        ));
        assert_eq!(stub.seen.len(), 1);
        assert!(
            stub.seen[0]
                .url
                .ends_with(&format!("/v1/blocks/{PAGE_ID}/children"))
        );
        assert_eq!(stub.seen[0].method, Method::Patch);
        assert_eq!(
            body_json(&stub.seen[0])["children"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(result["appendedBlocks"], json!(2));

        // Empty markdown fails before any call.
        let mut stub = Stub::of(&[]);
        assert!(is_tool_error(&call(
            "appendContent",
            &format!(r#"{{"pageId": "{PAGE_ID}", "markdown": "  \n"}}"#),
            &mut stub,
        )));
        assert!(stub.seen.is_empty());

        // More than 100 blocks go out in chunks.
        let markdown = (0..101)
            .map(|n| format!("para {n}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut stub = Stub::of(&[ok_json(r#"{"results": []}"#), ok_json(r#"{"results": []}"#)]);
        let result = answered(call(
            "appendContent",
            &format!(
                r#"{{"pageId": "{PAGE_ID}", "markdown": {}}}"#,
                serde_json::to_string(&markdown).unwrap()
            ),
            &mut stub,
        ));
        assert_eq!(stub.seen.len(), 2);
        assert_eq!(
            body_json(&stub.seen[0])["children"]
                .as_array()
                .unwrap()
                .len(),
            100
        );
        assert_eq!(
            body_json(&stub.seen[1])["children"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(result["appendedBlocks"], json!(101));
    }

    #[test]
    fn list_comments_reads_the_thread() {
        let mut stub = Stub::of(&[ok_json(
            r#"{"results": [{
                "id": "comment-1", "discussion_id": "d1",
                "rich_text": [{"plain_text": "Nice"}],
                "created_time": "2026-10-11T00:00:00.000Z",
                "created_by": {"id": "u1", "name": "Ann"}
            }], "has_more": false, "next_cursor": null}"#,
        )]);
        let result = answered(call(
            "listComments",
            &format!(r#"{{"pageId": "{PAGE_ID}", "pageSize": 5}}"#),
            &mut stub,
        ));
        assert!(stub.seen[0].url.contains(&format!("block_id={PAGE_ID}")));
        assert!(stub.seen[0].url.contains("page_size=5"));
        assert_eq!(result["results"][0]["text"], json!("Nice"));
        assert_eq!(result["results"][0]["discussionId"], json!("d1"));
        assert_eq!(result["results"][0]["author"]["name"], json!("Ann"));
    }

    #[test]
    fn create_comment_posts_and_replies() {
        let mut stub = Stub::of(&[ok_json(
            r#"{"id": "comment-1", "rich_text": [{"plain_text": "Hi"}], "created_by": {"id": "u1"}}"#,
        )]);
        let result = answered(call(
            "createComment",
            &format!(r#"{{"pageId": "{PAGE_ID}", "text": "Hi"}}"#),
            &mut stub,
        ));
        let posted = body_json(&stub.seen[0]);
        assert_eq!(posted["parent"]["page_id"], json!(PAGE_ID));
        assert_eq!(posted["rich_text"][0]["text"]["content"], json!("Hi"));
        assert!(posted.get("discussion_id").is_none());
        assert_eq!(result["text"], json!("Hi"));

        let mut stub = Stub::of(&[ok_json(
            r#"{"id": "comment-2", "discussion_id": "d1", "rich_text": []}"#,
        )]);
        let _ = answered(call(
            "createComment",
            &format!(r#"{{"pageId": "{PAGE_ID}", "text": "Reply", "discussionId": "d1"}}"#),
            &mut stub,
        ));
        assert_eq!(body_json(&stub.seen[0])["discussion_id"], json!("d1"));
    }

    #[test]
    fn get_current_user_reads_persons_and_bots() {
        let mut stub = Stub::of(&[ok_json(
            r#"{"object": "user", "id": "u1", "name": "Ann", "type": "person", "person": {"email": "a@test"}}"#,
        )]);
        let result = answered(call("getCurrentUser", "{}", &mut stub));
        assert_eq!(result["email"], json!("a@test"));
        assert!(result.get("workspace").is_none());

        let mut stub = Stub::of(&[ok_json(
            r#"{"object": "user", "id": "b1", "name": "Bot", "type": "bot", "bot": {"workspace_name": "W"}}"#,
        )]);
        let result = answered(call("getCurrentUser", "{}", &mut stub));
        assert_eq!(result["workspace"], json!("W"));
        assert!(result.get("email").is_none());
    }
}
