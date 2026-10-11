// Copyright 2026 Contextual, Inc. https://agentrq.com
// This notice may not be modified or removed.
// SPDX-License-Identifier: AGPL-3.0-only

//! Google Drive connector: manage files and folders through the Google Drive
//! API v3. The host runs the OAuth 2.0 sign-in, refreshes the token and adds
//! `Authorization: Bearer …` after each request leaves the guest: this code
//! never sees it.
//!
//! The tools are shaped around what an agent wants to accomplish — searching
//! files, reading a file's content, sharing a file — rather than the API's
//! endpoints. A tool may make several HTTP calls to finish its job (`readFile`
//! fetches the metadata before the content); tests inject a stub sender so the
//! whole orchestration is covered without a network.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use connectors_sdk::http::{Method, Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::Deserialize;
use serde_json::{Value, json};

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://www.googleapis.com";

/// The multipart boundary used for content uploads.
const BOUNDARY: &str = "gdrive-connector-boundary";

/// `readFile` exports these instead of downloading them: Google-native files
/// have no bytes of their own.
const GOOGLE_APPS_PREFIX: &str = "application/vnd.google-apps.";

/// The format Google-native files are exported to unless asked otherwise.
const DEFAULT_EXPORT_MIME_TYPE: &str = "text/plain";

