//! Google Docs connector: read, create and edit documents through the
//! Google Docs API v1. The host runs the OAuth 2.0 sign-in, refreshes the
//! token and adds `Authorization: Bearer …` after each request leaves the
//! guest: this code never sees it.
//!
//! The tools are shaped around what an agent wants to accomplish — read a
//! document's text, create a document with content, apply typed edits —
//! rather than the API's endpoints. A tool may make several HTTP calls to
//! finish its job (`createDocument` writes the initial text after creating;
//! `editDocument` resolves the append position before its one batch call);
//! tests inject a stub sender so the whole orchestration is covered without
//! a network.

use connectors_sdk::http::{Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::Deserialize;
use serde_json::{Value, json};

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://docs.googleapis.com";

/// List nesting deeper than this is treated as this deep when extracting
/// text: a sanity cap on a value the API keeps small, not a real shape.
const MAX_LIST_NESTING: u64 = 8;

/// The connector.
#[derive(Debug)]
pub struct Gdocs;

// ---------------------------------------------------------------------------
// Tool inputs. `Edit` is parsed into an enum at the boundary: an unknown
// `op`, or a field missing that one op requires, is invalid input.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadDocument {
    document_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateDocument {
    title: String,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditDocument {
    document_id: String,
    edits: Vec<Edit>,
}

/// One typed edit. Indices are UTF-16 code-unit offsets, as the Docs API
/// counts them.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
enum Edit {
    /// Insert `text` at the end of the document.
    Append { text: String },
    /// Insert `text` at `index`.
    Insert { text: String, index: u32 },
    /// Replace every occurrence of `find` with `replace`.
    Replace {
        find: String,
        replace: String,
        #[serde(rename = "matchCase")]
        match_case: Option<bool>,
    },
    /// Delete the half-open range `[startIndex, endIndex)`.
    DeleteRange {
        #[serde(rename = "startIndex")]
        start_index: u32,
        #[serde(rename = "endIndex")]
        end_index: u32,
    },
}

impl Connector for Gdocs {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        Self::call_with(request, |request| Ok(request.clone().send()?))
    }
}

impl Gdocs {
    /// Dispatches a tool call, sending requests through `send`: the real
    /// `Request::send` in production, a stub in tests so multi-call tools are
    /// covered end to end without a network.
    fn call_with(
        request: ToolRequest,
        mut send: impl FnMut(&Request) -> Result<Response, ConnectorError>,
    ) -> Result<ToolResponse, ConnectorError> {
        let base = host::config("api_base").unwrap_or_else(|| DEFAULT_API_BASE.into());
        let api = Api::new(&base);
        match request.tool.as_str() {
            "readDocument" => read_document(&api, &request, &mut send),
            "createDocument" => create_document(&api, &request, &mut send),
            "editDocument" => edit_document(&api, &request, &mut send),
            other => Err(ConnectorError::NotFound(format!("unknown tool: {other}"))),
        }
    }
}

connectors_sdk::export_connector!(Gdocs);

/// The Docs API endpoints derived from the configured base URL.
struct Api {
    documents: String,
}

impl Api {
    fn new(base: &str) -> Self {
        Self {
            documents: format!("{}/v1/documents", base.trim_end_matches('/')),
        }
    }

    /// The URL of one document's resource.
    fn document(&self, document_id: &str) -> String {
        format!("{}/{}", self.documents, encode(document_id))
    }
}

// ---------------------------------------------------------------------------
// readDocument
// ---------------------------------------------------------------------------

/// Runs `readDocument`: one `documents.get` asking only for the title and
/// the structural content, then the plain text walked out of it.
fn read_document(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: ReadDocument = request.json()?;
    let planned = plan_read_document(api, &input)?; // coverage: exclude — plan_read_document wraps an infallible GET in Ok
    let response = match send_step(send, &planned, "reading the document") {
        Ok(response) => response,
        Err(error) => return Ok(error),
    };
    parse_read_document(&input, &response)
}

/// Plans `readDocument`'s `documents.get`: only the title and the structural
/// content travel back, so the agent is not handed the document's styles and
/// formatting.
fn plan_read_document(api: &Api, input: &ReadDocument) -> Result<Request, ConnectorError> {
    Ok(json_get(with_query(
        &api.document(&input.document_id),
        &[(
            "fields",
            Some(String::from("title,body,tabs(documentTab(body))")),
        )],
    )))
}

/// The tool result of `readDocument`'s `documents.get`: the document's ID,
/// title, and plain text.
fn parse_read_document(
    input: &ReadDocument,
    response: &Response,
) -> Result<ToolResponse, ConnectorError> {
    let body: Value = response.json()?;
    let mut text = String::new();
    if let Some(content) = body
        .get("body")
        .and_then(|body| body.get("content"))
        .and_then(Value::as_array)
    {
        collect_text(content, &mut text);
    }
    for tab in body
        .get("tabs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(content) = tab
            .get("documentTab")
            .and_then(|tab| tab.get("body"))
            .and_then(|body| body.get("content"))
            .and_then(Value::as_array)
        {
            collect_text(content, &mut text);
        }
    }
    ToolResponse::structured(&json!({
        "documentId": &input.document_id,
        "title": body.get("title").and_then(Value::as_str).unwrap_or_default(),
        "text": text,
    }))
}

/// Appends the plain text of structural elements — paragraphs (bulleted or
/// not), tables, and tables of contents — to `out`.
fn collect_text(elements: &[Value], out: &mut String) {
    for element in elements {
        if let Some(paragraph) = element.get("paragraph") {
            collect_paragraph(paragraph, out);
        } else if let Some(table) = element.get("table") {
            collect_table(table, out);
        } else if let Some(toc) = element.get("tableOfContents")
            && let Some(content) = toc.get("content").and_then(Value::as_array)
        {
            collect_text(content, out);
        }
    }
}

/// Appends one paragraph's text runs to `out`, ending the line. List items
/// keep their nesting as indentation under a `- ` marker.
fn collect_paragraph(paragraph: &Value, out: &mut String) {
    if let Some(bullet) = paragraph.get("bullet") {
        let nesting = bullet
            .get("nestingLevel")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(MAX_LIST_NESTING);
        for _ in 0..nesting {
            out.push_str("  ");
        }
        out.push_str("- ");
    }
    if let Some(elements) = paragraph.get("elements").and_then(Value::as_array) {
        for element in elements {
            if let Some(content) = element
                .get("textRun")
                .and_then(|run| run.get("content"))
                .and_then(Value::as_str)
            {
                out.push_str(content);
            }
        }
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Appends one table to `out`: tab-separated cells, one row per line, each
/// cell flattened onto a single line.
fn collect_table(table: &Value, out: &mut String) {
    let Some(rows) = table.get("tableRows").and_then(Value::as_array) else {
        return;
    };
    for row in rows {
        let Some(cells) = row.get("tableCells").and_then(Value::as_array) else {
            continue;
        };
        for (index, cell) in cells.iter().enumerate() {
            if index > 0 {
                out.push('\t');
            }
            let mut cell_text = String::new();
            if let Some(content) = cell.get("content").and_then(Value::as_array) {
                collect_text(content, &mut cell_text);
            }
            out.push_str(&flatten(&cell_text));
        }
        out.push('\n');
    }
}

/// Flattens multi-line text onto one line, for table cells.
fn flatten(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// createDocument
// ---------------------------------------------------------------------------

/// Runs `createDocument`: creates the document, then writes the initial text
/// at the start of the body when `text` was given.
fn create_document(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: CreateDocument = request.json()?;
    if input.title.is_empty() {
        return Err(ConnectorError::InvalidInput(
            "title must not be empty".into(),
        ));
    }
    let planned = plan_create_document(api, &input)?; // coverage: exclude — plan_create_document builds from serde_json::Value, which serializes infallibly
    let created = match send_step(send, &planned, "creating the document") {
        Ok(created) => created,
        Err(error) => return Ok(error),
    };
    let document_id = parse_document_id(&created)?;
    if let Some(text) = input.text.as_deref().filter(|text| !text.is_empty()) {
        let planned = plan_write_initial_text(api, &document_id, text)?; // coverage: exclude — plan_write_initial_text builds from serde_json::Value, which serializes infallibly
        match send_step(send, &planned, "writing the initial text") {
            Ok(_) => {}
            Err(error) => return Ok(error),
        }
    }
    combine_create_document(&input, &document_id)
}

/// Plans `createDocument`'s `documents.create`.
fn plan_create_document(api: &Api, input: &CreateDocument) -> Result<Request, ConnectorError> {
    json_post(api.documents.clone(), &json!({ "title": &input.title }))
}

/// Plans the `batchUpdate` that writes a new document's initial text: index
/// 1 is the start of a fresh document's body.
fn plan_write_initial_text(
    api: &Api,
    document_id: &str,
    text: &str,
) -> Result<Request, ConnectorError> {
    json_post(
        format!("{}:batchUpdate", api.document(document_id)),
        &json!({ "requests": [insert_text(1, text)] }),
    )
}

/// The new document's ID from a `documents.create` answer.
fn parse_document_id(response: &Response) -> Result<String, ConnectorError> {
    let body: Value = response.json()?;
    body.get("documentId")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| ConnectorError::Upstream("the create answer had no documentId".into()))
}

/// The tool result of `createDocument`: the new document's ID and title.
fn combine_create_document(
    input: &CreateDocument,
    document_id: &str,
) -> Result<ToolResponse, ConnectorError> {
    ToolResponse::structured(&json!({
        "documentId": document_id,
        "title": &input.title,
    }))
}

// ---------------------------------------------------------------------------
// editDocument
// ---------------------------------------------------------------------------

/// Runs `editDocument`: resolves where appends insert when any edit appends,
/// then applies every typed edit in one `batchUpdate` call.
fn edit_document(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: EditDocument = request.json()?;
    let mut end = match resolve_end_index(api, &input, send) {
        Ok(end) => end,
        Err(error) => return Ok(error),
    };
    if input.edits.is_empty() {
        return combine_edit_document(&input, 0);
    }
    let planned = plan_edit_document(api, &input, &mut end)?; // coverage: exclude — plan_edit_document builds from serde_json::Value, which serializes infallibly
    match send_step(send, &planned, "applying the edits") {
        Ok(_) => {}
        Err(error) => return Ok(error),
    }
    combine_edit_document(&input, input.edits.len())
}

/// The index an `append` inserts at: one `documents.get` asking only for the
/// body's structural ends, then just before the last element's end. Skipped,
/// answering `None`, when no edit appends.
fn resolve_end_index(
    api: &Api,
    input: &EditDocument,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Option<u32>, ToolResponse> {
    if !input
        .edits
        .iter()
        .any(|edit| matches!(edit, Edit::Append { .. }))
    {
        return Ok(None);
    }
    let planned = json_get(with_query(
        &api.document(&input.document_id),
        &[("fields", Some(String::from("body(content(endIndex))")))],
    ));
    let response = send(&planned)
        .map_err(|error| ToolResponse::error(format!("finding where to append failed: {error}")))?;
    step_ok(&response, "finding where to append")?;
    let body: Value = response
        .json()
        .map_err(|error| ToolResponse::error(format!("finding where to append failed: {error}")))?;
    Ok(Some(document_end_index(&body)))
}

/// The index just before the end of the document body: where an append
/// inserts. An empty or unreadable body appends at the body's start.
fn document_end_index(body: &Value) -> u32 {
    body.get("body")
        .and_then(|body| body.get("content"))
        .and_then(Value::as_array)
        .and_then(|content| content.last())
        .and_then(|element| element.get("endIndex"))
        .and_then(Value::as_u64)
        .map(|end| end.saturating_sub(1))
        .and_then(|end| u32::try_from(end).ok())
        .unwrap_or(1)
}

/// Plans `editDocument`'s single `batchUpdate` call from the typed edits.
fn plan_edit_document(
    api: &Api,
    input: &EditDocument,
    end: &mut Option<u32>,
) -> Result<Request, ConnectorError> {
    let mut requests = Vec::with_capacity(input.edits.len());
    for edit in &input.edits {
        requests.push(plan_edit(edit, end));
    }
    json_post(
        format!("{}:batchUpdate", api.document(&input.document_id)),
        &json!({ "requests": requests }),
    )
}

/// Translates one typed edit into its Docs API batchUpdate request object.
/// Appends share the resolved end position, advancing past each appended
/// text so several appends land in order.
fn plan_edit(edit: &Edit, end: &mut Option<u32>) -> Value {
    match edit {
        Edit::Append { text } => {
            // `resolve_end_index` ran because an append is present; without a
            // resolved position, insert at the start of the body.
            let index = match end {
                Some(at) => {
                    let index = *at;
                    *at = at.saturating_add(utf16_len(text));
                    index
                }
                None => 1,
            };
            insert_text(index, text)
        }
        Edit::Insert { text, index } => insert_text(*index, text),
        Edit::Replace {
            find,
            replace,
            match_case,
        } => replace_text(find, replace, *match_case),
        Edit::DeleteRange {
            start_index,
            end_index,
        } => delete_range(*start_index, *end_index),
    }
}

/// An `insertText` request object.
fn insert_text(index: u32, text: &str) -> Value {
    json!({
        "insertText": {
            "location": { "index": index },
            "text": text,
        }
    })
}

/// A `replaceAllText` request object: every occurrence of `find` becomes
/// `replace`. `matchCase` rides along only when the caller set it.
fn replace_text(find: &str, replace: &str, match_case: Option<bool>) -> Value {
    let mut contains = serde_json::Map::with_capacity(2);
    contains.insert("text".to_string(), Value::from(find));
    if let Some(match_case) = match_case {
        contains.insert("matchCase".to_string(), Value::from(match_case));
    }
    json!({
        "replaceAllText": {
            "containsText": Value::Object(contains),
            "replaceText": replace,
        }
    })
}

/// A `deleteContentRange` request object for `[start_index, end_index)`.
fn delete_range(start_index: u32, end_index: u32) -> Value {
    json!({
        "deleteContentRange": {
            "range": { "startIndex": start_index, "endIndex": end_index }
        }
    })
}

/// The length the Docs API counts in: UTF-16 code units, not bytes.
fn utf16_len(text: &str) -> u32 {
    u32::try_from(text.encode_utf16().count()).unwrap_or(u32::MAX)
}

/// The tool result of `editDocument`: how many edits the batch applied.
fn combine_edit_document(
    input: &EditDocument,
    applied: usize,
) -> Result<ToolResponse, ConnectorError> {
    ToolResponse::structured(&json!({
        "documentId": &input.document_id,
        "appliedEdits": applied,
    }))
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Sends one step's request: a transport failure or a non-2xx answer becomes
/// a tool error naming the step, so the agent learns what failed and can react.
fn send_step(
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
    planned: &Request,
    step: &str,
) -> Result<Response, ToolResponse> {
    let response =
        send(planned).map_err(|error| ToolResponse::error(format!("{step} failed: {error}")))?;
    step_ok(&response, step)?;
    Ok(response)
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

/// A GET request that expects a JSON answer.
fn json_get(url: String) -> Request {
    Request::get(url).header("accept", "application/json")
}

/// A POST request carrying a JSON body, expecting a JSON answer.
fn json_post(url: String, body: &Value) -> Result<Request, ConnectorError> {
    Ok(Request::post(url)
        .header("accept", "application/json")
        .json(body)?) // coverage: exclude — Request::json serializes a serde_json::Value here, which is infallible
}

/// Appends the present query parameters to `url`, percent-encoding each value.
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
    use connectors_sdk::http::Method;
    use std::collections::VecDeque;

    /// The default API base, which `call` uses: `host::config` is always
    /// `None` off-host, so tests exercise the default.
    const API: &str = "https://docs.googleapis.com/v1/documents";

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
        Gdocs::call_with(request(tool, body), |request| stub.send(request))
    }

    fn structured(response: &ToolResponse) -> &str {
        response.structured.as_deref().unwrap()
    }

    fn body_of(stub: &Stub, index: usize) -> Value {
        serde_json::from_slice(&stub.seen[index].body).unwrap()
    }

    /// A document exercising every structural shape `readDocument` walks:
    /// paragraphs (plain, bulleted at two nesting levels, one without
    /// elements, one with a non-text element), a section break, a table with
    /// a multi-paragraph cell and an empty cell, a row without cells, a table
    /// without rows, tables of contents with and without content, and tabs.
    const DOCUMENT_BODY: &str = r#"{
        "title": "Brief",
        "body": {"content": [
            {"paragraph": {"elements": [
                {"inlineObjectElement": {}},
                {"textRun": {"content": "Hello "}},
                {"textRun": {"content": "world\n"}}
            ]}},
            {"sectionBreak": {}},
            {"paragraph": {}},
            {"paragraph": {"bullet": {"listId": "l1", "nestingLevel": 1}, "elements": [
                {"textRun": {"content": "nested item\n"}}
            ]}},
            {"paragraph": {"bullet": {"listId": "l1"}, "elements": [
                {"textRun": {"content": "top item\n"}}
            ]}},
            {"table": {}},
            {"table": {"tableRows": [
                {},
                {"tableCells": [
                    {"content": [
                        {"paragraph": {"elements": [{"textRun": {"content": "a1\n"}}]}},
                        {"paragraph": {"elements": [{"textRun": {"content": "a2"}}]}}
                    ]},
                    {"content": [
                        {"paragraph": {"elements": [{"textRun": {"content": "b1\n"}}]}}
                    ]},
                    {}
                ]}
            ]}},
            {"tableOfContents": {}},
            {"tableOfContents": {"content": [
                {"paragraph": {"elements": [{"textRun": {"content": "contents"}}]}}
            ]}}
        ]},
        "tabs": [
            {"documentTab": {}},
            {"documentTab": {"body": {"content": [
                {"paragraph": {"elements": [{"textRun": {"content": "tabbed\n"}}]}}
            ]}}}
        ]
    }"#;

    const DOCUMENT_TEXT: &str =
        "Hello world\n  - nested item\n- top item\na1 a2\tb1\t\ncontents\ntabbed\n";

    #[test]
    fn reads_a_document_as_plain_text() {
        // Arrange: a document exercising every structural shape.
        let mut stub = Stub::of(&[ok_json(DOCUMENT_BODY)]);

        // Act.
        let answered = call("readDocument", r#"{"documentId":"abc"}"#, &mut stub).unwrap();

        // Assert: one GET asking only for the title and the content, and the
        // walked plain text.
        assert_eq!(stub.seen.len(), 1);
        assert_eq!(stub.seen[0].method, Method::Get);
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/abc?fields=title%2Cbody%2Ctabs%28documentTab%28body%29%29")
        );
        let parsed: Value = serde_json::from_str(structured(&answered)).unwrap();
        assert_eq!(parsed["documentId"], json!("abc"));
        assert_eq!(parsed["title"], json!("Brief"));
        assert_eq!(parsed["text"], json!(DOCUMENT_TEXT));
    }

    #[test]
    fn reads_a_document_without_body_or_tabs() {
        // Arrange: the API answers with neither body nor tabs.
        let mut stub = Stub::of(&[ok_json(r#"{"title":"Empty"}"#)]);

        // Act.
        let answered = call("readDocument", r#"{"documentId":"abc"}"#, &mut stub).unwrap();

        // Assert: empty text, and the title it did return.
        assert_eq!(
            structured(&answered),
            r#"{"documentId":"abc","text":"","title":"Empty"}"#
        );

        // Arrange: no title either.
        let mut stub = Stub::of(&[ok_json("{}")]);

        // Act.
        let answered = call("readDocument", r#"{"documentId":"abc"}"#, &mut stub).unwrap();

        // Assert: the missing title defaults to empty.
        assert_eq!(
            structured(&answered),
            r#"{"documentId":"abc","text":"","title":""}"#
        );
    }

    #[test]
    fn reports_read_failures_as_tool_errors() {
        // Arrange: the API refuses the read.
        let mut stub = Stub::of(&[response(404, "not found")]);

        // Act.
        let answered = call("readDocument", r#"{"documentId":"abc"}"#, &mut stub).unwrap();

        // Assert: a tool error naming the step, not a call error.
        assert!(answered.structured.is_none());
        assert_eq!(stub.seen.len(), 1);

        // Arrange: a 2xx answer that is not JSON.
        let mut stub = Stub::of(&[ok_json("<html>")]);

        // Act.
        let garbled = call("readDocument", r#"{"documentId":"abc"}"#, &mut stub);

        // Assert: an upstream contract break is a call error.
        assert!(
            matches!(garbled, Err(ConnectorError::Upstream(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{garbled:?}"
        );

        // Arrange: createDocument's answer is parsed for the new ID; a 2xx
        // answer that is not JSON must surface as Upstream there too.
        let mut stub = Stub::of(&[ok_json("<html>")]);

        // Act.
        let garbled = call("createDocument", r#"{"title":"One"}"#, &mut stub);

        // Assert.
        assert!(
            matches!(garbled, Err(ConnectorError::Upstream(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{garbled:?}"
        );

        // Arrange: no response queued, so the send itself fails.
        let mut stub = Stub::of(&[]);

        // Act.
        let unsent = call("readDocument", r#"{"documentId":"abc"}"#, &mut stub).unwrap();

        // Assert: the transport failure is a tool error naming the step.
        assert!(unsent.structured.is_none());
    }

    #[test]
    fn creates_a_document_with_initial_text() {
        // Arrange: create answers, then the initial-text write answers.
        let mut stub = Stub::of(&[
            ok_json(r#"{"documentId":"new","title":"Brief"}"#),
            ok_json(r#"{"replies":[{}]}"#),
        ]);

        // Act.
        let answered = call(
            "createDocument",
            r#"{"title":"Brief","text":"Hello"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: create, then one batchUpdate inserting at index 1.
        assert_eq!(stub.seen.len(), 2);
        assert_eq!(stub.seen[0].method, Method::Post);
        assert_eq!(stub.seen[0].url, API);
        assert_eq!(body_of(&stub, 0), json!({"title": "Brief"}));
        assert_eq!(stub.seen[1].url, format!("{API}/new:batchUpdate"));
        assert_eq!(
            body_of(&stub, 1),
            json!({"requests": [{"insertText": {"location": {"index": 1}, "text": "Hello"}}]})
        );
        assert_eq!(
            structured(&answered),
            r#"{"documentId":"new","title":"Brief"}"#
        );
    }

    #[test]
    fn creates_a_document_without_text_in_one_call() {
        // Arrange: no text, and empty text, both skip the write.
        let mut stub = Stub::of(&[
            ok_json(r#"{"documentId":"a","title":"One"}"#),
            ok_json(r#"{"documentId":"b","title":"Two"}"#),
        ]);

        // Act.
        let plain = call("createDocument", r#"{"title":"One"}"#, &mut stub).unwrap();
        let empty = call("createDocument", r#"{"title":"Two","text":""}"#, &mut stub).unwrap();

        // Assert: only the create call went out each time.
        assert_eq!(stub.seen.len(), 2);
        assert_eq!(structured(&plain), r#"{"documentId":"a","title":"One"}"#);
        assert_eq!(structured(&empty), r#"{"documentId":"b","title":"Two"}"#);
    }

    #[test]
    fn reports_create_failures_as_tool_errors() {
        // Arrange: creating refuses.
        let mut stub = Stub::of(&[response(403, "insufficient scope")]);

        // Act.
        let refused = call("createDocument", r#"{"title":"Brief"}"#, &mut stub).unwrap();

        // Assert: a tool error from the create step, before any write.
        assert!(refused.structured.is_none());
        assert_eq!(stub.seen.len(), 1);

        // Arrange: creating works but the initial-text write refuses.
        let mut stub = Stub::of(&[
            ok_json(r#"{"documentId":"new","title":"Brief"}"#),
            response(400, "bad request"),
        ]);

        // Act.
        let failed = call(
            "createDocument",
            r#"{"title":"Brief","text":"Hello"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: a tool error from the write step, after the create.
        assert!(failed.structured.is_none());
        assert_eq!(stub.seen.len(), 2);

        // Arrange: the create answer carries no document ID.
        let mut stub = Stub::of(&[ok_json(r#"{"title":"Brief"}"#)]);

        // Act.
        let missing = call("createDocument", r#"{"title":"Brief"}"#, &mut stub);

        // Assert.
        assert!(
            matches!(&missing, Err(ConnectorError::Upstream(message)) if message.contains("documentId")),
            "{missing:?}"
        );
    }

    #[test]
    fn applies_typed_edits_in_one_batch_call() {
        // Arrange: the append position resolves to 10, then the batch answers.
        let mut stub = Stub::of(&[
            ok_json(r#"{"body":{"content":[{"endIndex":11}]}}"#),
            ok_json(r#"{"replies":[{},{},{},{},{},{}]}"#),
        ]);

        // Act.
        let answered = call(
            "editDocument",
            r#"{"documentId":"abc","edits":[
                {"op":"append","text":"a😀b"},
                {"op":"append","text":"!"},
                {"op":"insert","index":5,"text":"x"},
                {"op":"replace","find":"old","replace":"new","matchCase":true},
                {"op":"replace","find":"x","replace":"y"},
                {"op":"deleteRange","startIndex":2,"endIndex":4}
            ]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: one position lookup, then one batchUpdate. The appends
        // share the resolved end, advancing past each text in UTF-16 units
        // ("a😀b" is 4 units), so they land in order.
        assert_eq!(stub.seen.len(), 2);
        assert_eq!(stub.seen[0].method, Method::Get);
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/abc?fields=body%28content%28endIndex%29%29")
        );
        assert_eq!(stub.seen[1].method, Method::Post);
        assert_eq!(stub.seen[1].url, format!("{API}/abc:batchUpdate"));
        assert_eq!(
            body_of(&stub, 1),
            json!({"requests": [
                {"insertText": {"location": {"index": 10}, "text": "a😀b"}},
                {"insertText": {"location": {"index": 14}, "text": "!"}},
                {"insertText": {"location": {"index": 5}, "text": "x"}},
                {"replaceAllText": {
                    "containsText": {"text": "old", "matchCase": true},
                    "replaceText": "new"
                }},
                {"replaceAllText": {
                    "containsText": {"text": "x"},
                    "replaceText": "y"
                }},
                {"deleteContentRange": {"range": {"startIndex": 2, "endIndex": 4}}},
            ]})
        );
        assert_eq!(
            structured(&answered),
            r#"{"appliedEdits":6,"documentId":"abc"}"#
        );
    }

    #[test]
    fn edits_without_appends_skip_the_position_lookup() {
        // Arrange: no append edit, so no GET goes out.
        let mut stub = Stub::of(&[ok_json(r#"{"replies":[{}]}"#)]);

        // Act.
        let answered = call(
            "editDocument",
            r#"{"documentId":"abc","edits":[{"op":"insert","index":1,"text":"Hi"}]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: only the batchUpdate went out.
        assert_eq!(stub.seen.len(), 1);
        assert_eq!(stub.seen[0].url, format!("{API}/abc:batchUpdate"));
        assert_eq!(
            structured(&answered),
            r#"{"appliedEdits":1,"documentId":"abc"}"#
        );
    }

    #[test]
    fn appends_to_an_empty_document_start_at_one() {
        // Arrange: the body has no content to read an end from.
        let mut stub = Stub::of(&[ok_json(r#"{"body":{}}"#), ok_json(r#"{"replies":[{}]}"#)]);

        // Act.
        call(
            "editDocument",
            r#"{"documentId":"abc","edits":[{"op":"append","text":"Hi"}]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the append inserts at the body's start.
        assert_eq!(
            body_of(&stub, 1)["requests"],
            json!([{"insertText": {"location": {"index": 1}, "text": "Hi"}}])
        );

        // Arrange: an end index too large for u32.
        let mut stub = Stub::of(&[
            ok_json(r#"{"body":{"content":[{"endIndex":18446744073709551615}]}}"#),
            ok_json(r#"{"replies":[{}]}"#),
        ]);

        // Act.
        call(
            "editDocument",
            r#"{"documentId":"abc","edits":[{"op":"append","text":"Hi"}]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the unreadable end falls back to the body's start.
        assert_eq!(
            body_of(&stub, 1)["requests"],
            json!([{"insertText": {"location": {"index": 1}, "text": "Hi"}}])
        );
    }

    #[test]
    fn plans_an_append_without_a_resolved_position_at_one() {
        // Arrange: plan_edit called directly, without resolve_end_index.
        let edit: Edit = serde_json::from_str(r#"{"op":"append","text":"Hi"}"#).unwrap();

        // Act.
        let planned = plan_edit(&edit, &mut None);

        // Assert: the defensive default inserts at the body's start.
        assert_eq!(
            planned,
            json!({"insertText": {"location": {"index": 1}, "text": "Hi"}})
        );
    }

    #[test]
    fn accepts_an_empty_edit_list_without_calling() {
        // Arrange: no responses queued; none may be needed.
        let mut stub = Stub::of(&[]);

        // Act.
        let answered = call(
            "editDocument",
            r#"{"documentId":"abc","edits":[]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: nothing went out, zero edits applied.
        assert!(stub.seen.is_empty());
        assert_eq!(
            structured(&answered),
            r#"{"appliedEdits":0,"documentId":"abc"}"#
        );
    }

    #[test]
    fn reports_edit_failures_as_tool_errors() {
        // Arrange: resolving the append position refuses.
        let mut stub = Stub::of(&[response(404, "not found")]);

        // Act.
        let refused = call(
            "editDocument",
            r#"{"documentId":"abc","edits":[{"op":"append","text":"Hi"}]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: a tool error from the position lookup, before the batch.
        assert!(refused.structured.is_none());
        assert_eq!(stub.seen.len(), 1);

        // Arrange: the position lookup answers garbage.
        let mut stub = Stub::of(&[ok_json("<html>")]);

        // Act.
        let garbled = call(
            "editDocument",
            r#"{"documentId":"abc","edits":[{"op":"append","text":"Hi"}]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: a tool error from the position lookup.
        assert!(garbled.structured.is_none());
        assert_eq!(stub.seen.len(), 1);

        // Arrange: no response queued, so the lookup's send fails.
        let mut stub = Stub::of(&[]);

        // Act.
        let unsent = call(
            "editDocument",
            r#"{"documentId":"abc","edits":[{"op":"append","text":"Hi"}]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: a tool error from the position lookup's failed send.
        assert!(unsent.structured.is_none());

        // Arrange: the batch itself refuses.
        let mut stub = Stub::of(&[response(400, "bad request")]);

        // Act.
        let failed = call(
            "editDocument",
            r#"{"documentId":"abc","edits":[{"op":"insert","index":1,"text":"Hi"}]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: a tool error from the batch step.
        assert!(failed.structured.is_none());
        assert_eq!(stub.seen.len(), 1);
    }

    #[test]
    fn refuses_bad_input() {
        // Arrange: no responses needed; input parsing fails first.
        let mut stub = Stub::of(&[]);

        for (tool, body) in [
            ("readDocument", "{}"),
            ("createDocument", "{}"),
            ("createDocument", r#"{"title":""}"#),
            ("editDocument", "{}"),
            ("editDocument", r#"{"documentId":"abc"}"#),
            (
                "editDocument",
                r#"{"documentId":"abc","edits":[{"op":"nuke"}]}"#,
            ),
            (
                "editDocument",
                r#"{"documentId":"abc","edits":[{"op":"insert","text":"Hi"}]}"#,
            ),
            (
                "editDocument",
                r#"{"documentId":"abc","edits":[{"op":"append"}]}"#,
            ),
            (
                "editDocument",
                r#"{"documentId":"abc","edits":[{"text":"Hi"}]}"#,
            ),
        ] {
            // Act.
            let refused = call(tool, body, &mut stub);

            // Assert.
            assert!(
                matches!(refused, Err(ConnectorError::InvalidInput(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
                "{tool} {body}: {refused:?}"
            );
        }

        // Arrange: an unknown tool.
        let unknown = call("nope", "{}", &mut stub);

        // Act + assert.
        assert_eq!(
            unknown.unwrap_err(),
            ConnectorError::NotFound("unknown tool: nope".into())
        );
    }

    #[test]
    fn off_wasm_nothing_is_sent() {
        // Arrange: the real sender, which fails off-host.
        // Act.
        let sent = Gdocs::call_tool(request("readDocument", r#"{"documentId":"a"}"#)).unwrap();

        // Assert: the transport failure surfaces as a tool error.
        assert!(sent.structured.is_none());
    }

    #[test]
    fn builds_query_strings() {
        // Arrange: present, absent and encoded parameters.
        // Act + assert.
        assert_eq!(
            with_query(
                "https://docs.test/x",
                &[("a", Some("1".to_string())), ("b", None)]
            ),
            "https://docs.test/x?a=1"
        );
        assert_eq!(
            with_query("https://docs.test/x", &[("b", None)]),
            "https://docs.test/x"
        );
    }

    #[test]
    fn trims_a_trailing_slash_off_the_base() {
        assert_eq!(
            Api::new("https://docs.test/").documents,
            "https://docs.test/v1/documents"
        );
        assert_eq!(
            Api::new("https://docs.test/").document("a/b c"),
            "https://docs.test/v1/documents/a%2Fb%20c"
        );
    }

    #[test]
    fn manifest_is_embedded() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["name"], "gdocs");
        assert_eq!(manifest["version"], "0.2.0");
        assert_eq!(manifest["auth"]["schemes"][0]["type"], "oauth2");
        assert_eq!(
            manifest["permissions"]["http"]["allow"][0],
            DEFAULT_API_BASE
        );
        let tools: Vec<&str> = manifest["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(tools, ["readDocument", "createDocument", "editDocument"]);
    }
}
