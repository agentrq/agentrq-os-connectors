//! Gmail, redesigned around what agents actually do with mail.
//!
//! Instead of mirroring the Gmail API v1 endpoint-for-endpoint, this
//! connector offers twelve task-shaped tools: each one completes a job —
//! searching, reading a thread, sending, replying, forwarding, drafting,
//! organizing, fetching an attachment, managing labels — in a single call,
//! making whatever HTTP requests the job needs internally.
//!
//! The host runs the OAuth 2.0 sign-in, refreshes the token and adds
//! `Authorization: Bearer …` after each request leaves the guest: this
//! code never sees it.
//!
//! Every tool takes an optional `userId` (default `"me"`); the remaining
//! parameters are plain task inputs, not API parameter names.

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use connectors_sdk::http::{Method, Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::str::FromStr;

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://www.googleapis.com";

/// The default and maximum number of messages `searchEmails` returns.
const DEFAULT_SEARCH_RESULTS: u32 = 10;
const MAX_SEARCH_RESULTS: u32 = 50;

/// How many messages `organizeEmail` resolves when given a bulk `query`.
const BULK_LIST_LIMIT: u32 = 500;

/// How many message IDs one `messages/batchModify` call takes, per the API.
const BATCH_MODIFY_LIMIT: usize = 1000;

/// MIME boundaries. Underscores cannot appear in base64, so encoded
/// attachment content can never collide with them.
const MIXED_BOUNDARY: &str = "AGENTRQ_MIXED_BOUNDARY";
const ALT_BOUNDARY: &str = "AGENTRQ_ALT_BOUNDARY";

/// The connector.
#[derive(Debug)]
pub struct Gmail;

impl Connector for Gmail {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        Self::call_with(request, |req| Ok(req.clone().send()?))
    }
}

impl Gmail {
    /// Dispatches a tool call, sending through `send`. Production passes
    /// `|req| Ok(req.clone().send()?)` — the real SDK's `send` takes the
    /// request by value and reports `HttpError`, which `?` converts; tests
    /// pass a stub, so the whole orchestration — including everything after
    /// each send — is covered.
    fn call_with(
        request: ToolRequest,
        send: impl FnMut(&Request) -> Result<Response, ConnectorError>,
    ) -> Result<ToolResponse, ConnectorError> {
        let base = host::config("api_base").unwrap_or_else(|| DEFAULT_API_BASE.into());
        let root = base.trim_end_matches('/');
        let mut send = send;
        let payload = match request.tool.as_str() {
            "searchEmails" => search_emails(root, &request.json()?, &mut send),
            "readThread" => read_thread(root, &request.json()?, &mut send),
            "sendEmail" => send_email(root, &request.json()?, &mut send),
            "replyToEmail" => reply_to_email(root, &request.json()?, &mut send),
            "forwardEmail" => forward_email(root, &request.json()?, &mut send),
            "saveDraft" => save_draft(root, &request.json()?, &mut send),
            "sendDraft" => send_draft(root, &request.json()?, &mut send),
            "discardDraft" => discard_draft(root, &request.json()?, &mut send),
            "organizeEmail" => organize_email(root, &request.json()?, &mut send),
            "getAttachment" => get_attachment(root, &request.json()?, &mut send),
            "manageLabels" => manage_labels(root, &request.json()?, &mut send),
            "getProfile" => get_profile(root, &request.json()?, &mut send),
            other => return Err(ConnectorError::NotFound(format!("unknown tool: {other}"))),
        };
        match payload {
            Ok(json) => ToolResponse::structured(&json),
            Err(ToolError::Input(error)) => Err(error),
            Err(ToolError::Step(failed)) => Ok(failed),
        }
    }
}

connectors_sdk::export_connector!(Gmail);

/// How a tool call fails: bad input is a connector `Err`; a failed upstream
/// step is an `Ok` tool error that names the step.
enum ToolError {
    Input(ConnectorError),
    Step(ToolResponse),
}

impl From<ConnectorError> for ToolError {
    fn from(error: ConnectorError) -> Self {
        Self::Input(error)
    }
}

/// What `organizeEmail` should do, parsed from the `action` string at the
/// tool boundary so the rest of the code matches on a real enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrganizeAction {
    Archive,
    Trash,
    Untrash,
    MarkRead,
    MarkUnread,
    AddLabels,
    RemoveLabels,
}

impl FromStr for OrganizeAction {
    type Err = ConnectorError;

    fn from_str(action: &str) -> Result<Self, ConnectorError> {
        match action.to_ascii_lowercase().as_str() {
            "archive" => Ok(Self::Archive),
            "trash" => Ok(Self::Trash),
            "untrash" => Ok(Self::Untrash),
            "markread" => Ok(Self::MarkRead),
            "markunread" => Ok(Self::MarkUnread),
            "addlabels" => Ok(Self::AddLabels),
            "removelabels" => Ok(Self::RemoveLabels),
            _ => Err(ConnectorError::InvalidInput(format!(
                "unknown organize action: {action}"
            ))),
        }
    }
}

impl OrganizeAction {
    /// The canonical name echoed back in the tool result.
    fn name(self) -> &'static str {
        match self {
            Self::Archive => "archive",
            Self::Trash => "trash",
            Self::Untrash => "untrash",
            Self::MarkRead => "markRead",
            Self::MarkUnread => "markUnread",
            Self::AddLabels => "addLabels",
            Self::RemoveLabels => "removeLabels",
        }
    }

    /// The `(add, remove)` label ids the action maps to. `AddLabels` and
    /// `RemoveLabels` take them from the caller; the rest are the Gmail
    /// system labels for the job.
    fn label_delta(
        self,
        label_ids: Option<Vec<String>>,
    ) -> Result<(Vec<String>, Vec<String>), ConnectorError> {
        let missing =
            |action: &str| ConnectorError::InvalidInput(format!("{action} needs labelIds"));
        match self {
            Self::Archive => Ok((Vec::new(), vec!["INBOX".to_string()])),
            Self::Trash => Ok((vec!["TRASH".to_string()], Vec::new())),
            Self::Untrash => Ok((Vec::new(), vec!["TRASH".to_string()])),
            Self::MarkRead => Ok((Vec::new(), vec!["UNREAD".to_string()])),
            Self::MarkUnread => Ok((vec!["UNREAD".to_string()], Vec::new())),
            Self::AddLabels => Ok((label_ids.ok_or(missing("addLabels"))?, Vec::new())),
            Self::RemoveLabels => Ok((Vec::new(), label_ids.ok_or(missing("removeLabels"))?)),
        }
    }
}

/// What `manageLabels` should do, parsed from the `action` string at the
/// tool boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LabelAction {
    List,
    Create,
    Rename,
    Delete,
}

impl FromStr for LabelAction {
    type Err = ConnectorError;