/// The connector.
#[derive(Debug)]
pub struct Gdrive;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchFiles {
    q: Option<String>,
    page_size: Option<u32>,
    page_token: Option<String>,
    order_by: Option<String>,
    fields: Option<String>,
    spaces: Option<String>,
    corpora: Option<String>,
    include_items_from_all_drives: Option<bool>,
    supports_all_drives: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadFile {
    file_id: String,
    export_mime_type: Option<String>,
}

/// The metadata `readFile` decides the content fetch from.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileMetadata {
    id: Option<String>,
    name: Option<String>,
    mime_type: Option<String>,
    size: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetFile {
    file_id: String,
    fields: Option<String>,
    supports_all_drives: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateFile {
    name: String,
    mime_type: Option<String>,
    parents: Option<Vec<String>>,
    description: Option<String>,
    content_base64: Option<String>,
    content_mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateFile {
    file_id: String,
    name: Option<String>,
    description: Option<String>,
    trashed: Option<bool>,
    add_parents: Option<Vec<String>>,
    remove_parents: Option<Vec<String>>,
    content_base64: Option<String>,
    content_mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CopyFile {
    file_id: String,
    name: Option<String>,
    parents: Option<Vec<String>>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteFile {
    file_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShareFile {
    file_id: String,
    #[serde(rename = "type")]
    permission_type: String,
    role: String,
    email_address: Option<String>,
    send_notification_email: Option<bool>,
    email_message: Option<String>,
}

impl Connector for Gdrive {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        Self::call_with(request, |request| request.send())
    }
}

impl Gdrive {
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
            "searchFiles" => one_call(&api, &request, &mut send, plan_search_files),
            "readFile" => read_file(&api, &request, &mut send),
            "getFile" => one_call(&api, &request, &mut send, plan_get_file),
            "createFile" => one_call(&api, &request, &mut send, plan_create_file),
            "updateFile" => one_call(&api, &request, &mut send, plan_update_file),
            "copyFile" => one_call(&api, &request, &mut send, plan_copy_file),
            "deleteFile" => one_call(&api, &request, &mut send, plan_delete_file),
            "shareFile" => one_call(&api, &request, &mut send, plan_share_file),
            other => Err(ConnectorError::NotFound(format!("unknown tool: {other}"))),
        }
    }
}

connectors_sdk::export_connector!(Gdrive);

/// The Drive API endpoints derived from the configured base URL.
struct Api {
    root: String,
    files: String,
}

impl Api {
    fn new(base: &str) -> Self {
        let root = base.trim_end_matches('/').to_string();
        let files = format!("{root}/drive/v3/files");
        Self { root, files }
    }

    /// The URL of one file's resource.
    fn file(&self, file_id: &str) -> String {
        format!("{}/{}", self.files, encode(file_id))
    }
}

/// Plans a single-request tool's call, sends it and answers the response.
fn one_call(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
    plan: fn(&Api, &ToolRequest) -> Result<Request, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let planned = plan(api, request)?;
    answer(&send(&planned)?)
}

/// Runs the two Drive calls behind `readFile`: the file's metadata first, to
/// learn its MIME type, then the content — exported for Google-native files,
/// downloaded directly for everything else.
fn read_file(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: ReadFile = request.json()?;

    let metadata = send(&plan_read_metadata(api, &input)?)?;
    if let Err(error) = step_ok(&metadata, "reading file metadata") {
        return Ok(error);
    }
    let file = parse_read_metadata(&metadata)?;

    let (content_request, exported_as) = plan_read_content(api, &input, &file);
    let step = match exported_as {
        Some(_) => "exporting file content",
        None => "downloading file content",
    };
    let content = send(&content_request)?;
    if let Err(error) = step_ok(&content, step) {
        return Ok(error);
    }

    combine_read_file(&file, &content, exported_as.as_deref())
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

/// Plans `readFile`'s metadata fetch: just enough fields to decide how to get
/// the content.
fn plan_read_metadata(api: &Api, input: &ReadFile) -> Result<Request, ConnectorError> {
    Ok(json_get(with_query(
        &api.file(&input.file_id),
        &[(
            "fields",
            Some("id,name,mimeType,size,exportLinks".to_string()),
        )],
    )))
}

/// The metadata `readFile` decides the content fetch from.
fn parse_read_metadata(response: &Response) -> Result<FileMetadata, ConnectorError> {
    response.json()
}

/// Plans `readFile`'s content fetch: `files.export` for Google-native files,
/// `files.get?alt=media` for everything else. Also answers whether the content
/// was exported, and as what.
fn plan_read_content(
    api: &Api,
    input: &ReadFile,
    file: &FileMetadata,
) -> (Request, Option<String>) {
    let native = file
        .mime_type
        .as_deref()
        .is_some_and(|mime| mime.starts_with(GOOGLE_APPS_PREFIX));
    if native {
        let export_as = input
            .export_mime_type
            .clone()
            .unwrap_or_else(|| DEFAULT_EXPORT_MIME_TYPE.to_string());
        let url = with_query(
            &format!("{}/export", api.file(&input.file_id)),
            &[("mimeType", Some(export_as.clone()))],
        );
        (Request::get(url), Some(export_as))
    } else {
        let url = with_query(&api.file(&input.file_id), &[("alt", Some("media".into()))]);
        (Request::get(url), None)
    }
}

/// Combines `readFile`'s metadata and content into the tool's answer.
fn combine_read_file(
    file: &FileMetadata,
    content: &Response,
    exported_as: Option<&str>,
) -> Result<ToolResponse, ConnectorError> {
    let payload = object(&[
        ("fileId", file.id.as_ref().map(|id| json!(id))),
        ("name", file.name.as_ref().map(|name| json!(name))),
        ("mimeType", file.mime_type.as_ref().map(|mime| json!(mime))),
        ("size", file.size.as_ref().map(|size| json!(size))),
        ("contentBase64", Some(json!(STANDARD.encode(&content.body)))),
        ("exportedAs", exported_as.map(|mime| json!(mime))),
    ]);
    ToolResponse::structured(&payload)
}

/// The tool result of the API's response: its JSON, or a tool error that
/// says what the API answered, so the model can react (a 403 usually means
/// a missing scope). `files.delete` answers 204 with no body.
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

/// A GET request that expects a JSON answer.
fn json_get(url: String) -> Request {
    Request::get(url).header("accept", "application/json")
}

/// Plans `searchFiles`.
fn plan_search_files(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: SearchFiles = request.json()?;
    Ok(json_get(with_query(
        &api.files,
        &[
            ("q", input.q),
            ("pageSize", input.page_size.map(|n| n.to_string())),
            ("pageToken", input.page_token),
            ("orderBy", input.order_by),
            ("fields", input.fields),
            ("spaces", input.spaces),
            ("corpora", input.corpora),
            (
                "includeItemsFromAllDrives",
                input.include_items_from_all_drives.map(|b| b.to_string()),
            ),
            (
                "supportsAllDrives",
                input.supports_all_drives.map(|b| b.to_string()),
            ),
        ],
    )))
}

/// Plans `getFile`.
fn plan_get_file(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: GetFile = request.json()?;
    Ok(json_get(with_query(
        &api.file(&input.file_id),
        &[
            ("fields", input.fields),
            (
                "supportsAllDrives",
                input.supports_all_drives.map(|b| b.to_string()),
            ),
        ],
    )))
}

/// Plans `createFile`: metadata-only JSON, or a multipart upload when
/// `contentBase64` is present.
fn plan_create_file(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: CreateFile = request.json()?;
    let metadata = object(&[
        ("name", Some(json!(input.name))),
        ("mimeType", input.mime_type.as_ref().map(|s| json!(s))),
        ("parents", input.parents.as_ref().map(|p| json!(p))),
        ("description", input.description.as_ref().map(|s| json!(s))),
    ]);
    match upload(
        &metadata,
        input.content_base64.as_ref(),
        input.content_mime_type.as_deref(),
        input.mime_type.as_deref(),
    )? {
        Some((content_type, body)) => Ok(upload_request(
            Method::Post,
            format!("{}/upload/drive/v3/files?uploadType=multipart", api.root),
            content_type,
            body,
        )),
        None => Ok(Request::post(&api.files).json(&metadata)?),
    }
}

/// Plans `updateFile`: metadata, parent moves and/or content replacement.
fn plan_update_file(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: UpdateFile = request.json()?;
    let metadata = object(&[
        ("name", input.name.as_ref().map(|s| json!(s))),
        ("description", input.description.as_ref().map(|s| json!(s))),
        ("trashed", input.trashed.map(|b| json!(b))),
    ]);
    let url = with_query(
        &api.file(&input.file_id),
        &[
            ("addParents", input.add_parents.map(|p| p.join(","))),
            ("removeParents", input.remove_parents.map(|p| p.join(","))),
        ],
    );
    match upload(
        &metadata,
        input.content_base64.as_ref(),
        input.content_mime_type.as_deref(),
        None,
    )? {
        Some((content_type, body)) => Ok(upload_request(
            Method::Patch,
            format!(
                "{}/upload/drive/v3/files/{}?uploadType=multipart",
                api.root,
                encode(&input.file_id)
            ),
            content_type,
            body,
        )),
        None => Ok(Request::new(Method::Patch, url).json(&metadata)?),
    }
}

/// Plans `copyFile`.
fn plan_copy_file(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: CopyFile = request.json()?;
    let body = object(&[
        ("name", input.name.as_ref().map(|s| json!(s))),
        ("parents", input.parents.as_ref().map(|p| json!(p))),
        ("description", input.description.as_ref().map(|s| json!(s))),
    ]);
    Request::post(format!("{}/copy", api.file(&input.file_id))).json(&body)
}

/// Plans `deleteFile`.
fn plan_delete_file(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: DeleteFile = request.json()?;
    Ok(Request::new(Method::Delete, api.file(&input.file_id)))
}

/// Plans `shareFile`.
fn plan_share_file(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: ShareFile = request.json()?;
    let url = with_query(
        &format!("{}/permissions", api.file(&input.file_id)),
        &[(
            "sendNotificationEmail",
            input.send_notification_email.map(|b| b.to_string()),
        )],
    );
    let body = object(&[
        ("type", Some(json!(input.permission_type))),
        ("role", Some(json!(input.role))),
        (
            "emailAddress",
            input.email_address.as_ref().map(|s| json!(s)),
        ),
        (
            "emailMessage",
            input.email_message.as_ref().map(|s| json!(s)),
        ),
    ]);
    Request::post(url).json(&body)
}

/// A request carrying a `multipart/related` body: the content-type header and
/// the raw body are set directly instead of going through `.json()`.
fn upload_request(method: Method, url: String, content_type: String, body: Vec<u8>) -> Request {
    let mut planned = Request::new(method, url);
    planned
        .headers
        .push(("content-type".to_string(), content_type));
    planned.body = body;
    planned
}

/// If `content_base64` is present, the decoded content plus its multipart
/// content-type and body: the file `metadata` as JSON followed by the content.
fn upload(
    metadata: &Value,
    content_base64: Option<&String>,
    content_mime_type: Option<&str>,
    default_mime_type: Option<&str>,
) -> Result<Option<(String, Vec<u8>)>, ConnectorError> {
    let Some(encoded) = content_base64 else {
        return Ok(None);
    };
    let bytes = STANDARD.decode(encoded).map_err(|error| {
        ConnectorError::InvalidInput(format!("contentBase64 is not valid base64: {error}"))
    })?;
    let mime = content_mime_type
        .or(default_mime_type)
        .unwrap_or("application/octet-stream");
    Ok(Some(multipart(metadata, &bytes, mime)))
}

/// A `multipart/related` body: the file metadata as JSON, then the content.
fn multipart(metadata: &Value, content: &[u8], content_type: &str) -> (String, Vec<u8>) {
    let mut body = Vec::new();
    let mut part = |text: &str| body.extend_from_slice(text.as_bytes());
    part(&format!("--{BOUNDARY}\r\n"));
    part("Content-Type: application/json; charset=UTF-8\r\n\r\n");
    part(&metadata.to_string());
    part("\r\n");
    part(&format!("--{BOUNDARY}\r\n"));
    part(&format!("Content-Type: {content_type}\r\n"));
    part("Content-Transfer-Encoding: base64\r\n\r\n");
    part(&STANDARD.encode(content));
    part("\r\n");
    part(&format!("--{BOUNDARY}--\r\n"));
    (format!("multipart/related; boundary=\"{BOUNDARY}\""), body)
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
    use std::collections::VecDeque;

    const API: &str = "https://www.test/drive/v3/files";

    fn api() -> Api {
        Api::new("https://www.test/")
    }

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
        Gdrive::call_with(request(tool, body), |request| stub.send(request))
    }

    fn header(request: &Request, name: &str) -> Option<String> {
        request
            .headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    }

    #[test]
    fn searches_files() {
        // Arrange: a search with every parameter set.
        let mut stub = Stub::of(&[ok_json(r#"{"files":[{"id":"a"}]}"#)]);

        // Act.
        let answered = call(
            "searchFiles",
            r#"{"q":"name contains 'budget' and trashed=false","pageSize":20,"pageToken":"tok","orderBy":"modifiedTime desc","fields":"files(id)","spaces":"drive","corpora":"user","includeItemsFromAllDrives":true,"supportsAllDrives":true}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            answered.structured.as_deref(),
            Some(r#"{"files":[{"id":"a"}]}"#)
        );
        assert_eq!(stub.seen.len(), 1);
        assert_eq!(stub.seen[0].method, Method::Get);
        assert_eq!(
            stub.seen[0].url,
            "https://www.googleapis.com/drive/v3/files?q=name%20contains%20%27budget%27%20and%20trashed%3Dfalse\
             &pageSize=20&pageToken=tok&orderBy=modifiedTime%20desc&fields=files%28id%29\
             &spaces=drive&corpora=user&includeItemsFromAllDrives=true&supportsAllDrives=true",
        );
    }

    #[test]
    fn searches_files_without_parameters() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"files":[]}"#)]);

        // Act.
        call("searchFiles", "{}", &mut stub).unwrap();

        // Assert: no query string at all.
        assert_eq!(
            stub.seen[0].url,
            "https://www.googleapis.com/drive/v3/files"
        );
    }

    #[test]
    fn gets_file() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"id":"abc","name":"Q3"}"#)]);

        // Act.
        let answered = call(
            "getFile",
            r#"{"fileId":"abc","fields":"id,name","supportsAllDrives":true}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            answered.structured.as_deref(),
            Some(r#"{"id":"abc","name":"Q3"}"#)
        );
        assert_eq!(
            stub.seen[0].url,
            "https://www.googleapis.com/drive/v3/files/abc?fields=id%2Cname&supportsAllDrives=true"
        );
    }

    #[test]
    fn creates_file_and_folder() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"id":"new","name":"Q3 Docs"}"#)]);

        // Act.
        let answered = call(
            "createFile",
            r#"{"name":"Q3 Docs","mimeType":"application/vnd.google-apps.folder","parents":["root"],"description":"Quarterly docs"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            answered.structured.as_deref(),
            Some(r#"{"id":"new","name":"Q3 Docs"}"#)
        );
        assert_eq!(stub.seen[0].method, Method::Post);
        assert_eq!(
            stub.seen[0].url,
            "https://www.googleapis.com/drive/v3/files"
        );
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            body,
            json!({
                "name": "Q3 Docs",
                "mimeType": "application/vnd.google-apps.folder",
                "parents": ["root"],
                "description": "Quarterly docs",
            })
        );
    }

    #[test]
    fn creates_file_with_content() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"id":"new"}"#)]);

        // Act.
        call(
            "createFile",
            r#"{"name":"notes.txt","mimeType":"text/plain","contentBase64":"aGVsbG8="}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: a multipart upload carrying the metadata and the content.
        assert_eq!(
            stub.seen[0].url,
            "https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart"
        );
        assert_eq!(
            header(&stub.seen[0], "content-type").as_deref(),
            Some("multipart/related; boundary=\"gdrive-connector-boundary\"")
        );
        let body = String::from_utf8(stub.seen[0].body.clone()).unwrap();
        assert!(body.contains("--gdrive-connector-boundary\r\n"));
        assert!(body.contains("Content-Type: application/json; charset=UTF-8"));
        assert!(body.contains(r#""name":"notes.txt""#));
        assert!(body.contains(r#""mimeType":"text/plain""#));
        assert!(body.contains("Content-Type: text/plain\r\n"));
        assert!(body.contains("\r\n\r\naGVsbG8=\r\n"));
        assert!(body.ends_with("--gdrive-connector-boundary--\r\n"));
    }

    #[test]
    fn creates_file_with_default_content_type() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"id":"new"}"#)]);

        // Act.
        call(
            "createFile",
            r#"{"name":"blob","contentBase64":"AA=="}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        let body = String::from_utf8(stub.seen[0].body.clone()).unwrap();
        assert!(body.contains("Content-Type: application/octet-stream\r\n"));
    }

    #[test]
    fn rejects_bad_base64() {
        // Arrange.
        let mut stub = Stub::of(&[]);

        // Act.
        let bad = call(
            "createFile",
            r#"{"name":"x","contentBase64":"!!!"}"#,
            &mut stub,
        );

        // Assert.
        assert!(
            matches!(bad, Err(ConnectorError::InvalidInput(ref m)) if m.contains("not valid base64")),
            "{bad:?}"
        );

        let bad = call(
            "updateFile",
            r#"{"fileId":"abc","contentBase64":"!!!"}"#,
            &mut stub,
        );
        assert!(
            matches!(bad, Err(ConnectorError::InvalidInput(ref m)) if m.contains("not valid base64")),
            "{bad:?}"
        );
    }

    #[test]
    fn updates_file() {
        // Arrange: rename, move and content replacement are one call each.
        let renamed = plan_update_file(
            &api(),
            &request(
                "updateFile",
                r#"{"fileId":"abc","name":"Q3 Budget","description":"The budget","trashed":true}"#,
            ),
        )
        .unwrap();
        assert_eq!(renamed.method, Method::Patch);
        assert_eq!(renamed.url, format!("{API}/abc"));
        let body: Value = serde_json::from_slice(&renamed.body).unwrap();
        assert_eq!(
            body,
            json!({ "name": "Q3 Budget", "description": "The budget", "trashed": true })
        );

        let moved = plan_update_file(
            &api(),
            &request(
                "updateFile",
                r#"{"fileId":"abc","addParents":["def"],"removeParents":["ghi"]}"#,
            ),
        )
        .unwrap();
        assert_eq!(
            moved.url,
            format!("{API}/abc?addParents=def&removeParents=ghi")
        );

        let replaced = plan_update_file(
            &api(),
            &request(
                "updateFile",
                r#"{"fileId":"a b","contentBase64":"aGVsbG8=","contentMimeType":"text/csv"}"#,
            ),
        )
        .unwrap();
        assert_eq!(replaced.method, Method::Patch);
        assert_eq!(
            replaced.url,
            "https://www.test/upload/drive/v3/files/a%20b?uploadType=multipart"
        );
        assert_eq!(
            header(&replaced, "content-type").as_deref(),
            Some("multipart/related; boundary=\"gdrive-connector-boundary\"")
        );
        let body = String::from_utf8(replaced.body).unwrap();
        assert!(body.contains("Content-Type: text/csv\r\n"));

        // Orchestration through call_with answers the API's response.
        let mut stub = Stub::of(&[ok_json(r#"{"id":"abc","name":"Q3 Budget"}"#)]);
        let answered = call(
            "updateFile",
            r#"{"fileId":"abc","name":"Q3 Budget"}"#,
            &mut stub,
        )
        .unwrap();
        assert_eq!(
            answered.structured.as_deref(),
            Some(r#"{"id":"abc","name":"Q3 Budget"}"#)
        );
        assert_eq!(stub.seen[0].method, Method::Patch);
    }

    #[test]
    fn copies_deletes_and_shares() {
        // Arrange.
        let mut stub = Stub::of(&[
            ok_json(r#"{"id":"copy"}"#),
            response(204, ""),
            ok_json(r#"{"id":"perm"}"#),
        ]);

        // Act: copy.
        let copied = call(
            "copyFile",
            r#"{"fileId":"abc","name":"Copy of Q3 Budget","parents":["root"],"description":"A copy"}"#,
            &mut stub,
        )
        .unwrap();
        assert_eq!(copied.structured.as_deref(), Some(r#"{"id":"copy"}"#));
        assert_eq!(
            stub.seen[0].url,
            "https://www.googleapis.com/drive/v3/files/abc/copy"
        );
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            body,
            json!({ "name": "Copy of Q3 Budget", "parents": ["root"], "description": "A copy" })
        );

        // Act: delete answers {"deleted": true} for the API's 204.
        let deleted = call("deleteFile", r#"{"fileId":"abc"}"#, &mut stub).unwrap();
        assert_eq!(deleted.structured.as_deref(), Some(r#"{"deleted":true}"#));
        assert_eq!(stub.seen[1].method, Method::Delete);
        assert_eq!(
            stub.seen[1].url,
            "https://www.googleapis.com/drive/v3/files/abc"
        );

        // Act: share.
        call(
            "shareFile",
            r#"{"fileId":"abc","type":"user","role":"reader","emailAddress":"alex@example.com","sendNotificationEmail":false,"emailMessage":"FYI"}"#,
            &mut stub,
        )
        .unwrap();
        assert_eq!(
            stub.seen[2].url,
            "https://www.googleapis.com/drive/v3/files/abc/permissions?sendNotificationEmail=false"
        );
        let body: Value = serde_json::from_slice(&stub.seen[2].body).unwrap();
        assert_eq!(
            body,
            json!({
                "type": "user",
                "role": "reader",
                "emailAddress": "alex@example.com",
                "emailMessage": "FYI",
            })
        );
    }

    #[test]
    fn reads_exported_file() {
        // Arrange: a Google Doc, so the content comes from files.export.
        let mut stub = Stub::of(&[
            ok_json(
                r#"{"id":"doc1","name":"Budget","mimeType":"application/vnd.google-apps.document","size":"0"}"#,
            ),
            response(200, "exported text"),
        ]);

        // Act.
        let answered = call(
            "readFile",
            r#"{"fileId":"doc1","exportMimeType":"text/markdown"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: metadata first, then the export.
        assert_eq!(stub.seen.len(), 2);
        assert_eq!(
            stub.seen[0].url,
            "https://www.googleapis.com/drive/v3/files/doc1?fields=id%2Cname%2CmimeType%2Csize%2CexportLinks"
        );
        assert_eq!(
            stub.seen[1].url,
            "https://www.googleapis.com/drive/v3/files/doc1/export?mimeType=text%2Fmarkdown"
        );
        let payload: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(
            payload,
            json!({
                "fileId": "doc1",
                "name": "Budget",
                "mimeType": "application/vnd.google-apps.document",
                "size": "0",
                "contentBase64": STANDARD.encode("exported text"),
                "exportedAs": "text/markdown",
            })
        );
    }

    #[test]
    fn reads_exported_file_with_default_format() {
        // Arrange: a Google Sheet, no exportMimeType given.
        let mut stub = Stub::of(&[
            ok_json(
                r#"{"id":"sheet1","name":"Q3","mimeType":"application/vnd.google-apps.spreadsheet"}"#,
            ),
            response(200, "a,b"),
        ]);

        // Act.
        let answered = call("readFile", r#"{"fileId":"sheet1"}"#, &mut stub).unwrap();

        // Assert: the default export format is text/plain.
        assert_eq!(
            stub.seen[1].url,
            "https://www.googleapis.com/drive/v3/files/sheet1/export?mimeType=text%2Fplain"
        );
        let payload: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(payload["exportedAs"], json!("text/plain"));
        assert_eq!(payload["contentBase64"], json!(STANDARD.encode("a,b")));
    }

    #[test]
    fn reads_downloaded_file() {
        // Arrange: a regular file, so the content comes from files.get?alt=media.
        let mut stub = Stub::of(&[
            ok_json(
                r#"{"id":"pdf1","name":"report.pdf","mimeType":"application/pdf","size":"456"}"#,
            ),
            response(200, "%PDF-bytes"),
        ]);

        // Act.
        let answered = call("readFile", r#"{"fileId":"pdf1"}"#, &mut stub).unwrap();

        // Assert: no exportedAs for a plain download.
        assert_eq!(
            stub.seen[1].url,
            "https://www.googleapis.com/drive/v3/files/pdf1?alt=media"
        );
        let payload: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(
            payload,
            json!({
                "fileId": "pdf1",
                "name": "report.pdf",
                "mimeType": "application/pdf",
                "size": "456",
                "contentBase64": STANDARD.encode("%PDF-bytes"),
            })
        );
        assert!(payload.get("exportedAs").is_none());
    }

    #[test]
    fn read_file_reports_failed_steps() {
        // Arrange: the metadata fetch fails.
        let mut stub = Stub::of(&[response(404, "not found")]);

        // Act.
        let failed = call("readFile", r#"{"fileId":"missing"}"#, &mut stub).unwrap();

        // Assert: a tool error, not a Rust error, naming the failed step.
        assert!(failed.structured.is_none());

        // Arrange: the metadata fetch succeeds but the export fails.
        let mut stub = Stub::of(&[
            ok_json(r#"{"id":"doc1","mimeType":"application/vnd.google-apps.document"}"#),
            response(403, "insufficient scope"),
        ]);

        // Act.
        let failed = call("readFile", r#"{"fileId":"doc1"}"#, &mut stub).unwrap();

        // Assert.
        assert!(failed.structured.is_none());
    }

    #[test]
    fn read_file_rejects_bad_input_and_metadata() {
        // Arrange.
        let mut stub = Stub::of(&[response(200, "<html>")]);

        // Act: a 2xx metadata response that is not JSON.
        let garbled = call("readFile", r#"{"fileId":"doc1"}"#, &mut stub);

        // Assert.
        assert!(
            matches!(garbled, Err(ConnectorError::Upstream(_))),
            "{garbled:?}"
        );

        // Act: a missing fileId.
        let missing = call("readFile", "{}", &mut stub);

        // Assert.
        assert!(
            matches!(missing, Err(ConnectorError::InvalidInput(_))),
            "{missing:?}"
        );
    }

    #[test]
    fn plans_read_metadata_and_content() {
        // Arrange.
        let input: ReadFile = request("readFile", r#"{"fileId":"doc1"}"#).json().unwrap();

        // Act: the metadata request.
        let metadata = plan_read_metadata(&api(), &input).unwrap();

        // Assert.
        assert_eq!(metadata.method, Method::Get);
        assert_eq!(
            metadata.url,
            format!("{API}/doc1?fields=id%2Cname%2CmimeType%2Csize%2CexportLinks")
        );

        // Act/Assert: export for a Google-native file with a custom format.
        let native = FileMetadata {
            id: Some("doc1".into()),
            name: None,
            mime_type: Some("application/vnd.google-apps.document".into()),
            size: None,
        };
        let custom: ReadFile = request(
            "readFile",
            r#"{"fileId":"doc1","exportMimeType":"application/pdf"}"#,
        )
        .json()
        .unwrap();
        let (exported, exported_as) = plan_read_content(&api(), &custom, &native);
        assert_eq!(
            exported.url,
            format!("{API}/doc1/export?mimeType=application%2Fpdf")
        );
        assert_eq!(exported_as.as_deref(), Some("application/pdf"));

        // Act/Assert: download for a regular file.
        let regular = FileMetadata {
            id: Some("pdf1".into()),
            name: None,
            mime_type: Some("application/pdf".into()),
            size: None,
        };
        let (downloaded, exported_as) = plan_read_content(&api(), &input, &regular);
        assert_eq!(downloaded.url, format!("{API}/doc1?alt=media"));
        assert_eq!(exported_as, None);

        // Act/Assert: download when the MIME type is unknown.
        let unknown = FileMetadata {
            id: None,
            name: None,
            mime_type: None,
            size: None,
        };
        let (downloaded, exported_as) = plan_read_content(&api(), &input, &unknown);
        assert!(downloaded.url.ends_with("?alt=media"));
        assert_eq!(exported_as, None);
    }

    #[test]
    fn parses_read_metadata() {
        // Arrange/Act/Assert: valid metadata.
        let file = parse_read_metadata(&ok_json(
            r#"{"id":"doc1","name":"Budget","mimeType":"application/vnd.google-apps.document"}"#,
        ))
        .unwrap();
        assert_eq!(file.id.as_deref(), Some("doc1"));
        assert_eq!(file.name.as_deref(), Some("Budget"));
        assert_eq!(
            file.mime_type.as_deref(),
            Some("application/vnd.google-apps.document")
        );
        assert_eq!(file.size, None);

        // Arrange/Act/Assert: not JSON.
        let garbled = parse_read_metadata(&ok_json("<html>"));
        assert!(
            matches!(garbled, Err(ConnectorError::Upstream(_))),
            "{garbled:?}"
        );
    }

    #[test]
    fn combines_read_file() {
        // Arrange: everything present, exported.
        let file = FileMetadata {
            id: Some("doc1".into()),
            name: Some("Budget".into()),
            mime_type: Some("application/vnd.google-apps.document".into()),
            size: Some("0".into()),
        };
        let content = response(200, "text");

        // Act.
        let combined = combine_read_file(&file, &content, Some("text/plain")).unwrap();

        // Assert.
        let payload: Value = serde_json::from_str(combined.structured.as_deref().unwrap()).unwrap();
        assert_eq!(
            payload,
            json!({
                "fileId": "doc1",
                "name": "Budget",
                "mimeType": "application/vnd.google-apps.document",
                "size": "0",
                "contentBase64": STANDARD.encode("text"),
                "exportedAs": "text/plain",
            })
        );

        // Arrange: nothing present, downloaded.
        let bare = FileMetadata {
            id: None,
            name: None,
            mime_type: None,
            size: None,
        };

        // Act.
        let combined = combine_read_file(&bare, &content, None).unwrap();

        // Assert: only the content survives.
        let payload: Value = serde_json::from_str(combined.structured.as_deref().unwrap()).unwrap();
        assert_eq!(payload, json!({ "contentBase64": STANDARD.encode("text") }));
    }

    #[test]
    fn checks_intermediate_steps() {
        assert!(step_ok(&ok_json("{}"), "reading file metadata").is_ok());
        let failed = step_ok(&response(404, "gone"), "reading file metadata");
        assert!(failed.is_err());
    }

    #[test]
    fn propagates_sender_errors() {
        // Arrange: the sender itself fails.
        let failed = Gdrive::call_with(request("getFile", r#"{"fileId":"abc"}"#), |_| {
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
        let exhausted = call("getFile", r#"{"fileId":"abc"}"#, &mut stub);

        // Assert.
        assert_eq!(
            exhausted.unwrap_err(),
            ConnectorError::Upstream("stub sender ran out of responses".into())
        );
    }

    #[test]
    fn refuses_bad_input() {
        // Arrange.
        let mut stub = Stub::of(&[]);

        // Act: a missing fileId never reaches the sender.
        let missing = call("getFile", "{}", &mut stub);

        // Assert.
        assert!(
            matches!(missing, Err(ConnectorError::InvalidInput(_))),
            "{missing:?}"
        );
        assert!(stub.seen.is_empty());

        // Act: an unknown tool.
        let unknown = call("nope", "{}", &mut stub);

        // Assert.
        assert_eq!(
            unknown.unwrap_err(),
            ConnectorError::NotFound("unknown tool: nope".into())
        );
    }

    #[test]
    fn maps_responses() {
        let ok = answer(&response(200, r#"{"id":"abc","name":"Q3"}"#)).unwrap();
        assert_eq!(
            ok.structured.as_deref(),
            Some(r#"{"id":"abc","name":"Q3"}"#)
        );
        let deleted = answer(&response(204, "")).unwrap();
        assert_eq!(deleted.structured.as_deref(), Some(r#"{"deleted":true}"#));
        let refused = answer(&response(403, "insufficient scope")).unwrap();
        assert_eq!(
            refused,
            ToolResponse::error("The API answered 403: insufficient scope")
        );
        let garbled = answer(&response(200, "<html>"));
        assert!(
            matches!(garbled, Err(ConnectorError::Upstream(_))),
            "{garbled:?}"
        );
    }

    #[test]
    fn answers_tool_errors_through_call_with() {
        // Arrange: the API refuses a single-call tool.
        let mut stub = Stub::of(&[response(403, "insufficient scope")]);

        // Act.
        let refused = call("getFile", r#"{"fileId":"abc"}"#, &mut stub).unwrap();

        // Assert: a tool error, not a Rust error.
        assert!(refused.structured.is_none());
    }

    #[test]
    fn off_wasm_nothing_is_sent() {
        let sent = Gdrive::call_tool(request("searchFiles", "{}"));
        assert!(matches!(&sent, Err(ConnectorError::Upstream(m)) if m.contains("wasm32-wasip2")));
    }

    #[test]
    fn manifest_is_embedded() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["auth"]["schemes"][0]["type"], "oauth2");
        assert_eq!(
            manifest["auth"]["schemes"][0]["scopes"][0],
            "https://www.googleapis.com/auth/drive"
        );
        assert_eq!(
            manifest["permissions"]["http"]["allow"][0],
            DEFAULT_API_BASE
        );
        assert_eq!(manifest["version"], "0.2.0");
        let tools: Vec<&str> = manifest["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            tools,
            [
                "searchFiles",
                "readFile",
                "getFile",
                "createFile",
                "updateFile",
                "copyFile",
                "deleteFile",
                "shareFile",
            ]
        );
        let read = &manifest["tools"][1];
        assert_eq!(
            read["inputSchema"]["required"].as_array().unwrap(),
            &vec![Value::from("fileId")]
        );
    }
}