    fn from_str(action: &str) -> Result<Self, ConnectorError> {
        match action.to_ascii_lowercase().as_str() {
            "list" => Ok(Self::List),
            "create" => Ok(Self::Create),
            "rename" => Ok(Self::Rename),
            "delete" => Ok(Self::Delete),
            _ => Err(ConnectorError::InvalidInput(format!(
                "unknown label action: {action}"
            ))),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchEmails {
    user_id: Option<String>,
    query: String,
    max_results: Option<u32>,
    include_html: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadThread {
    user_id: Option<String>,
    thread_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttachmentInput {
    filename: String,
    mime_type: String,
    content_base64: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendEmail {
    user_id: Option<String>,
    to: Vec<String>,
    cc: Option<Vec<String>>,
    bcc: Option<Vec<String>>,
    subject: String,
    body: String,
    html_body: Option<String>,
    attachments: Option<Vec<AttachmentInput>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReplyToEmail {
    user_id: Option<String>,
    message_id: String,
    body: String,
    cc: Option<Vec<String>>,
    bcc: Option<Vec<String>>,
    attachments: Option<Vec<AttachmentInput>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForwardEmail {
    user_id: Option<String>,
    message_id: String,
    to: Vec<String>,
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveDraft {
    user_id: Option<String>,
    draft_id: Option<String>,
    to: Option<Vec<String>>,
    cc: Option<Vec<String>>,
    bcc: Option<Vec<String>>,
    subject: Option<String>,
    body: Option<String>,
    html_body: Option<String>,
    attachments: Option<Vec<AttachmentInput>>,
    in_reply_to_message_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendDraft {
    user_id: Option<String>,
    draft_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiscardDraft {
    user_id: Option<String>,
    draft_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrganizeEmail {
    user_id: Option<String>,
    action: String,
    message_ids: Option<Vec<String>>,
    thread_ids: Option<Vec<String>>,
    query: Option<String>,
    label_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetAttachment {
    user_id: Option<String>,
    message_id: String,
    attachment_id: Option<String>,
    filename: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManageLabels {
    user_id: Option<String>,
    action: String,
    label_id: Option<String>,
    name: Option<String>,
    message_list_visibility: Option<String>,
    label_list_visibility: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetProfile {
    user_id: Option<String>,
}

/// One header of a Gmail message payload.
#[derive(Debug, Deserialize)]
struct ApiHeader {
    name: String,
    value: String,
}

/// The body of one MIME part: inline data, or an attachment reference.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiBody {
    data: Option<String>,
    attachment_id: Option<String>,
    size: Option<u64>,
}

/// One node of a Gmail message's MIME tree.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiPart {
    #[serde(default)]
    mime_type: String,
    #[serde(default)]
    filename: String,
    #[serde(default)]
    headers: Vec<ApiHeader>,
    #[serde(default)]
    body: ApiBody,
    #[serde(default)]
    parts: Vec<ApiPart>,
}

/// A Gmail message as the API returns it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiMessage {
    id: String,
    thread_id: String,
    #[serde(default)]
    snippet: String,
    #[serde(default)]
    payload: ApiPart,
}

/// The agent-facing view of one message: headers, bodies and attachment
/// metadata, parsed out of the API's MIME tree.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EmailView {
    id: String,
    thread_id: String,
    from: String,
    to: String,
    cc: String,
    subject: String,
    date: String,
    snippet: String,
    body_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    body_html: Option<String>,
    attachments: Vec<AttachmentView>,
}

/// One attachment found in a message: enough to fetch it later.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AttachmentView {
    #[serde(skip_serializing_if = "Option::is_none")]
    attachment_id: Option<String>,
    filename: String,
    mime_type: String,
    size: u64,
}

/// Everything `build_mime` needs to assemble an RFC 5322 message.
struct ComposedMail {
    to: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    subject: String,
    text: String,
    html: Option<String>,
    attachments: Vec<AttachmentInput>,
    in_reply_to: Option<String>,
}

/// What `replyToEmail` and `saveDraft` read off the original message to
/// thread a reply.
struct ReplyContext {
    to: String,
    subject: String,
    in_reply_to: Option<String>,
    thread_id: String,
}

/// The user a request acts as: `userId`, or "me".
fn user(user_id: &Option<String>) -> &str {
    user_id.as_deref().unwrap_or("me")
}

/// `{root}/gmail/v1/users/{userId}/{path}`: the endpoint a tool call hits.
fn user_url(root: &str, user_id: &Option<String>, path: &str) -> String {
    format!("{root}/gmail/v1/users/{}/{path}", user(user_id))
}

/// A GET request asking for JSON back.
fn get(url: String) -> Request {
    Request::get(url).header("accept", "application/json")
}

/// A request with a JSON body and the headers the API wants. Serializing a
/// `serde_json::Value` cannot fail, so the fallback never runs.
fn json_request(method: Method, url: String, body: &Value) -> Request {
    Request {
        method,
        url,
        headers: vec![
            ("accept".to_string(), "application/json".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ],
        body: serde_json::to_vec(body).unwrap_or_default(),
    }
}

fn post_json(url: String, body: &Value) -> Request {
    json_request(Method::Post, url, body)
}

fn put_json(url: String, body: &Value) -> Request {
    json_request(Method::Put, url, body)
}

fn delete(url: String) -> Request {
    Request::new(Method::Delete, url).header("accept", "application/json")
}

/// Runs one step of a multi-call tool: sends `request`, and turns the
/// outcome into the step's JSON or a tool error naming the step. A failed
/// upstream call is a tool result, not a connector bug.
fn step(
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
    request: &Request,
    name: &str,
) -> Result<Value, ToolError> {
    let response = send(request)
        .map_err(|error| ToolError::Step(ToolResponse::error(format!("{name} failed: {error}"))))?;
    checked(&response, name).map_err(ToolError::Step)
}

/// The JSON of a 2xx response, `{"deleted": true}` for an empty one (the API
/// answers 204 with no body), or a tool error naming the failed step.
fn checked(response: &Response, step_name: &str) -> Result<Value, ToolResponse> {
    if !(200..300).contains(&response.status) {
        let body = String::from_utf8_lossy(&response.body);
        return Err(ToolResponse::error(format!(
            "{step_name} failed: the API answered {}: {body}",
            response.status
        )));
    }
    if response.body.is_empty() {
        return Ok(json!({ "deleted": true }));
    }
    response
        .json()
        .map_err(|error| ToolResponse::error(format!("{step_name} failed: {error}")))
}

/// Parses one `messages.get` response; a 2xx body that is not a message is a
/// named tool error, not a connector bug.
fn message_from(body: Value, step_name: &str) -> Result<ApiMessage, ToolError> {
    serde_json::from_value(body).map_err(|error| {
        ToolError::Step(ToolResponse::error(format!("{step_name} failed: {error}")))
    })
}

/// The message ids in a `messages.list` response.
fn message_ids_in(list: &Value) -> Vec<String> {
    list.get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter_map(|message| {
                    message
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The value of the first header named `name`, case-insensitively.
fn header<'a>(headers: &'a [ApiHeader], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str())
}

/// `"Alice <alice@example.com>"` to `"alice@example.com"`; a bare address
/// passes through untouched.
fn extract_address(from: &str) -> &str {
    match (from.find('<'), from.find('>')) {
        (Some(open), Some(close)) if open < close => from[open + 1..close].trim(),
        _ => from.trim(),
    }
}

/// Prefixes a subject with `Re:`, unless it already carries one.
fn reply_subject(subject: &str) -> String {
    if subject.to_ascii_lowercase().starts_with("re:") {
        subject.to_string()
    } else {
        format!("Re: {subject}")
    }
}

/// Decodes a base64url message body to text, falling back to empty when the
/// API hands us something unexpected.
fn decode_text(data: &str) -> String {
    URL_SAFE_NO_PAD
        .decode(data)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

/// The agent-facing view of one Gmail message.
fn parse_email(message: &ApiMessage, include_html: bool) -> EmailView {
    let headers = &message.payload.headers;
    let mut text = String::new();
    let mut html = None;
    let mut attachments = Vec::new();
    walk_part(
        &message.payload,
        include_html,
        &mut text,
        &mut html,
        &mut attachments,
    );
    EmailView {
        id: message.id.clone(),
        thread_id: message.thread_id.clone(),
        from: header(headers, "From").unwrap_or("").to_string(),
        to: header(headers, "To").unwrap_or("").to_string(),
        cc: header(headers, "Cc").unwrap_or("").to_string(),
        subject: header(headers, "Subject").unwrap_or("").to_string(),
        date: header(headers, "Date").unwrap_or("").to_string(),
        snippet: message.snippet.clone(),
        body_text: text,
        body_html: html,
        attachments,
    }
}

/// Walks the MIME tree: the first `text/plain` part is the body, the first
/// `text/html` part is the HTML body when wanted, and parts with a filename
/// are attachments.
fn walk_part(
    part: &ApiPart,
    include_html: bool,
    text: &mut String,
    html: &mut Option<String>,
    attachments: &mut Vec<AttachmentView>,
) {
    if !part.filename.is_empty() {
        attachments.push(AttachmentView {
            attachment_id: part.body.attachment_id.clone(),
            filename: part.filename.clone(),
            mime_type: part.mime_type.clone(),
            size: part.body.size.unwrap_or(0),
        });
        return;
    }
    match part.mime_type.as_str() {
        "text/plain" if text.is_empty() => {
            if let Some(data) = &part.body.data {
                *text = decode_text(data);
            }
        }
        "text/html" if include_html && html.is_none() => {
            if let Some(data) = &part.body.data {
                *html = Some(decode_text(data));
            }
        }
        _ => {}
    }
    for child in &part.parts {
        walk_part(child, include_html, text, html, attachments);
    }
}

/// Builds the RFC 5322 message for `sendEmail`, `replyToEmail`,
/// `forwardEmail` and `saveDraft`, so agents never hand-assemble MIME: plain
/// text, or `multipart/alternative` when there is an HTML body, wrapped in
/// `multipart/mixed` when there are attachments.
fn build_mime(mail: &ComposedMail) -> Result<String, ConnectorError> {
    let mut mime = String::new();
    if !mail.to.is_empty() {
        header_line(&mut mime, "To", &mail.to.join(", "));
    }
    if !mail.cc.is_empty() {
        header_line(&mut mime, "Cc", &mail.cc.join(", "));
    }
    if !mail.bcc.is_empty() {
        header_line(&mut mime, "Bcc", &mail.bcc.join(", "));
    }
    header_line(&mut mime, "Subject", &mail.subject);
    if let Some(in_reply_to) = &mail.in_reply_to {
        header_line(&mut mime, "In-Reply-To", in_reply_to);
        header_line(&mut mime, "References", in_reply_to);
    }
    header_line(&mut mime, "MIME-Version", "1.0");
    if mail.attachments.is_empty() {
        write_body(&mut mime, &mail.text, mail.html.as_deref());
    } else {
        header_line(
            &mut mime,
            "Content-Type",
            &format!("multipart/mixed; boundary=\"{MIXED_BOUNDARY}\""),
        );
        mime.push_str("\r\n");
        part_boundary(&mut mime, MIXED_BOUNDARY);
        write_body(&mut mime, &mail.text, mail.html.as_deref());
        for attachment in &mail.attachments {
            part_boundary(&mut mime, MIXED_BOUNDARY);
            write_attachment(&mut mime, attachment)?;
        }
        mime.push_str(&format!("--{MIXED_BOUNDARY}--\r\n"));
    }
    Ok(mime)
}

/// One `Name: value` header line.
fn header_line(mime: &mut String, name: &str, value: &str) {
    mime.push_str(name);
    mime.push_str(": ");
    mime.push_str(value);
    mime.push_str("\r\n");
}

/// `--boundary\r\n`: opens one part of a multipart message.
fn part_boundary(mime: &mut String, boundary: &str) {
    mime.push_str("--");
    mime.push_str(boundary);
    mime.push_str("\r\n");
}

/// The text (and optional HTML) part: alone, or as the first part of a
/// `multipart/mixed` message.
fn write_body(mime: &mut String, text: &str, html: Option<&str>) {
    match html {
        Some(html) => {
            header_line(
                mime,
                "Content-Type",
                &format!("multipart/alternative; boundary=\"{ALT_BOUNDARY}\""),
            );
            mime.push_str("\r\n");
            part_boundary(mime, ALT_BOUNDARY);
            write_text_part(mime, "text/plain", text);
            part_boundary(mime, ALT_BOUNDARY);
            write_text_part(mime, "text/html", html);
            mime.push_str(&format!("--{ALT_BOUNDARY}--\r\n"));
        }
        None => write_text_part(mime, "text/plain", text),
    }
}

/// One `text/*` part, sent as raw UTF-8.
fn write_text_part(mime: &mut String, mime_type: &str, text: &str) {
    header_line(
        mime,
        "Content-Type",
        &format!("{mime_type}; charset=\"UTF-8\""),
    );
    header_line(mime, "Content-Transfer-Encoding", "8bit");
    mime.push_str("\r\n");
    mime.push_str(text);
    mime.push_str("\r\n");
}

/// One file part. The agent's base64 is normalized to the standard alphabet
/// first, so content round-tripped from `getAttachment` (base64url) works.
fn write_attachment(mime: &mut String, attachment: &AttachmentInput) -> Result<(), ConnectorError> {
    let filename = attachment.filename.replace('"', "'");
    header_line(
        mime,
        "Content-Type",
        &format!("{}; name=\"{filename}\"", attachment.mime_type),
    );
    header_line(mime, "Content-Transfer-Encoding", "base64");
    header_line(
        mime,
        "Content-Disposition",
        &format!("attachment; filename=\"{filename}\""),
    );
    mime.push_str("\r\n");
    mime.push_str(&wrapped_base64(&normalized_base64(
        &attachment.content_base64,
    )?));
    mime.push_str("\r\n");
    Ok(())
}

/// Accepts the standard base64 agents produce as well as the base64url Gmail
/// returns, normalizing to standard base64 for the MIME part.
fn normalized_base64(encoded: &str) -> Result<String, ConnectorError> {
    let bytes = STANDARD
        .decode(encoded)
        .or_else(|_| URL_SAFE_NO_PAD.decode(encoded))
        .map_err(|error| {
            ConnectorError::InvalidInput(format!("contentBase64 is not valid base64: {error}"))
        })?;
    Ok(STANDARD.encode(bytes))
}

/// Wraps base64 at 76 characters per line, as RFC 2045 asks of
/// `Content-Transfer-Encoding: base64` parts. Base64 is pure ASCII, so
/// chunking the bytes is safe.
fn wrapped_base64(encoded: &str) -> String {
    let mut wrapped = String::with_capacity(encoded.len() + encoded.len() / 76 * 2 + 2);
    for chunk in encoded.as_bytes().chunks(76) {
        wrapped.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        wrapped.push_str("\r\n");
    }
    wrapped
}

/// Reads the headers `replyToEmail` and `saveDraft` need to thread a reply:
/// who to answer, a `Re:` subject, and the threading headers.
fn reply_context(
    root: &str,
    user_id: &Option<String>,
    message_id: &str,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ReplyContext, ToolError> {
    let name = format!("reading message {message_id}");
    let original = step(
        send,
        &get(with_query(
            &user_url(root, user_id, &format!("messages/{}", encode(message_id))),
            &[
                ("format", Some("metadata".to_string())),
                (
                    "metadataHeaders",
                    Some("From,To,Cc,Subject,Message-ID".to_string()),
                ),
            ],
        )),
        &name,
    )?;
    let message = message_from(original, &name)?;
    let headers = &message.payload.headers;
    let from = header(headers, "From").ok_or(ToolError::Step(ToolResponse::error(format!(
        "{name} failed: the message has no From header"
    ))))?;
    Ok(ReplyContext {
        to: extract_address(from).to_string(),
        subject: reply_subject(header(headers, "Subject").unwrap_or("")),
        in_reply_to: header(headers, "Message-ID").map(str::to_string),
        thread_id: message.thread_id,
    })
}

/// `{"addLabelIds": …, "removeLabelIds": …}`, omitting the empty side, plus
/// `ids` for the batch endpoint.
fn modify_body(ids: Option<&[String]>, add: &[String], remove: &[String]) -> Value {
    let mut body = object(&[
        ("addLabelIds", (!add.is_empty()).then_some(json!(add))),
        (
            "removeLabelIds",
            (!remove.is_empty()).then_some(json!(remove)),
        ),
    ]);
    if let Some(ids) = ids {
        body["ids"] = json!(ids);
    }
    body
}

fn search_emails(
    root: &str,
    input: &SearchEmails,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    let max = input
        .max_results
        .unwrap_or(DEFAULT_SEARCH_RESULTS)
        .clamp(1, MAX_SEARCH_RESULTS);
    let listed = step(
        send,
        &get(with_query(
            &user_url(root, &input.user_id, "messages"),
            &[
                ("q", Some(input.query.clone())),
                ("maxResults", Some(max.to_string())),
            ],
        )),
        "listing messages",
    )?;
    let ids = message_ids_in(&listed);
    let mut messages = Vec::with_capacity(ids.len());
    let include_html = input.include_html.unwrap_or(false);
    for id in &ids {
        let name = format!("reading message {id}");
        let full = step(
            send,
            &get(with_query(
                &user_url(root, &input.user_id, &format!("messages/{}", encode(id))),
                &[("format", Some("full".to_string()))],
            )),
            &name,
        )?;
        messages.push(parse_email(&message_from(full, &name)?, include_html));
    }
    Ok(json!({ "messages": messages }))
}

fn read_thread(
    root: &str,
    input: &ReadThread,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    let thread = step(
        send,
        &get(with_query(
            &user_url(
                root,
                &input.user_id,
                &format!("threads/{}", encode(&input.thread_id)),
            ),
            &[("format", Some("full".to_string()))],
        )),
        "reading the thread",
    )?;
    let mut messages = Vec::new();
    if let Some(raw) = thread.get("messages").and_then(Value::as_array) {
        messages.reserve(raw.len());
        for (index, item) in raw.iter().enumerate() {
            let name = format!("reading message {index} of the thread");
            messages.push(parse_email(&message_from(item.clone(), &name)?, false));
        }
    }
    let subject = messages
        .first()
        .map(|message: &EmailView| message.subject.as_str())
        .unwrap_or("");
    Ok(json!({
        "id": thread.get("id").and_then(Value::as_str).unwrap_or(&input.thread_id),
        "subject": subject,
        "messages": messages,
    }))
}

fn send_email(
    root: &str,
    input: &SendEmail,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    if input.to.is_empty()
        && input.cc.as_ref().is_none_or(Vec::is_empty)
        && input.bcc.as_ref().is_none_or(Vec::is_empty)
    {
        return Err(ToolError::Input(ConnectorError::InvalidInput(
            "sendEmail needs at least one recipient in to, cc, or bcc".into(),
        )));
    }
    let mail = ComposedMail {
        to: input.to.clone(),
        cc: input.cc.clone().unwrap_or_default(),
        bcc: input.bcc.clone().unwrap_or_default(),
        subject: input.subject.clone(),
        text: input.body.clone(),
        html: input.html_body.clone(),
        attachments: input.attachments.clone().unwrap_or_default(),
        in_reply_to: None,
    };
    let raw = URL_SAFE_NO_PAD.encode(build_mime(&mail)?);
    let sent = step(
        send,
        &post_json(
            user_url(root, &input.user_id, "messages/send"),
            &json!({ "raw": raw }),
        ),
        "sending the message",
    )?;
    Ok(json!({ "id": sent.get("id"), "threadId": sent.get("threadId") }))
}

fn reply_to_email(
    root: &str,
    input: &ReplyToEmail,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    let ReplyContext {
        to,
        subject,
        in_reply_to,
        thread_id,
    } = reply_context(root, &input.user_id, &input.message_id, send)?;
    let mail = ComposedMail {
        to: vec![to],
        cc: input.cc.clone().unwrap_or_default(),
        bcc: input.bcc.clone().unwrap_or_default(),
        subject,
        text: input.body.clone(),
        html: None,
        attachments: input.attachments.clone().unwrap_or_default(),
        in_reply_to,
    };
    let raw = URL_SAFE_NO_PAD.encode(build_mime(&mail)?);
    let sent = step(
        send,
        &post_json(
            user_url(root, &input.user_id, "messages/send"),
            &json!({ "raw": raw, "threadId": thread_id }),
        ),
        "sending the reply",
    )?;
    Ok(json!({ "id": sent.get("id"), "threadId": sent.get("threadId") }))
}

fn forward_email(
    root: &str,
    input: &ForwardEmail,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    let name = format!("reading message {}", input.message_id);
    let full = step(
        send,
        &get(with_query(
            &user_url(
                root,
                &input.user_id,
                &format!("messages/{}", encode(&input.message_id)),
            ),
            &[("format", Some("full".to_string()))],
        )),
        &name,
    )?;
    let original = parse_email(&message_from(full, &name)?, false);
    let subject = if original.subject.to_ascii_lowercase().starts_with("fwd:") {
        original.subject.clone()
    } else {
        format!("Fwd: {}", original.subject)
    };
    let mut text = String::new();
    if let Some(note) = &input.body {
        text.push_str(note);
        text.push_str("\n\n");
    }
    text.push_str("---------- Forwarded message ----------\n");
    for (label, value) in [
        ("From", original.from.as_str()),
        ("Date", original.date.as_str()),
        ("Subject", original.subject.as_str()),
        ("To", original.to.as_str()),
    ] {
        text.push_str(label);
        text.push_str(": ");
        text.push_str(value);
        text.push('\n');
    }
    text.push('\n');
    text.push_str(&original.body_text);
    let mail = ComposedMail {
        to: input.to.clone(),
        cc: Vec::new(),
        bcc: Vec::new(),
        subject,
        text,
        html: None,
        attachments: Vec::new(),
        in_reply_to: None,
    };
    let raw = URL_SAFE_NO_PAD.encode(build_mime(&mail)?); // coverage: exclude — forward_email never attaches files, so build_mime cannot fail here
    let sent = step(
        send,
        &post_json(
            user_url(root, &input.user_id, "messages/send"),
            &json!({ "raw": raw }),
        ),
        "forwarding the message",
    )?;
    Ok(json!({ "id": sent.get("id"), "threadId": sent.get("threadId") }))
}

fn save_draft(
    root: &str,
    input: &SaveDraft,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    let mut in_reply_to = None;
    let mut thread_id = None;
    if let Some(original_id) = &input.in_reply_to_message_id {
        let context = reply_context(root, &input.user_id, original_id, send)?;
        in_reply_to = context.in_reply_to;
        thread_id = Some(context.thread_id);
    }
    let mail = ComposedMail {
        to: input.to.clone().unwrap_or_default(),
        cc: input.cc.clone().unwrap_or_default(),
        bcc: input.bcc.clone().unwrap_or_default(),
        subject: input.subject.clone().unwrap_or_default(),
        text: input.body.clone().unwrap_or_default(),
        html: input.html_body.clone(),
        attachments: input.attachments.clone().unwrap_or_default(),
        in_reply_to,
    };
    let raw = URL_SAFE_NO_PAD.encode(build_mime(&mail)?);
    let mut message = json!({ "raw": raw });
    if let Some(thread) = &thread_id {
        message["threadId"] = json!(thread);
    }
    let body = json!({ "message": message });
    let (request, step_name) = match &input.draft_id {
        Some(id) => (
            put_json(
                user_url(root, &input.user_id, &format!("drafts/{}", encode(id))),
                &body,
            ),
            "updating the draft",
        ),
        None => (
            post_json(user_url(root, &input.user_id, "drafts"), &body),
            "creating the draft",
        ),
    };
    let draft = step(send, &request, step_name)?;
    Ok(json!({
        "draftId": draft.get("id"),
        "messageId": draft.get("message").and_then(|message| message.get("id")),
    }))
}

fn send_draft(
    root: &str,
    input: &SendDraft,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    let sent = step(
        send,
        &post_json(
            user_url(root, &input.user_id, "drafts/send"),
            &json!({ "id": input.draft_id }),
        ),
        "sending the draft",
    )?;
    Ok(json!({ "id": sent.get("id"), "threadId": sent.get("threadId") }))
}

fn discard_draft(
    root: &str,
    input: &DiscardDraft,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    step(
        send,
        &delete(user_url(
            root,
            &input.user_id,
            &format!("drafts/{}", encode(&input.draft_id)),
        )),
        "discarding the draft",
    )
}

fn organize_email(
    root: &str,
    input: &OrganizeEmail,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    let action: OrganizeAction = input.action.parse()?;
    let (add, remove) = action.label_delta(input.label_ids.clone())?;
    if input.message_ids.is_none() && input.thread_ids.is_none() && input.query.is_none() {
        return Err(ToolError::Input(ConnectorError::InvalidInput(
            "organizeEmail needs messageIds, threadIds, or query".into(),
        )));
    }
    let mut ids = input.message_ids.clone().unwrap_or_default();
    if let Some(query) = &input.query {
        // A query can match more than one page of results: follow
        // `nextPageToken` so the whole match set is organized, not just the
        // first page.
        let mut page_token: Option<String> = None;
        loop {
            let listed = step(
                send,
                &get(with_query(
                    &user_url(root, &input.user_id, "messages"),
                    &[
                        ("q", Some(query.clone())),
                        ("maxResults", Some(BULK_LIST_LIMIT.to_string())),
                        ("pageToken", page_token.clone()),
                    ],
                )),
                "listing messages for the query",
            )?;
            for id in message_ids_in(&listed) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            page_token = listed
                .get("nextPageToken")
                .and_then(Value::as_str)
                .map(String::from);
            if page_token.is_none() {
                break;
            }
        }
    }
    let threads = input.thread_ids.clone().unwrap_or_default();
    let mut affected: u64 = 0;
    // `batchModify` takes at most 1000 IDs per request: chunk bigger sets.
    for chunk in ids.chunks(BATCH_MODIFY_LIMIT) {
        step(
            send,
            &post_json(
                user_url(root, &input.user_id, "messages/batchModify"),
                &modify_body(Some(chunk), &add, &remove),
            ),
            "updating the messages",
        )?;
        affected += chunk.len() as u64;
    }
    for thread_id in &threads {
        step(
            send,
            &post_json(
                user_url(
                    root,
                    &input.user_id,
                    &format!("threads/{}/modify", encode(thread_id)),
                ),
                &modify_body(None, &add, &remove),
            ),
            "updating the thread",
        )?;
        affected += 1;
    }
    Ok(json!({ "action": action.name(), "affectedCount": affected }))
}

fn get_attachment(
    root: &str,
    input: &GetAttachment,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    if input.attachment_id.is_none() && input.filename.is_none() {
        return Err(ToolError::Input(ConnectorError::InvalidInput(
            "getAttachment needs attachmentId or filename".into(),
        )));
    }
    let name = format!("reading message {}", input.message_id);
    let full = step(
        send,
        &get(with_query(
            &user_url(
                root,
                &input.user_id,
                &format!("messages/{}", encode(&input.message_id)),
            ),
            &[("format", Some("full".to_string()))],
        )),
        &name,
    )?;
    let email = parse_email(&message_from(full, &name)?, false);
    let wanted = email.attachments.iter().find(|attachment| {
        input
            .attachment_id
            .as_ref()
            .is_some_and(|id| attachment.attachment_id.as_deref() == Some(id.as_str()))
            || input
                .filename
                .as_ref()
                .is_some_and(|filename| attachment.filename == *filename)
    });
    let wanted = match wanted {
        Some(attachment) => attachment,
        None => {
            return Err(ToolError::Step(ToolResponse::error(format!(
                "{name} failed: no attachment matched"
            ))));
        }
    };
    let attachment_id = match &wanted.attachment_id {
        Some(id) => id,
        None => {
            return Err(ToolError::Step(ToolResponse::error(format!(
                "{name} failed: the attachment has no download id"
            ))));
        }
    };
    let data = step(
        send,
        &get(user_url(
            root,
            &input.user_id,
            &format!(
                "messages/{}/attachments/{}",
                encode(&input.message_id),
                encode(attachment_id)
            ),
        )),
        "reading the attachment",
    )?;
    Ok(json!({
        "filename": &wanted.filename,
        "mimeType": &wanted.mime_type,
        "size": data.get("size").and_then(Value::as_u64).unwrap_or(wanted.size),
        "contentBase64": data.get("data"),
    }))
}

fn manage_labels(
    root: &str,
    input: &ManageLabels,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    let action: LabelAction = input.action.parse()?;
    let labels = user_url(root, &input.user_id, "labels");
    match action {
        LabelAction::List => step(send, &get(labels), "listing labels"),
        LabelAction::Create => {
            let name = input
                .name
                .clone()
                .ok_or(ConnectorError::InvalidInput("create needs a name".into()))?;
            let body = object(&[
                ("name", Some(json!(name))),
                (
                    "messageListVisibility",
                    input.message_list_visibility.as_ref().map(|s| json!(s)),
                ),
                (
                    "labelListVisibility",
                    input.label_list_visibility.as_ref().map(|s| json!(s)),
                ),
            ]);
            step(send, &post_json(labels, &body), "creating the label")
        }
        LabelAction::Rename => {
            let id = input.label_id.clone().ok_or(ConnectorError::InvalidInput(
                "rename needs a labelId".into(),
            ))?;
            let name = input
                .name
                .clone()
                .ok_or(ConnectorError::InvalidInput("rename needs a name".into()))?;
            let body = object(&[
                ("name", Some(json!(name))),
                (
                    "messageListVisibility",
                    input.message_list_visibility.as_ref().map(|s| json!(s)),
                ),
                (
                    "labelListVisibility",
                    input.label_list_visibility.as_ref().map(|s| json!(s)),
                ),
            ]);
            step(
                send,
                &put_json(format!("{labels}/{}", encode(&id)), &body),
                "renaming the label",
            )
        }
        LabelAction::Delete => {
            let id = input.label_id.clone().ok_or(ConnectorError::InvalidInput(
                "delete needs a labelId".into(),
            ))?;
            step(
                send,
                &delete(format!("{labels}/{}", encode(&id))),
                "deleting the label",
            )
        }
    }
}

fn get_profile(
    root: &str,
    input: &GetProfile,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    let profile = step(
        send,
        &get(user_url(root, &input.user_id, "profile")),
        "reading the profile",
    )?;
    Ok(json!({
        "emailAddress": profile.get("emailAddress"),
        "messagesTotal": profile.get("messagesTotal"),
        "threadsTotal": profile.get("threadsTotal"),
    }))
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
    Value::Object(
        fields
            .iter()
            .filter_map(|(name, value)| {
                value
                    .as_ref()
                    .map(|present| ((*name).to_string(), present.clone()))
            })
            .collect(),
    )
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

    fn tool_request(tool: &str, body: &str) -> ToolRequest {
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

    fn ok(body: &Value) -> Response {
        response(200, &body.to_string())
    }

    /// A scripted sender: plays back queued responses and records the
    /// requests it was asked to send.
    struct Script {
        responses: VecDeque<Result<Response, ConnectorError>>,
        seen: Vec<(Method, String, Vec<u8>)>,
    }

    impl Script {
        fn new() -> Self {
            Self {
                responses: VecDeque::new(),
                seen: Vec::new(),
            }
        }

        fn will_answer(mut self, response: Response) -> Self {
            self.responses.push_back(Ok(response));
            self
        }

        fn will_fail(mut self, error: ConnectorError) -> Self {
            self.responses.push_back(Err(error));
            self
        }

        fn sender(&mut self) -> impl FnMut(&Request) -> Result<Response, ConnectorError> + '_ {
            move |request: &Request| {
                self.seen
                    .push((request.method, request.url.clone(), request.body.clone()));
                self.responses
                    .pop_front()
                    .unwrap_or(Err(ConnectorError::Upstream(
                        "the script ran out of responses".into(),
                    )))
            }
        }

        fn call(&mut self, tool: &str, body: &str) -> Result<ToolResponse, ConnectorError> {
            Gmail::call_with(tool_request(tool, body), self.sender())
        }

        fn seen_url(&self, index: usize) -> &str {
            &self.seen[index].1
        }

        fn seen_method(&self, index: usize) -> Method {
            self.seen[index].0
        }

        fn seen_json(&self, index: usize) -> Value {
            serde_json::from_slice(&self.seen[index].2).unwrap()
        }
    }

    /// The structured payload of a successful tool call.
    fn structured(result: &Result<ToolResponse, ConnectorError>) -> Value {
        let response = result.as_ref().unwrap();
        serde_json::from_str(response.structured.as_deref().unwrap()).unwrap()
    }

    /// Whether the call answered with a tool error (no structured payload).
    fn is_tool_error(result: &Result<ToolResponse, ConnectorError>) -> bool {
        matches!(result, Ok(response) if response.structured.is_none())
    }

    /// A simple `text/plain` message, built with `json!` so the fixture is
    /// obviously well-formed.
    fn message_fixture(id: &str, thread_id: &str, subject: &str, from: &str, text: &str) -> Value {
        json!({
            "id": id,
            "threadId": thread_id,
            "snippet": format!("{text}..."),
            "payload": {
                "mimeType": "text/plain",
                "filename": "",
                "headers": [
                    {"name": "From", "value": from},
                    {"name": "Subject", "value": subject},
                    {"name": "Date", "value": "Sat, 10 Oct 2026 10:00:00 -0700"},
                ],
                "body": {"size": text.len(), "data": URL_SAFE_NO_PAD.encode(text)},
                "parts": [],
            },
        })
    }

    /// A rich multipart message: alternative text+HTML plus an attachment.
    /// The subject header is lowercase on purpose, to exercise
    /// case-insensitive header lookup.
    fn rich_message_fixture() -> Value {
        json!({
            "id": "m1",
            "threadId": "t1",
            "snippet": "Hello there...",
            "payload": {
                "mimeType": "multipart/mixed",
                "filename": "",
                "headers": [
                    {"name": "From", "value": "Alice <alice@example.com>"},
                    {"name": "To", "value": "bob@example.com"},
                    {"name": "Cc", "value": "carol@example.com"},
                    {"name": "subject", "value": "Hello"},
                    {"name": "Date", "value": "Sat, 10 Oct 2026 10:00:00 -0700"},
                    {"name": "Message-ID", "value": "<abc123@example.com>"},
                ],
                "body": {"size": 0},
                "parts": [
                    {
                        "mimeType": "multipart/alternative",
                        "filename": "",
                        "headers": [],
                        "body": {"size": 0},
                        "parts": [
                            {
                                "mimeType": "text/plain",
                                "filename": "",
                                "headers": [],
                                "body": {"size": 11, "data": URL_SAFE_NO_PAD.encode("Hello there")},
                                "parts": [],
                            },
                            {
                                "mimeType": "text/plain",
                                "filename": "",
                                "headers": [],
                                "body": {"size": 12, "data": URL_SAFE_NO_PAD.encode("Hello again!")},
                                "parts": [],
                            },
                            {
                                "mimeType": "text/html",
                                "filename": "",
                                "headers": [],
                                "body": {"size": 24, "data": URL_SAFE_NO_PAD.encode("<b>Hello there</b>")},
                                "parts": [],
                            },
                        ],
                    },
                    {
                        "mimeType": "application/pdf",
                        "filename": "doc.pdf",
                        "headers": [],
                        "body": {"attachmentId": "att1", "size": 1234},
                        "parts": [],
                    },
                    {
                        "mimeType": "image/png",
                        "filename": "inline.png",
                        "headers": [],
                        "body": {"size": 10},
                        "parts": [],
                    },
                ],
            },
        })
    }

    /// The metadata `replyToEmail` reads off the original message.
    fn metadata_fixture() -> Value {
        json!({
            "id": "m1",
            "threadId": "t9",
            "payload": {
                "mimeType": "multipart/alternative",
                "headers": [
                    {"name": "From", "value": "Alice Example <alice@example.com>"},
                    {"name": "To", "value": "me@example.com"},
                    {"name": "Subject", "value": "Project update"},
                    {"name": "Message-ID", "value": "<msg1@example.com>"},
                ],
            },
        })
    }

    fn plain_mail() -> ComposedMail {
        ComposedMail {
            to: vec!["bob@example.com".to_string()],
            cc: vec!["carol@example.com".to_string()],
            bcc: Vec::new(),
            subject: "Hello".to_string(),
            text: "Hi Bob".to_string(),
            html: None,
            attachments: Vec::new(),
            in_reply_to: None,
        }
    }

    #[test]
    fn user_defaults_to_me() {
        assert_eq!(user(&None), "me");
        assert_eq!(
            user(&Some("alex@example.com".to_string())),
            "alex@example.com"
        );
    }

    #[test]
    fn builds_user_urls() {
        assert_eq!(
            user_url("https://www.test/", &None, "profile"),
            "https://www.test//gmail/v1/users/me/profile"
        );
        assert_eq!(
            user_url("https://www.test", &Some("me".to_string()), "messages"),
            "https://www.test/gmail/v1/users/me/messages"
        );
    }

    #[test]
    fn encodes_query_and_paths() {
        assert_eq!(
            with_query(
                "https://x/messages",
                &[
                    ("q", Some("from:boss".to_string())),
                    ("maxResults", Some("10".to_string())),
                    ("pageToken", None),
                ],
            ),
            "https://x/messages?q=from%3Aboss&maxResults=10"
        );
        assert_eq!(with_query("https://x/messages", &[]), "https://x/messages");
        assert_eq!(encode("a b+c~d"), "a%20b%2Bc~d");
        assert_eq!(encode("AZaz09-._~"), "AZaz09-._~");
    }

    #[test]
    fn builds_objects_from_present_fields() {
        assert_eq!(
            object(&[("a", Some(json!(1))), ("b", None), ("c", Some(json!("x"))),]),
            json!({"a": 1, "c": "x"})
        );
        assert_eq!(object(&[("a", None)]), json!({}));
    }

    #[test]
    fn builds_modify_bodies() {
        assert_eq!(
            modify_body(
                Some(&["m1".to_string(), "m2".to_string()]),
                &["STARRED".to_string()],
                &["UNREAD".to_string()],
            ),
            json!({
                "ids": ["m1", "m2"],
                "addLabelIds": ["STARRED"],
                "removeLabelIds": ["UNREAD"],
            })
        );
        assert_eq!(
            modify_body(None, &[], &["INBOX".to_string()]),
            json!({ "removeLabelIds": ["INBOX"] })
        );
        assert_eq!(
            modify_body(None, &["TRASH".to_string()], &[]),
            json!({ "addLabelIds": ["TRASH"] })
        );
        assert_eq!(modify_body(None, &[], &[]), json!({}));
    }

    #[test]
    fn finds_headers_case_insensitively() {
        let headers = vec![
            ApiHeader {
                name: "Subject".to_string(),
                value: "Hi".to_string(),
            },
            ApiHeader {
                name: "FROM".to_string(),
                value: "a@b.c".to_string(),
            },
        ];
        assert_eq!(header(&headers, "subject"), Some("Hi"));
        assert_eq!(header(&headers, "from"), Some("a@b.c"));
        assert_eq!(header(&headers, "Date"), None);
    }

    #[test]
    fn extracts_addresses() {
        assert_eq!(
            extract_address("Alice <alice@example.com>"),
            "alice@example.com"
        );
        assert_eq!(extract_address("alice@example.com"), "alice@example.com");
        assert_eq!(extract_address("  bob@example.com  "), "bob@example.com");
        assert_eq!(extract_address("a > b < c@d"), "a > b < c@d");
    }

    #[test]
    fn prefixes_reply_subjects() {
        assert_eq!(reply_subject("hello"), "Re: hello");
        assert_eq!(reply_subject("Re: hello"), "Re: hello");
        assert_eq!(reply_subject("RE: hello"), "RE: hello");
        assert_eq!(reply_subject(""), "Re: ");
        assert_eq!(reply_subject("Regarding lunch"), "Re: Regarding lunch");
    }

    #[test]
    fn decodes_bodies_leniently() {
        assert_eq!(decode_text(&URL_SAFE_NO_PAD.encode("Hello")), "Hello");
        assert_eq!(decode_text("!!! not base64 !!!"), "");
        assert_eq!(decode_text(&URL_SAFE_NO_PAD.encode([0xff, 0xfe])), "");
    }

    #[test]
    fn normalizes_attachment_base64() {
        // Standard base64 passes through unchanged.
        assert_eq!(normalized_base64("aGVsbG8=").unwrap(), "aGVsbG8=");
        // Base64url (`--8_` decodes to the same bytes as `++8/`) is
        // converted to the standard alphabet.
        assert_eq!(normalized_base64("--8_").unwrap(), "++8/");
        // Garbage is invalid input, not a panic.
        assert!(normalized_base64("!!!").is_err());
    }

    #[test]
    fn wraps_base64_at_76_columns() {
        let short = "a".repeat(76);
        assert_eq!(wrapped_base64(&short), format!("{short}\r\n"));
        let long = "b".repeat(77);
        let expected = format!("{}\r\nb\r\n", "b".repeat(76));
        assert_eq!(wrapped_base64(&long), expected);
        assert_eq!(wrapped_base64(""), "");
    }

    #[test]
    fn builds_plain_mime() {
        assert_eq!(
            build_mime(&plain_mail()).unwrap(),
            "To: bob@example.com\r\n\
             Cc: carol@example.com\r\n\
             Subject: Hello\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: text/plain; charset=\"UTF-8\"\r\n\
             Content-Transfer-Encoding: 8bit\r\n\
             \r\n\
             Hi Bob\r\n"
        );
    }

    #[test]
    fn builds_minimal_mime_without_optional_headers() {
        let mail = ComposedMail {
            to: Vec::new(),
            cc: Vec::new(),
            bcc: vec!["hidden@example.com".to_string()],
            subject: String::new(),
            text: String::new(),
            html: None,
            attachments: Vec::new(),
            in_reply_to: None,
        };
        let mime = build_mime(&mail).unwrap();
        assert!(!mime.contains("To:"));
        assert!(!mime.contains("Cc:"));
        assert!(mime.contains("Bcc: hidden@example.com\r\n"));
    }

    #[test]
    fn builds_alternative_mime_for_html() {
        let mut mail = plain_mail();
        mail.html = Some("<b>Hi Bob</b>".to_string());
        let mime = build_mime(&mail).unwrap();
        assert!(mime.contains(
            "Content-Type: multipart/alternative; boundary=\"AGENTRQ_ALT_BOUNDARY\"\r\n"
        ));
        assert!(mime.contains(
            "--AGENTRQ_ALT_BOUNDARY\r\n\
             Content-Type: text/plain; charset=\"UTF-8\"\r\n"
        ));
        assert!(mime.contains(
            "--AGENTRQ_ALT_BOUNDARY\r\n\
             Content-Type: text/html; charset=\"UTF-8\"\r\n"
        ));
        assert!(mime.contains("<b>Hi Bob</b>\r\n--AGENTRQ_ALT_BOUNDARY--\r\n"));
    }

    #[test]
    fn builds_mixed_mime_for_attachments() {
        let mut mail = plain_mail();
        mail.html = Some("<b>Hi Bob</b>".to_string());
        mail.attachments = vec![
            AttachmentInput {
                filename: "notes.txt".to_string(),
                mime_type: "text/plain".to_string(),
                content_base64: URL_SAFE_NO_PAD.encode("some notes").to_string(),
            },
            AttachmentInput {
                filename: "we\"ird.pdf".to_string(),
                mime_type: "application/pdf".to_string(),
                content_base64: "aGVsbG8=".to_string(),
            },
        ];
        let mime = build_mime(&mail).unwrap();
        assert!(
            mime.contains("Content-Type: multipart/mixed; boundary=\"AGENTRQ_MIXED_BOUNDARY\"\r\n")
        );
        // The text part comes first, then each attachment.
        let text_pos = mime.find("Hi Bob\r\n").unwrap();
        let first_pos = mime.find("notes.txt").unwrap();
        let second_pos = mime.find("we'ird.pdf").unwrap();
        assert!(text_pos < first_pos && first_pos < second_pos);
        assert!(mime.contains("Content-Type: text/plain; name=\"notes.txt\"\r\n"));
        assert!(mime.contains("Content-Transfer-Encoding: base64\r\n"));
        assert!(mime.contains("Content-Disposition: attachment; filename=\"notes.txt\"\r\n"));
        // Quotes in filenames are sanitized so they cannot break the header.
        assert!(mime.contains("filename=\"we'ird.pdf\""));
        assert!(!mime.contains("we\"ird.pdf"));
        assert!(mime.ends_with("--AGENTRQ_MIXED_BOUNDARY--\r\n"));
    }

    #[test]
    fn adds_threading_headers_for_replies() {
        let mut mail = plain_mail();
        mail.in_reply_to = Some("<abc123@example.com>".to_string());
        let mime = build_mime(&mail).unwrap();
        assert!(mime.contains("In-Reply-To: <abc123@example.com>\r\n"));
        assert!(mime.contains("References: <abc123@example.com>\r\n"));

        let mut bare = plain_mail();
        bare.in_reply_to = None;
        let mime = build_mime(&bare).unwrap();
        assert!(!mime.contains("In-Reply-To"));
    }

    #[test]
    fn rejects_bad_attachment_base64() {
        let mut mail = plain_mail();
        mail.attachments = vec![AttachmentInput {
            filename: "x.bin".to_string(),
            mime_type: "application/octet-stream".to_string(),
            content_base64: "!!!".to_string(),
        }];
        let error = build_mime(&mail).unwrap_err();
        assert!(
            matches!(&error, ConnectorError::InvalidInput(message)
                if message.contains("contentBase64 is not valid base64")),
            "{error:?}"
        );
    }

    #[test]
    fn parses_full_messages() {
        let message: ApiMessage = serde_json::from_value(rich_message_fixture()).unwrap();
        let email = parse_email(&message, true);
        assert_eq!(email.id, "m1");
        assert_eq!(email.thread_id, "t1");
        assert_eq!(email.from, "Alice <alice@example.com>");
        assert_eq!(email.to, "bob@example.com");
        assert_eq!(email.cc, "carol@example.com");
        assert_eq!(email.subject, "Hello");
        assert_eq!(email.date, "Sat, 10 Oct 2026 10:00:00 -0700");
        assert_eq!(email.snippet, "Hello there...");
        // The first text/plain part wins.
        assert_eq!(email.body_text, "Hello there");
        assert_eq!(email.body_html.as_deref(), Some("<b>Hello there</b>"));
        assert_eq!(email.attachments.len(), 2);
        assert_eq!(email.attachments[0].filename, "doc.pdf");
        assert_eq!(email.attachments[0].mime_type, "application/pdf");
        assert_eq!(email.attachments[0].size, 1234);
        assert_eq!(email.attachments[0].attachment_id.as_deref(), Some("att1"));
        // An attachment part without an id keeps its metadata; the id is
        // simply absent.
        assert_eq!(email.attachments[1].filename, "inline.png");
        assert_eq!(email.attachments[1].attachment_id, None);
    }

    #[test]
    fn skips_html_unless_asked() {
        let message: ApiMessage = serde_json::from_value(rich_message_fixture()).unwrap();
        let email = parse_email(&message, false);
        assert_eq!(email.body_text, "Hello there");
        assert_eq!(email.body_html, None);
        let serialized = serde_json::to_value(&email).unwrap();
        assert!(serialized.get("bodyHtml").is_none());
    }

    #[test]
    fn skips_html_parts_without_data() {
        // An html part with no body data contributes nothing, even when
        // HTML is wanted.
        let message: ApiMessage = serde_json::from_value(json!({
            "id": "m1",
            "threadId": "t1",
            "payload": {
                "mimeType": "multipart/alternative",
                "headers": [],
                "body": {"size": 0},
                "parts": [
                    {"mimeType": "text/html", "headers": [], "body": {"size": 10}, "parts": []},
                ],
            },
        }))
        .unwrap();
        let email = parse_email(&message, true);
        assert_eq!(email.body_html, None);
    }

    #[test]
    fn parses_minimal_messages() {
        // No snippet, no body on the part, headers missing: everything
        // falls back to empty.
        let message: ApiMessage = serde_json::from_value(json!({
            "id": "m9",
            "threadId": "t9",
            "payload": {"mimeType": "text/plain", "headers": []},
        }))
        .unwrap();
        let email = parse_email(&message, true);
        assert_eq!(email.snippet, "");
        assert_eq!(email.body_text, "");
        assert_eq!(email.from, "");
        assert!(email.attachments.is_empty());
    }

    #[test]
    fn parses_organize_actions() {
        assert_eq!("archive".parse(), Ok(OrganizeAction::Archive));
        assert_eq!("trash".parse(), Ok(OrganizeAction::Trash));
        assert_eq!("untrash".parse(), Ok(OrganizeAction::Untrash));
        assert_eq!("markRead".parse(), Ok(OrganizeAction::MarkRead));
        assert_eq!("markUnread".parse(), Ok(OrganizeAction::MarkUnread));
        assert_eq!("addLabels".parse(), Ok(OrganizeAction::AddLabels));
        assert_eq!("removeLabels".parse(), Ok(OrganizeAction::RemoveLabels));
        assert_eq!("Archive".parse(), Ok(OrganizeAction::Archive));
        let error: Result<OrganizeAction, _> = "explode".parse();
        assert_eq!(
            error.unwrap_err(),
            ConnectorError::InvalidInput("unknown organize action: explode".into())
        );
    }

    #[test]
    fn names_organize_actions() {
        assert_eq!(OrganizeAction::Archive.name(), "archive");
        assert_eq!(OrganizeAction::Trash.name(), "trash");
        assert_eq!(OrganizeAction::Untrash.name(), "untrash");
        assert_eq!(OrganizeAction::MarkRead.name(), "markRead");
        assert_eq!(OrganizeAction::MarkUnread.name(), "markUnread");
        assert_eq!(OrganizeAction::AddLabels.name(), "addLabels");
        assert_eq!(OrganizeAction::RemoveLabels.name(), "removeLabels");
    }

    #[test]
    fn maps_organize_actions_to_labels() {
        assert_eq!(
            OrganizeAction::Archive.label_delta(None).unwrap(),
            (Vec::new(), vec!["INBOX".to_string()])
        );
        assert_eq!(
            OrganizeAction::Trash.label_delta(None).unwrap(),
            (vec!["TRASH".to_string()], Vec::new())
        );
        assert_eq!(
            OrganizeAction::Untrash.label_delta(None).unwrap(),
            (Vec::new(), vec!["TRASH".to_string()])
        );
        assert_eq!(
            OrganizeAction::MarkRead.label_delta(None).unwrap(),
            (Vec::new(), vec!["UNREAD".to_string()])
        );
        assert_eq!(
            OrganizeAction::MarkUnread.label_delta(None).unwrap(),
            (vec!["UNREAD".to_string()], Vec::new())
        );
        let labels = Some(vec!["L1".to_string()]);
        assert_eq!(
            OrganizeAction::AddLabels
                .label_delta(labels.clone())
                .unwrap(),
            (vec!["L1".to_string()], Vec::new())
        );
        assert_eq!(
            OrganizeAction::RemoveLabels.label_delta(labels).unwrap(),
            (Vec::new(), vec!["L1".to_string()])
        );
        assert_eq!(
            OrganizeAction::AddLabels.label_delta(None).unwrap_err(),
            ConnectorError::InvalidInput("addLabels needs labelIds".into())
        );
        assert_eq!(
            OrganizeAction::RemoveLabels.label_delta(None).unwrap_err(),
            ConnectorError::InvalidInput("removeLabels needs labelIds".into())
        );
    }

    #[test]
    fn parses_label_actions() {
        assert_eq!("list".parse(), Ok(LabelAction::List));
        assert_eq!("create".parse(), Ok(LabelAction::Create));
        assert_eq!("Rename".parse(), Ok(LabelAction::Rename));
        assert_eq!("DELETE".parse(), Ok(LabelAction::Delete));
        let error: Result<LabelAction, _> = "upsert".parse();
        assert_eq!(
            error.unwrap_err(),
            ConnectorError::InvalidInput("unknown label action: upsert".into())
        );
    }

    #[test]
    fn converts_connector_errors_to_tool_input_errors() {
        let error = ToolError::from(ConnectorError::InvalidInput("bad".into()));
        assert!(matches!(error, ToolError::Input(_))); // coverage: exclude — matches! false arm is unreachable in a passing test
    }

    #[test]
    fn searches_emails_end_to_end() {
        let list = json!({
            "messages": [{"id": "m1", "threadId": "t1"}, {"id": "m2", "threadId": "t2"}],
            "resultSizeEstimate": 2,
        });
        let mut script = Script::new()
            .will_answer(ok(&list))
            .will_answer(ok(&message_fixture(
                "m1",
                "t1",
                "First",
                "alice@example.com",
                "Hello one",
            )))
            .will_answer(ok(&message_fixture(
                "m2",
                "t2",
                "Second",
                "bob@example.com",
                "Hello two",
            )));
        let result = script.call("searchEmails", r#"{"query":"from:boss","maxResults":5}"#);

        assert_eq!(script.seen.len(), 3);
        assert_eq!(script.seen_method(0), Method::Get);
        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/messages?q=from%3Aboss&maxResults=5"
        );
        assert_eq!(
            script.seen_url(1),
            "https://www.googleapis.com/gmail/v1/users/me/messages/m1?format=full"
        );
        assert_eq!(
            script.seen_url(2),
            "https://www.googleapis.com/gmail/v1/users/me/messages/m2?format=full"
        );

        let payload = structured(&result);
        assert_eq!(payload["messages"].as_array().unwrap().len(), 2);
        assert_eq!(payload["messages"][0]["id"], "m1");
        assert_eq!(payload["messages"][0]["subject"], "First");
        assert_eq!(payload["messages"][0]["from"], "alice@example.com");
        assert_eq!(payload["messages"][0]["bodyText"], "Hello one");
        assert_eq!(payload["messages"][1]["subject"], "Second");
    }

    #[test]
    fn search_emails_defaults_and_caps_max_results() {
        let mut script = Script::new().will_answer(ok(&json!({"messages": []})));
        let result = script.call("searchEmails", r#"{"query":"x","maxResults":500}"#);
        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/messages?q=x&maxResults=50"
        );
        assert_eq!(structured(&result)["messages"].as_array().unwrap().len(), 0);

        let mut script = Script::new().will_answer(ok(&json!({})));
        let result = script.call("searchEmails", r#"{"query":"x","maxResults":0}"#);
        assert!(script.seen_url(0).ends_with("maxResults=1"));
        assert_eq!(structured(&result)["messages"].as_array().unwrap().len(), 0);

        // Messages without an id are skipped.
        let mut script = Script::new()
            .will_answer(ok(&json!({"messages": [{"id": "m1"}, {"nope": true}]})))
            .will_answer(ok(&message_fixture("m1", "t1", "S", "a@b.c", "text")));
        let result = script.call("searchEmails", r#"{"query":"x"}"#);
        assert!(script.seen_url(0).ends_with("maxResults=10"));
        assert_eq!(structured(&result)["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn search_emails_reports_failed_steps() {
        let mut script = Script::new().will_answer(response(403, "insufficient scope"));
        let result = script.call("searchEmails", r#"{"query":"x"}"#);
        assert!(is_tool_error(&result));

        let mut script = Script::new()
            .will_answer(ok(&json!({"messages": [{"id": "m1"}, {"id": "m2"}]})))
            .will_answer(ok(&message_fixture("m1", "t1", "S", "a@b.c", "text")))
            .will_answer(response(404, "not found"));
        let result = script.call("searchEmails", r#"{"query":"x"}"#);
        assert!(is_tool_error(&result));

        // A 2xx body that is not a message is a named tool error too.
        let mut script = Script::new()
            .will_answer(ok(&json!({"messages": [{"id": "m1"}]})))
            .will_answer(ok(&json!({"id": 42})));
        let result = script.call("searchEmails", r#"{"query":"x"}"#);
        assert!(is_tool_error(&result));

        // A broken sender is a named tool error as well.
        let mut script = Script::new().will_fail(ConnectorError::Upstream("boom".into()));
        let result = script.call("searchEmails", r#"{"query":"x"}"#);
        assert!(is_tool_error(&result));

        // Garbage JSON in a 200 is a named tool error.
        let mut script = Script::new().will_answer(response(200, "<html>not json</html>"));
        let result = script.call("searchEmails", r#"{"query":"x"}"#);
        assert!(is_tool_error(&result));
    }

    #[test]
    fn reads_threads_end_to_end() {
        let thread = json!({
            "id": "t1",
            "messages": [
                message_fixture("m1", "t1", "Thread subject", "alice@example.com", "First"),
                message_fixture("m2", "t1", "Re: Thread subject", "bob@example.com", "Second"),
            ],
        });
        let mut script = Script::new().will_answer(ok(&thread));
        let result = script.call("readThread", r#"{"threadId":"t1"}"#);

        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/threads/t1?format=full"
        );
        let payload = structured(&result);
        assert_eq!(payload["id"], "t1");
        assert_eq!(payload["subject"], "Thread subject");
        assert_eq!(payload["messages"].as_array().unwrap().len(), 2);
        assert_eq!(payload["messages"][0]["bodyText"], "First");
        assert_eq!(payload["messages"][1]["bodyText"], "Second");

        let mut script = Script::new().will_answer(response(404, "not found"));
        assert!(is_tool_error(
            &script.call("readThread", r#"{"threadId":"t1"}"#)
        ));

        // An empty thread has no subject and no messages.
        let mut script = Script::new().will_answer(ok(&json!({"id": "t2"})));
        let payload = structured(&script.call("readThread", r#"{"threadId":"t2"}"#));
        assert_eq!(payload["subject"], "");
        assert_eq!(payload["messages"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn sends_email_end_to_end() {
        let mut script = Script::new().will_answer(ok(&json!({
            "id": "sent1",
            "threadId": "thread1",
            "labelIds": ["SENT"],
        })));
        let result = script.call(
            "sendEmail",
            r#"{
                "to": ["bob@example.com"],
                "cc": ["carol@example.com"],
                "bcc": ["dan@example.com"],
                "subject": "Hello",
                "body": "Hi Bob",
                "htmlBody": "<b>Hi Bob</b>",
                "attachments": [
                    {"filename": "n.txt", "mimeType": "text/plain",
                     "contentBase64": "bm90ZXM="}
                ]
            }"#,
        );

        assert_eq!(script.seen_method(0), Method::Post);
        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/messages/send"
        );
        let body = script.seen_json(0);
        let raw = body["raw"].as_str().unwrap();
        let mime = String::from_utf8(URL_SAFE_NO_PAD.decode(raw).unwrap()).unwrap();
        assert!(mime.contains("To: bob@example.com\r\n"));
        assert!(mime.contains("Cc: carol@example.com\r\n"));
        assert!(mime.contains("Bcc: dan@example.com\r\n"));
        assert!(mime.contains("Subject: Hello\r\n"));
        assert!(mime.contains("multipart/mixed"));
        assert!(mime.contains("multipart/alternative"));
        assert!(mime.contains("name=\"n.txt\""));

        let payload = structured(&result);
        assert_eq!(payload, json!({"id": "sent1", "threadId": "thread1"}));

        let mut script = Script::new().will_answer(response(400, "bad request"));
        assert!(is_tool_error(&script.call(
            "sendEmail",
            r#"{"to":["a@b.c"],"subject":"s","body":"b"}"#
        )));
    }

    #[test]
    fn send_email_needs_a_recipient() {
        let mut script = Script::new();
        let result = script.call("sendEmail", r#"{"to":[],"subject":"s","body":"b"}"#);
        assert_eq!(
            result.unwrap_err(),
            ConnectorError::InvalidInput(
                "sendEmail needs at least one recipient in to, cc, or bcc".into()
            )
        );
        assert!(script.seen.is_empty());

        // A cc alone is enough.
        let mut script = Script::new().will_answer(ok(&json!({"id": "s", "threadId": "t"})));
        let result = script.call(
            "sendEmail",
            r#"{"to":[],"cc":["c@d.e"],"subject":"s","body":"b"}"#,
        );
        assert!(structured(&result)["id"] == "s");
    }

    #[test]
    fn replies_thread_the_conversation() {
        let mut script = Script::new()
            .will_answer(ok(&metadata_fixture()))
            .will_answer(ok(&json!({"id": "r1", "threadId": "t9"})));
        let result = script.call(
            "replyToEmail",
            r#"{"messageId":"m1","body":"On it!","cc":["teammate@example.com"]}"#,
        );

        assert_eq!(script.seen.len(), 2);
        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/messages/m1\
             ?format=metadata&metadataHeaders=From%2CTo%2CCc%2CSubject%2CMessage-ID"
        );
        assert_eq!(
            script.seen_url(1),
            "https://www.googleapis.com/gmail/v1/users/me/messages/send"
        );
        let body = script.seen_json(1);
        assert_eq!(body["threadId"], "t9");
        let mime = String::from_utf8(
            URL_SAFE_NO_PAD
                .decode(body["raw"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert!(mime.contains("To: alice@example.com\r\n"));
        assert!(mime.contains("Cc: teammate@example.com\r\n"));
        assert!(mime.contains("Subject: Re: Project update\r\n"));
        assert!(mime.contains("In-Reply-To: <msg1@example.com>\r\n"));
        assert!(mime.contains("References: <msg1@example.com>\r\n"));
        assert!(mime.contains("On it!"));

        let payload = structured(&result);
        assert_eq!(payload, json!({"id": "r1", "threadId": "t9"}));
    }

    #[test]
    fn reply_to_email_handles_odd_originals() {
        // A subject that already starts with Re: is left alone, and a
        // missing Message-ID just means no threading headers.
        let mut metadata = metadata_fixture();
        metadata["payload"]["headers"] = json!([
            {"name": "From", "value": "bare@example.com"},
            {"name": "Subject", "value": "Re: Project update"},
        ]);
        let mut script = Script::new()
            .will_answer(ok(&metadata))
            .will_answer(ok(&json!({"id": "r1", "threadId": "t9"})));
        let result = script.call("replyToEmail", r#"{"messageId":"m1","body":"b"}"#);
        let body = script.seen_json(1);
        let mime = String::from_utf8(
            URL_SAFE_NO_PAD
                .decode(body["raw"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert!(mime.contains("To: bare@example.com\r\n"));
        assert!(mime.contains("Subject: Re: Project update\r\n"));
        assert!(!mime.contains("In-Reply-To"));
        assert!(structured(&result)["id"] == "r1");

        // Without a From header there is nobody to reply to.
        let mut metadata = metadata_fixture();
        metadata["payload"]["headers"] = json!([{"name": "Subject", "value": "x"}]);
        let mut script = Script::new().will_answer(ok(&metadata));
        assert!(is_tool_error(
            &script.call("replyToEmail", r#"{"messageId":"m1","body":"b"}"#)
        ));

        // A missing original is a named tool error.
        let mut script = Script::new().will_answer(response(404, "not found"));
        assert!(is_tool_error(
            &script.call("replyToEmail", r#"{"messageId":"m1","body":"b"}"#)
        ));

        // So is a send that the API refuses.
        let mut script = Script::new()
            .will_answer(ok(&metadata_fixture()))
            .will_answer(response(500, "server error"));
        assert!(is_tool_error(
            &script.call("replyToEmail", r#"{"messageId":"m1","body":"b"}"#)
        ));
    }

    #[test]
    fn forwards_with_a_note() {
        let mut script = Script::new()
            .will_answer(ok(&rich_message_fixture()))
            .will_answer(ok(&json!({"id": "f1", "threadId": "tf"})));
        let result = script.call(
            "forwardEmail",
            r#"{"messageId":"m1","to":["zoe@example.com"],"body":"FYI"}"#,
        );

        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/messages/m1?format=full"
        );
        let body = script.seen_json(1);
        let mime = String::from_utf8(
            URL_SAFE_NO_PAD
                .decode(body["raw"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert!(mime.contains("To: zoe@example.com\r\n"));
        assert!(mime.contains("Subject: Fwd: Hello\r\n"));
        assert!(mime.contains("FYI\n\n---------- Forwarded message ----------\n"));
        assert!(mime.contains("From: Alice <alice@example.com>\n"));
        assert!(mime.contains("Hello there"));
        assert_eq!(structured(&result)["id"], "f1");
    }

    #[test]
    fn forwards_without_a_note_and_keeps_existing_prefix() {
        let mut message = rich_message_fixture();
        message["payload"]["headers"][3] = json!({"name": "Subject", "value": "Fwd: Hello"});
        let mut script = Script::new()
            .will_answer(ok(&message))
            .will_answer(ok(&json!({"id": "f1", "threadId": "tf"})));
        let result = script.call(
            "forwardEmail",
            r#"{"messageId":"m1","to":["zoe@example.com"]}"#,
        );
        assert_eq!(structured(&result)["id"], "f1");
        let body = script.seen_json(1);
        let mime = String::from_utf8(
            URL_SAFE_NO_PAD
                .decode(body["raw"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert!(mime.contains("Subject: Fwd: Hello\r\n"));
        assert!(!mime.contains("Fwd: Fwd:"));
        assert!(mime.contains("---------- Forwarded message ----------\nFrom:"));
    }

    #[test]
    fn forward_email_reports_failed_steps() {
        let mut script = Script::new().will_answer(response(404, "not found"));
        assert!(is_tool_error(
            &script.call("forwardEmail", r#"{"messageId":"m1","to":["z@e.c"]}"#)
        ));

        let mut script = Script::new()
            .will_answer(ok(&rich_message_fixture()))
            .will_answer(response(500, "server error"));
        assert!(is_tool_error(
            &script.call("forwardEmail", r#"{"messageId":"m1","to":["z@e.c"]}"#)
        ));
    }

    #[test]
    fn saves_and_updates_drafts() {
        let created = json!({"id": "d1", "message": {"id": "m1", "threadId": "t1"}});
        let mut script = Script::new().will_answer(ok(&created));
        let result = script.call(
            "saveDraft",
            r#"{"to":["bob@example.com"],"subject":"Draft","body":"later"}"#,
        );
        assert_eq!(script.seen_method(0), Method::Post);
        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/drafts"
        );
        let body = script.seen_json(0);
        let mime = String::from_utf8(
            URL_SAFE_NO_PAD
                .decode(body["message"]["raw"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert!(mime.contains("Subject: Draft\r\n"));
        assert!(body["message"].get("threadId").is_none());
        assert_eq!(
            structured(&result),
            json!({"draftId": "d1", "messageId": "m1"})
        );

        let mut script = Script::new().will_answer(ok(&created));
        let result = script.call(
            "saveDraft",
            r#"{"draftId":"d1","to":["bob@example.com"],"subject":"Draft 2","body":"later"}"#,
        );
        assert_eq!(script.seen_method(0), Method::Put);
        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/drafts/d1"
        );
        assert_eq!(structured(&result)["draftId"], "d1");

        let mut script = Script::new().will_answer(response(400, "bad"));
        assert!(is_tool_error(
            &script.call("saveDraft", r#"{"to":["a@b.c"]}"#)
        ));
    }

    #[test]
    fn save_draft_threads_replies() {
        let mut script = Script::new()
            .will_answer(ok(&metadata_fixture()))
            .will_answer(ok(
                &json!({"id": "d2", "message": {"id": "m2", "threadId": "t9"}}),
            ));
        let result = script.call(
            "saveDraft",
            r#"{"to":["alice@example.com"],"subject":"x","body":"y","inReplyToMessageId":"m1"}"#,
        );
        assert_eq!(script.seen.len(), 2);
        let body = script.seen_json(1);
        assert_eq!(body["message"]["threadId"], "t9");
        let mime = String::from_utf8(
            URL_SAFE_NO_PAD
                .decode(body["message"]["raw"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert!(mime.contains("In-Reply-To: <msg1@example.com>\r\n"));
        assert_eq!(
            structured(&result),
            json!({"draftId": "d2", "messageId": "m2"})
        );
    }

    #[test]
    fn sends_drafts() {
        let mut script = Script::new().will_answer(ok(&json!({
            "id": "s1",
            "threadId": "t1",
        })));
        let result = script.call("sendDraft", r#"{"draftId":"d1"}"#);
        assert_eq!(script.seen_method(0), Method::Post);
        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/drafts/send"
        );
        assert_eq!(script.seen_json(0), json!({"id": "d1"}));
        assert_eq!(structured(&result), json!({"id": "s1", "threadId": "t1"}));

        let mut script = Script::new().will_answer(response(404, "no such draft"));
        assert!(is_tool_error(
            &script.call("sendDraft", r#"{"draftId":"d1"}"#)
        ));
    }

    #[test]
    fn discards_drafts() {
        let mut script = Script::new().will_answer(response(204, ""));
        let result = script.call("discardDraft", r#"{"draftId":"d1"}"#);
        assert_eq!(script.seen_method(0), Method::Delete);
        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/drafts/d1"
        );
        assert_eq!(structured(&result), json!({"deleted": true}));

        let mut script = Script::new().will_answer(response(404, "nope"));
        assert!(is_tool_error(
            &script.call("discardDraft", r#"{"draftId":"d1"}"#)
        ));
    }

    #[test]
    fn organizes_messages() {
        let mut script = Script::new().will_answer(ok(&json!({})));
        let result = script.call(
            "organizeEmail",
            r#"{"action":"archive","messageIds":["m1","m2"]}"#,
        );
        assert_eq!(script.seen_method(0), Method::Post);
        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/messages/batchModify"
        );
        assert_eq!(
            script.seen_json(0),
            json!({"ids": ["m1", "m2"], "removeLabelIds": ["INBOX"]})
        );
        assert_eq!(
            structured(&result),
            json!({"action": "archive", "affectedCount": 2})
        );
    }

    #[test]
    fn organizes_by_query_with_deduplication() {
        let mut script = Script::new()
            .will_answer(ok(&json!({"messages": [{"id": "m2"}, {"id": "m3"}]})))
            .will_answer(ok(&json!({})));
        let result = script.call(
            "organizeEmail",
            r#"{"action":"trash","messageIds":["m1","m2"],"query":"is:unread"}"#,
        );
        assert_eq!(script.seen.len(), 2);
        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/messages?q=is%3Aunread&maxResults=500"
        );
        assert_eq!(
            script.seen_json(1),
            json!({"ids": ["m1", "m2", "m3"], "addLabelIds": ["TRASH"]})
        );
        assert_eq!(structured(&result)["affectedCount"], 3);
    }

    #[test]
    fn organizes_a_multi_page_query() {
        // Arrange: the query matches two pages of messages.
        let mut script = Script::new()
            .will_answer(ok(&json!({
                "messages": [{"id": "m1"}],
                "nextPageToken": "tok",
            })))
            .will_answer(ok(&json!({"messages": [{"id": "m2"}]})))
            .will_answer(ok(&json!({})));
        let result = script.call(
            "organizeEmail",
            r#"{"action":"archive","query":"is:unread"}"#,
        );

        // Assert: both pages were listed, then both messages archived.
        assert_eq!(script.seen.len(), 3);
        assert_eq!(
            script.seen_url(1),
            "https://www.googleapis.com/gmail/v1/users/me/messages?q=is%3Aunread&maxResults=500&pageToken=tok"
        );
        assert_eq!(
            script.seen_json(2),
            json!({"ids": ["m1", "m2"], "removeLabelIds": ["INBOX"]})
        );
        assert_eq!(structured(&result)["affectedCount"], 2);
    }

    #[test]
    fn organizes_threads_one_call_each() {
        let mut script = Script::new()
            .will_answer(ok(&json!({})))
            .will_answer(ok(&json!({})));
        let result = script.call(
            "organizeEmail",
            r#"{"action":"markRead","threadIds":["t1","t2"]}"#,
        );
        assert_eq!(script.seen.len(), 2);
        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/threads/t1/modify"
        );
        assert_eq!(script.seen_json(0), json!({"removeLabelIds": ["UNREAD"]}));
        assert_eq!(
            script.seen_url(1),
            "https://www.googleapis.com/gmail/v1/users/me/threads/t2/modify"
        );
        assert_eq!(
            structured(&result),
            json!({"action": "markRead", "affectedCount": 2})
        );
    }

    #[test]
    fn organizes_messages_and_threads_together() {
        let mut script = Script::new()
            .will_answer(ok(&json!({})))
            .will_answer(ok(&json!({})));
        let result = script.call(
            "organizeEmail",
            r#"{"action":"addLabels","messageIds":["m1"],"threadIds":["t1"],"labelIds":["L1"]}"#,
        );
        assert_eq!(
            script.seen_json(0),
            json!({"ids": ["m1"], "addLabelIds": ["L1"]})
        );
        assert_eq!(script.seen_json(1), json!({"addLabelIds": ["L1"]}));
        assert_eq!(structured(&result)["affectedCount"], 2);
    }

    #[test]
    fn organize_email_validates_input() {
        let mut script = Script::new();
        assert_eq!(
            script
                .call(
                    "organizeEmail",
                    r#"{"action":"explode","messageIds":["m1"]}"#
                )
                .unwrap_err(),
            ConnectorError::InvalidInput("unknown organize action: explode".into())
        );

        let mut script = Script::new();
        assert_eq!(
            script
                .call(
                    "organizeEmail",
                    r#"{"action":"addLabels","messageIds":["m1"]}"#
                )
                .unwrap_err(),
            ConnectorError::InvalidInput("addLabels needs labelIds".into())
        );

        let mut script = Script::new();
        assert_eq!(
            script
                .call("organizeEmail", r#"{"action":"archive"}"#)
                .unwrap_err(),
            ConnectorError::InvalidInput(
                "organizeEmail needs messageIds, threadIds, or query".into()
            )
        );
        assert!(script.seen.is_empty());

        // A query that matches nothing is a successful no-op, not an error.
        let mut script = Script::new().will_answer(ok(&json!({})));
        let result = script.call("organizeEmail", r#"{"action":"trash","query":"zzz"}"#);
        assert_eq!(
            structured(&result),
            json!({"action": "trash", "affectedCount": 0})
        );
    }

    #[test]
    fn organize_email_reports_failed_steps() {
        let mut script = Script::new().will_answer(response(400, "bad labels"));
        let result = script.call(
            "organizeEmail",
            r#"{"action":"markUnread","messageIds":["m1"]}"#,
        );
        assert!(is_tool_error(&result));

        let mut script = Script::new().will_answer(response(404, "no thread"));
        let result = script.call(
            "organizeEmail",
            r#"{"action":"untrash","threadIds":["t1"]}"#,
        );
        assert!(is_tool_error(&result));

        let mut script = Script::new().will_answer(response(403, "no scope"));
        let result = script.call("organizeEmail", r#"{"action":"archive","query":"x"}"#);
        assert!(is_tool_error(&result));
    }

    #[test]
    fn gets_attachments_by_filename() {
        let mut script = Script::new()
            .will_answer(ok(&rich_message_fixture()))
            .will_answer(ok(&json!({
                "size": 1234,
                "data": URL_SAFE_NO_PAD.encode("pdf bytes"),
            })));
        let result = script.call(
            "getAttachment",
            r#"{"messageId":"m1","filename":"doc.pdf"}"#,
        );

        assert_eq!(script.seen.len(), 2);
        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/messages/m1?format=full"
        );
        assert_eq!(
            script.seen_url(1),
            "https://www.googleapis.com/gmail/v1/users/me/messages/m1/attachments/att1"
        );
        assert_eq!(
            structured(&result),
            json!({
                "filename": "doc.pdf",
                "mimeType": "application/pdf",
                "size": 1234,
                "contentBase64": URL_SAFE_NO_PAD.encode("pdf bytes"),
            })
        );
    }

    #[test]
    fn gets_attachments_by_id() {
        let mut script = Script::new()
            .will_answer(ok(&rich_message_fixture()))
            .will_answer(ok(&json!({"size": 5, "data": "aGk="})));
        let result = script.call(
            "getAttachment",
            r#"{"messageId":"m1","attachmentId":"att1"}"#,
        );
        let payload = structured(&result);
        assert_eq!(payload["filename"], "doc.pdf");
        assert_eq!(payload["contentBase64"], "aGk=");
    }

    #[test]
    fn get_attachment_reports_failed_downloads() {
        let mut script = Script::new()
            .will_answer(ok(&rich_message_fixture()))
            .will_answer(response(404, "gone"));
        assert!(is_tool_error(&script.call(
            "getAttachment",
            r#"{"messageId":"m1","filename":"doc.pdf"}"#
        )));
    }

    #[test]
    fn get_attachment_validates_input() {
        let mut script = Script::new();
        assert_eq!(
            script
                .call("getAttachment", r#"{"messageId":"m1"}"#)
                .unwrap_err(),
            ConnectorError::InvalidInput("getAttachment needs attachmentId or filename".into())
        );
        assert!(script.seen.is_empty());

        let mut script = Script::new().will_answer(ok(&rich_message_fixture()));
        assert!(is_tool_error(&script.call(
            "getAttachment",
            r#"{"messageId":"m1","filename":"nope.txt"}"#
        )));

        // An attachment part with no download id cannot be fetched.
        let mut script = Script::new().will_answer(ok(&rich_message_fixture()));
        assert!(is_tool_error(&script.call(
            "getAttachment",
            r#"{"messageId":"m1","filename":"inline.png"}"#
        )));

        let mut script = Script::new().will_answer(response(404, "gone"));
        assert!(is_tool_error(&script.call(
            "getAttachment",
            r#"{"messageId":"m1","filename":"doc.pdf"}"#
        )));
    }

    #[test]
    fn manages_labels() {
        let labels = json!({"labels": [{"id": "L1", "name": "Receipts"}]});

        let mut script = Script::new().will_answer(ok(&labels));
        let result = script.call("manageLabels", r#"{"action":"list"}"#);
        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/labels"
        );
        assert_eq!(structured(&result), labels);

        let created = json!({"id": "L2", "name": "Invoices"});
        let mut script = Script::new().will_answer(ok(&created));
        let result = script.call(
            "manageLabels",
            r#"{"action":"create","name":"Invoices","messageListVisibility":"show"}"#,
        );
        assert_eq!(script.seen_method(0), Method::Post);
        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/labels"
        );
        assert_eq!(
            script.seen_json(0),
            json!({"name": "Invoices", "messageListVisibility": "show"})
        );
        assert_eq!(structured(&result), created);

        let renamed = json!({"id": "L2", "name": "Bills"});
        let mut script = Script::new().will_answer(ok(&renamed));
        let result = script.call(
            "manageLabels",
            r#"{"action":"rename","labelId":"L2","name":"Bills"}"#,
        );
        assert_eq!(script.seen_method(0), Method::Put);
        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/labels/L2"
        );
        assert_eq!(script.seen_json(0), json!({"name": "Bills"}));
        assert_eq!(structured(&result), renamed);

        let mut script = Script::new().will_answer(response(204, ""));
        let result = script.call("manageLabels", r#"{"action":"delete","labelId":"L2"}"#);
        assert_eq!(script.seen_method(0), Method::Delete);
        assert_eq!(structured(&result), json!({"deleted": true}));

        let mut script = Script::new().will_answer(response(400, "bad"));
        assert!(is_tool_error(
            &script.call("manageLabels", r#"{"action":"create","name":"x"}"#)
        ));
    }

    #[test]
    fn manage_labels_passes_visibility_options() {
        let mut script = Script::new().will_answer(ok(&json!({"id": "L3"})));
        script
            .call(
                "manageLabels",
                r#"{"action":"create","name":"X","messageListVisibility":"hide","labelListVisibility":"labelHide"}"#,
            )
            .unwrap();
        assert_eq!(
            script.seen_json(0),
            json!({
                "name": "X",
                "messageListVisibility": "hide",
                "labelListVisibility": "labelHide",
            })
        );

        let mut script = Script::new().will_answer(ok(&json!({"id": "L3"})));
        script
            .call(
                "manageLabels",
                r#"{"action":"rename","labelId":"L3","name":"Y","messageListVisibility":"show","labelListVisibility":"labelShow"}"#,
            )
            .unwrap();
        assert_eq!(
            script.seen_json(0),
            json!({
                "name": "Y",
                "messageListVisibility": "show",
                "labelListVisibility": "labelShow",
            })
        );
    }

    #[test]
    fn manage_labels_validates_input() {
        let mut script = Script::new();
        assert_eq!(
            script
                .call("manageLabels", r#"{"action":"upsert"}"#)
                .unwrap_err(),
            ConnectorError::InvalidInput("unknown label action: upsert".into())
        );

        let mut script = Script::new();
        assert_eq!(
            script
                .call("manageLabels", r#"{"action":"create"}"#)
                .unwrap_err(),
            ConnectorError::InvalidInput("create needs a name".into())
        );

        let mut script = Script::new();
        assert_eq!(
            script
                .call("manageLabels", r#"{"action":"rename","name":"x"}"#)
                .unwrap_err(),
            ConnectorError::InvalidInput("rename needs a labelId".into())
        );

        let mut script = Script::new();
        assert_eq!(
            script
                .call("manageLabels", r#"{"action":"rename","labelId":"L1"}"#)
                .unwrap_err(),
            ConnectorError::InvalidInput("rename needs a name".into())
        );

        let mut script = Script::new();
        assert_eq!(
            script
                .call("manageLabels", r#"{"action":"delete"}"#)
                .unwrap_err(),
            ConnectorError::InvalidInput("delete needs a labelId".into())
        );
        assert!(script.seen.is_empty());
    }

    #[test]
    fn gets_profiles() {
        let mut script = Script::new().will_answer(ok(&json!({
            "emailAddress": "me@example.com",
            "messagesTotal": 42,
            "threadsTotal": 7,
            "historyId": "123",
        })));
        let result = script.call("getProfile", "{}");
        assert_eq!(
            script.seen_url(0),
            "https://www.googleapis.com/gmail/v1/users/me/profile"
        );
        // Only the fields agents need; the history cursor is dropped.
        assert_eq!(
            structured(&result),
            json!({
                "emailAddress": "me@example.com",
                "messagesTotal": 42,
                "threadsTotal": 7,
            })
        );

        let mut script = Script::new().will_answer(response(401, "unauthorized"));
        assert!(is_tool_error(&script.call("getProfile", "{}")));
    }

    #[test]
    fn rejects_bad_input() {
        let mut script = Script::new();
        let error = script.call("searchEmails", "{}").unwrap_err();
        assert!(matches!(error, ConnectorError::InvalidInput(_))); // coverage: exclude — matches! false arm is unreachable in a passing test
        assert!(script.seen.is_empty());
        // Every dispatch arm parses the body with `request.json()?`: malformed
        // JSON must fail input validation before anything is sent.
        for tool in [
            "searchEmails",
            "readThread",
            "sendEmail",
            "replyToEmail",
            "forwardEmail",
            "saveDraft",
            "sendDraft",
            "discardDraft",
            "organizeEmail",
            "getAttachment",
            "manageLabels",
            "getProfile",
        ] {
            let mut script = Script::new();
            let error = script.call(tool, "{oops").unwrap_err();
            assert!(matches!(error, ConnectorError::InvalidInput(_)), "{tool}"); // coverage: exclude — matches! false arm is unreachable in a passing test
            assert!(script.seen.is_empty(), "{tool} sent nothing");
        }
    }

    #[test]
    fn surfaces_garbled_message_bodies() {
        // A 2xx body that is not a message is a named tool error: the API
        // answered, but not with what was asked for.
        let mut script =
            Script::new().will_answer(ok(&json!({"id": "t1", "messages": [{"bogus": true}]})));
        assert!(is_tool_error(
            &script.call("readThread", r#"{"threadId":"t1"}"#)
        ));

        let mut script = Script::new().will_answer(ok(&json!({"bogus": true})));
        assert!(is_tool_error(
            &script.call("forwardEmail", r#"{"messageId":"m1","to":["a@b.c"]}"#)
        ));

        let mut script = Script::new().will_answer(ok(&json!({"bogus": true})));
        assert!(is_tool_error(&script.call(
            "getAttachment",
            r#"{"messageId":"m1","attachmentId":"att1"}"#
        )));

        // The same garble inside reply_context fails draft replies too.
        let mut script = Script::new().will_answer(ok(&json!({"bogus": true})));
        assert!(is_tool_error(
            &script.call("saveDraft", r#"{"inReplyToMessageId":"m1"}"#)
        ));
    }

    #[test]
    fn rejects_invalid_attachment_base64() {
        let bad_attachment =
            r#"[{"filename":"a.bin","mimeType":"application/octet-stream","contentBase64":"!!!"}]"#;

        // sendEmail builds the MIME before sending anything.
        let mut script = Script::new();
        let body = format!(
            r#"{{"to":["a@b.c"],"subject":"s","body":"b","attachments":{bad_attachment}}}"#
        );
        let error = script.call("sendEmail", &body).unwrap_err();
        assert!(matches!(error, ConnectorError::InvalidInput(_))); // coverage: exclude — matches! false arm is unreachable in a passing test
        assert!(script.seen.is_empty());

        // replyToEmail reads the original first, then fails building the MIME.
        let mut script = Script::new().will_answer(ok(&metadata_fixture()));
        let body = format!(r#"{{"messageId":"m1","body":"b","attachments":{bad_attachment}}}"#);
        let error = script.call("replyToEmail", &body).unwrap_err();
        assert!(matches!(error, ConnectorError::InvalidInput(_))); // coverage: exclude — matches! false arm is unreachable in a passing test
        assert_eq!(script.seen.len(), 1);

        // saveDraft with no in-reply-to also builds before sending.
        let mut script = Script::new();
        let body = format!(r#"{{"body":"b","attachments":{bad_attachment}}}"#);
        let error = script.call("saveDraft", &body).unwrap_err();
        assert!(matches!(error, ConnectorError::InvalidInput(_))); // coverage: exclude — matches! false arm is unreachable in a passing test
        assert!(script.seen.is_empty());
    }

    #[test]
    fn rejects_unknown_tools() {
        let mut script = Script::new();
        assert_eq!(
            script.call("nope", "{}").unwrap_err(),
            ConnectorError::NotFound("unknown tool: nope".into())
        );
    }

    #[test]
    fn off_wasm_nothing_is_sent() {
        // Off-host every send fails, which surfaces as a tool error naming
        // the step — never as a half-built request escaping anywhere.
        let result = Gmail::call_tool(tool_request("searchEmails", r#"{"query":"x"}"#));
        assert!(is_tool_error(&result));
    }

    #[test]
    fn manifest_is_embedded() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["name"], "gmail");
        assert_eq!(manifest["version"], "0.2.0");
        assert_eq!(manifest["auth"]["schemes"][0]["type"], "oauth2");
        assert_eq!(
            manifest["auth"]["schemes"][0]["scopes"][0],
            "https://www.googleapis.com/auth/gmail.modify"
        );
        assert_eq!(
            manifest["permissions"]["http"]["allow"][0],
            DEFAULT_API_BASE
        );
        let tools = manifest["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 12);
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "searchEmails",
                "readThread",
                "sendEmail",
                "replyToEmail",
                "forwardEmail",
                "saveDraft",
                "sendDraft",
                "discardDraft",
                "organizeEmail",
                "getAttachment",
                "manageLabels",
                "getProfile",
            ]
        );
    }
}
