//! Outlook Mail (Microsoft 365), redesigned around what agents actually do with mail.
//!
//! Instead of mirroring the Microsoft Graph mail endpoints one-for-one, this
//! connector offers eleven task-shaped tools: each one completes a job —
//! finding mail by browsing a folder or searching with full bodies, reading a
//! message, sending, replying, forwarding, drafting, organizing, fetching an
//! attachment, listing the reachable mailboxes — in a single call, making
//! whatever HTTP requests the job needs internally.
//!
//! The host runs the OAuth 2.0 sign-in, refreshes the token and adds
//! `Authorization: Bearer …` after each request leaves the guest: this
//! code never sees it.
//!
//! Every mail tool takes an optional `mailbox` (default `"me"`): the
//! signed-in user's mailbox, or the address of a shared or delegated mailbox
//! the user can access. Only the signed-in mailbox is discoverable with the
//! granted scopes; other mailboxes are passed explicitly.

use base64::{
    DecodeError, Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use connectors_sdk::http::{Method, Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::str::FromStr;

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://graph.microsoft.com";

/// The default and maximum number of messages `searchMessages` returns.
const DEFAULT_SEARCH_RESULTS: u32 = 10;
const MAX_SEARCH_RESULTS: u32 = 50;

/// How many messages `organizeMessage` resolves when given a bulk `query`.
/// Graph `$search` returns at most 250 results.
const BULK_LIST_LIMIT: u32 = 250;

/// Message fields for full reads: headers, metadata and the body.
const FULL_SELECT: &str = "id,subject,from,toRecipients,ccRecipients,receivedDateTime,bodyPreview,body,isRead,hasAttachments,parentFolderId,conversationId,importance";

/// The connector.
#[derive(Debug)]
pub struct OutlookMail;

impl Connector for OutlookMail {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        Self::call_with(request, |req| Ok(req.clone().send()?))
    }
}

impl OutlookMail {
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
            "searchMessages" => search_messages(root, &request.json()?, &mut send),
            "getMessage" => get_message(root, &request.json()?, &mut send),
            "sendMessage" => send_message(root, &request.json()?, &mut send),
            "replyToMessage" => reply_to_message(root, &request.json()?, &mut send),
            "forwardMessage" => forward_message(root, &request.json()?, &mut send),
            "saveDraft" => save_draft(root, &request.json()?, &mut send),
            "sendDraft" => send_draft(root, &request.json()?, &mut send),
            "discardDraft" => discard_draft(root, &request.json()?, &mut send),
            "organizeMessage" => organize_message(root, &request.json()?, &mut send),
            "getAttachment" => get_attachment(root, &request.json()?, &mut send),
            "listMailAccounts" => list_mail_accounts(root, &request.json()?, &mut send),
            other => return Err(ConnectorError::NotFound(format!("unknown tool: {other}"))),
        };
        match payload {
            Ok(json) => ToolResponse::structured(&json),
            Err(ToolError::Input(error)) => Err(error),
            Err(ToolError::Step(failed)) => Ok(failed),
        }
    }
}

connectors_sdk::export_connector!(OutlookMail);

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

/// What `organizeMessage` should do, parsed from the `action` string at the
/// tool boundary so the rest of the code matches on a real enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrganizeAction {
    MarkRead,
    MarkUnread,
    Archive,
    Delete,
    Move,
}

impl FromStr for OrganizeAction {
    type Err = ConnectorError;

    fn from_str(action: &str) -> Result<Self, ConnectorError> {
        match action.to_ascii_lowercase().as_str() {
            "markread" => Ok(Self::MarkRead),
            "markunread" => Ok(Self::MarkUnread),
            "archive" => Ok(Self::Archive),
            "delete" => Ok(Self::Delete),
            "move" => Ok(Self::Move),
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
            Self::MarkRead => "markRead",
            Self::MarkUnread => "markUnread",
            Self::Archive => "archive",
            Self::Delete => "delete",
            Self::Move => "move",
        }
    }

    /// The per-message request for one message id. `Delete` is the Graph
    /// DELETE, which moves the message to Deleted Items — recoverable, never
    /// a permanent delete.
    fn request_for(
        self,
        url: String,
        folder_id: Option<&str>,
    ) -> Result<(Request, &'static str), ConnectorError> {
        match self {
            Self::MarkRead => Ok((
                patch_json(url, &json!({ "isRead": true })),
                "marking the message read",
            )),
            Self::MarkUnread => Ok((
                patch_json(url, &json!({ "isRead": false })),
                "marking the message unread",
            )),
            Self::Archive => Ok((
                post_json(url + "/move", &json!({ "destinationId": "archive" })),
                "archiving the message",
            )),
            Self::Delete => Ok((delete(url), "deleting the message")),
            Self::Move => {
                let folder = folder_id
                    .ok_or_else(|| ConnectorError::InvalidInput("move needs folderId".into()))?;
                Ok((
                    post_json(url + "/move", &json!({ "destinationId": folder })),
                    "moving the message",
                ))
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchMessages {
    mailbox: Option<String>,
    query: Option<String>,
    folder: Option<String>,
    unread_only: Option<bool>,
    max_results: Option<u32>,
    skip: Option<u32>,
    include_html: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetMessage {
    mailbox: Option<String>,
    message_id: String,
    include_html: Option<bool>,
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
struct SendMessage {
    mailbox: Option<String>,
    to: Vec<String>,
    cc: Option<Vec<String>>,
    bcc: Option<Vec<String>>,
    subject: String,
    body: String,
    body_type: Option<String>,
    attachments: Option<Vec<AttachmentInput>>,
    save_to_sent_items: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReplyToMessage {
    mailbox: Option<String>,
    message_id: String,
    body: String,
    reply_all: Option<bool>,
    cc: Option<Vec<String>>,
    bcc: Option<Vec<String>>,
    attachments: Option<Vec<AttachmentInput>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForwardMessage {
    mailbox: Option<String>,
    message_id: String,
    to: Vec<String>,
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveDraft {
    mailbox: Option<String>,
    draft_id: Option<String>,
    to: Option<Vec<String>>,
    cc: Option<Vec<String>>,
    bcc: Option<Vec<String>>,
    subject: Option<String>,
    body: Option<String>,
    body_type: Option<String>,
    attachments: Option<Vec<AttachmentInput>>,
    in_reply_to_message_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendDraft {
    mailbox: Option<String>,
    draft_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiscardDraft {
    mailbox: Option<String>,
    draft_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrganizeMessage {
    mailbox: Option<String>,
    action: String,
    message_ids: Option<Vec<String>>,
    query: Option<String>,
    folder_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetAttachment {
    mailbox: Option<String>,
    message_id: String,
    attachment_id: Option<String>,
    filename: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListMailAccounts {}

/// One Graph email address.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiEmailAddress {
    #[serde(default)]
    name: String,
    #[serde(default)]
    address: String,
}

/// One Graph recipient: an `emailAddress` wrapper.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiRecipient {
    #[serde(default)]
    email_address: ApiEmailAddress,
}

/// A Graph message body.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiBody {
    #[serde(default)]
    content_type: String,
    #[serde(default)]
    content: String,
}

/// A Graph message as the API returns it.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiMessage {
    #[serde(default)]
    id: String,
    #[serde(default)]
    subject: String,
    #[serde(default)]
    from: ApiRecipient,
    #[serde(default)]
    to_recipients: Vec<ApiRecipient>,
    #[serde(default)]
    cc_recipients: Vec<ApiRecipient>,
    #[serde(default)]
    received_date_time: String,
    #[serde(default)]
    body_preview: String,
    #[serde(default)]
    body: ApiBody,
    #[serde(default)]
    is_read: bool,
    #[serde(default)]
    has_attachments: bool,
    #[serde(default)]
    parent_folder_id: String,
    #[serde(default)]
    conversation_id: String,
    #[serde(default)]
    importance: String,
}

/// A Graph attachment as the API returns it. Only file attachments carry
/// `contentBytes`; item and reference attachments do not.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiAttachment {
    #[serde(default)]
    name: String,
    #[serde(default)]
    content_type: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    content_bytes: Option<String>,
}

/// The agent-facing view of one address.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AddressView {
    name: String,
    address: String,
}

/// The agent-facing view of a message body.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BodyView {
    content_type: String,
    text: String,
}

/// The agent-facing view of one message: headers, body and metadata, shaped
/// out of the Graph message. Raw Graph JSON never leaves the connector.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MessageView {
    id: String,
    subject: String,
    from: AddressView,
    to: Vec<AddressView>,
    cc: Vec<AddressView>,
    received_date_time: String,
    body_preview: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<BodyView>,
    is_read: bool,
    has_attachments: bool,
    folder_id: String,
    conversation_id: String,
    importance: String,
}

/// The mailbox a request acts on: `mailbox`, or "me".
fn mailbox_name(mailbox: &Option<String>) -> &str {
    mailbox.as_deref().unwrap_or("me")
}

/// `{root}/v1.0/me/{path}`, or `{root}/v1.0/users/{mailbox}/{path}` for an
/// explicit shared or delegated mailbox.
fn mailbox_url(root: &str, mailbox: &Option<String>, path: &str) -> String {
    match mailbox_name(mailbox) {
        "me" => format!("{root}/v1.0/me/{path}"),
        other => format!("{root}/v1.0/users/{}/{path}", encode(other)),
    }
}

/// A GET request asking for JSON back.
fn get(url: String) -> Request {
    Request::get(url).header("accept", "application/json")
}

/// A GET request with extra headers, e.g. `Prefer` or `ConsistencyLevel`.
fn get_with(url: String, headers: &[(&str, &str)]) -> Request {
    let mut request = get(url);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request
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

fn patch_json(url: String, body: &Value) -> Request {
    json_request(Method::Patch, url, body)
}

fn delete(url: String) -> Request {
    Request::new(Method::Delete, url).header("accept", "application/json")
}

/// Runs one step of a multi-call tool: sends `request`, and turns the
/// outcome into the step's JSON or a tool error naming the step. A failed
/// upstream call is a tool result, not a connector bug. `empty` is answered
/// when the API returns 2xx with no body (202s and 204s).
fn step(
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
    request: &Request,
    name: &str,
    empty: &Value,
) -> Result<Value, ToolError> {
    let response = send(request)
        .map_err(|error| ToolError::Step(ToolResponse::error(format!("{name} failed: {error}"))))?;
    checked(&response, name, empty).map_err(ToolError::Step)
}

/// The JSON of a 2xx response, `empty` for an empty one, or a tool error
/// naming the failed step.
fn checked(response: &Response, step_name: &str, empty: &Value) -> Result<Value, ToolResponse> {
    if !(200..300).contains(&response.status) {
        let body = String::from_utf8_lossy(&response.body);
        return Err(ToolResponse::error(format!(
            "{step_name} failed: the API answered {}: {body}",
            response.status
        )));
    }
    if response.body.is_empty() {
        return Ok(empty.clone());
    }
    response
        .json()
        .map_err(|error| ToolResponse::error(format!("{step_name} failed: {error}")))
}

/// Parses one message response; a 2xx body that is not a message is a named
/// tool error, not a connector bug.
fn message_from(body: Value, step_name: &str) -> Result<ApiMessage, ToolError> {
    serde_json::from_value(body).map_err(|error| {
        ToolError::Step(ToolResponse::error(format!("{step_name} failed: {error}")))
    })
}

/// Parses one attachment response; a 2xx body that is not an attachment is
/// a named tool error, not a connector bug.
fn attachment_from(body: Value, step_name: &str) -> Result<ApiAttachment, ToolError> {
    serde_json::from_value(body).map_err(|error| {
        ToolError::Step(ToolResponse::error(format!("{step_name} failed: {error}")))
    })
}

/// The message ids in a message-list response (`value` array).
fn message_ids_in(list: &Value) -> Vec<String> {
    list.get("value")
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

/// Shapes the messages of a list response into agent-facing views.
fn messages_in(
    list: &Value,
    step_name: &str,
    include_body: bool,
) -> Result<Vec<MessageView>, ToolError> {
    let mut messages = Vec::new();
    if let Some(raw) = list.get("value").and_then(Value::as_array) {
        messages.reserve(raw.len());
        for (index, item) in raw.iter().enumerate() {
            let name = format!("reading message {index} of the {step_name}");
            messages.push(message_view(
                &message_from(item.clone(), &name)?,
                include_body,
            ));
        }
    }
    Ok(messages)
}

/// The agent-facing view of one address.
fn address_view(recipient: &ApiRecipient) -> AddressView {
    AddressView {
        name: recipient.email_address.name.clone(),
        address: recipient.email_address.address.clone(),
    }
}

/// The agent-facing view of one Graph message.
fn message_view(message: &ApiMessage, include_body: bool) -> MessageView {
    MessageView {
        id: message.id.clone(),
        subject: message.subject.clone(),
        from: address_view(&message.from),
        to: message.to_recipients.iter().map(address_view).collect(),
        cc: message.cc_recipients.iter().map(address_view).collect(),
        received_date_time: message.received_date_time.clone(),
        body_preview: message.body_preview.clone(),
        body: include_body.then(|| BodyView {
            content_type: message.body.content_type.clone(),
            text: message.body.content.clone(),
        }),
        is_read: message.is_read,
        has_attachments: message.has_attachments,
        folder_id: message.parent_folder_id.clone(),
        conversation_id: message.conversation_id.clone(),
        importance: message.importance.clone(),
    }
}

/// The `Prefer` header value asking Graph for the body as text or HTML.
fn body_prefer(include_html: bool) -> String {
    format!(
        "outlook.body-content-type=\"{}\"",
        if include_html { "html" } else { "text" }
    )
}

/// Escapes `"` and `\` inside a `$search` term, per the Graph docs.
fn escape_search(query: &str) -> String {
    let mut escaped = String::with_capacity(query.len());
    for ch in query.chars() {
        if ch == '"' || ch == '\\' {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// Accepts the standard base64 agents produce as well as URL-safe base64,
/// decoding to raw bytes.
fn decode_base64(encoded: &str) -> Result<Vec<u8>, DecodeError> {
    STANDARD
        .decode(encoded)
        .or_else(|_| URL_SAFE_NO_PAD.decode(encoded))
}

/// Normalizes agent-supplied base64 to the standard alphabet Graph expects.
fn normalized_base64(encoded: &str) -> Result<String, ConnectorError> {
    decode_base64(encoded)
        .map(|bytes| STANDARD.encode(bytes))
        .map_err(|error| {
            ConnectorError::InvalidInput(format!("contentBase64 is not valid base64: {error}"))
        })
}

/// `"alice@example.com"` to the Graph recipient object.
fn recipients(addresses: &[String]) -> Value {
    Value::Array(
        addresses
            .iter()
            .map(|address| json!({ "emailAddress": { "address": address } }))
            .collect(),
    )
}

/// The Graph `fileAttachment` objects for `sendMessage`, `replyToMessage`
/// and `saveDraft`.
fn file_attachments(attachments: &[AttachmentInput]) -> Result<Value, ConnectorError> {
    let mut bodies = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        bodies.push(json!({
            "@odata.type": "#microsoft.graph.fileAttachment",
            "name": attachment.filename,
            "contentType": attachment.mime_type,
            "contentBytes": normalized_base64(&attachment.content_base64)?,
        }));
    }
    Ok(Value::Array(bodies))
}

/// `text` or `html`, validated at the tool boundary.
fn body_type_or(body_type: &Option<String>) -> Result<&str, ConnectorError> {
    let body_type = body_type.as_deref().unwrap_or("text");
    match body_type {
        "text" | "html" => Ok(body_type),
        _ => Err(ConnectorError::InvalidInput(format!(
            "bodyType must be \"text\" or \"html\", not {body_type:?}"
        ))),
    }
}

/// The one mail-finding tool. With a query it searches — every folder, or
/// `folder` when the agent named one explicitly; without a query (or a blank
/// one) it browses `folder`, newest first. Both modes return full message
/// bodies, so there is never a list-then-get round trip.
fn search_messages(
    root: &str,
    input: &SearchMessages,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    let max = input
        .max_results
        .unwrap_or(DEFAULT_SEARCH_RESULTS)
        .clamp(1, MAX_SEARCH_RESULTS);
    let include_html = input.include_html.unwrap_or(false);
    let prefer = body_prefer(include_html);
    // A blank query is not a search: whitespace-only input browses.
    let query = input
        .query
        .as_deref()
        .map(str::trim)
        .filter(|query| !query.is_empty());
    // Browse mode lists `folder` (default inbox); search mode without an
    // explicit folder searches every folder.
    let folder: Option<String> = match (&input.folder, query.is_some()) {
        (Some(folder), _) => Some(folder.clone()),
        (None, true) => None,
        (None, false) => Some("inbox".to_string()),
    };
    let path = match &folder {
        Some(folder) => format!("mailFolders/{}/messages", encode(folder)),
        None => "messages".to_string(),
    };
    let mut headers: Vec<(&str, &str)> = vec![("Prefer", prefer.as_str())];
    let mut params: Vec<(&str, Option<String>)> = vec![
        ("$top", Some(max.to_string())),
        ("$skip", Some(input.skip.unwrap_or(0).to_string())),
        ("$select", Some(FULL_SELECT.to_string())),
    ];
    if let Some(query) = query {
        // `$search` needs the eventual-consistency header.
        headers.push(("ConsistencyLevel", "eventual"));
        params.push(("$search", Some(format!("\"{}\"", escape_search(query)))));
    }
    if input.unread_only.unwrap_or(false) {
        params.push(("$filter", Some("isRead eq false".to_string())));
    }
    let listed = step(
        send,
        &get_with(
            with_query(&mailbox_url(root, &input.mailbox, &path), &params),
            &headers,
        ),
        "searching messages",
        &json!({}),
    )?;
    let messages = messages_in(&listed, "searching messages", true)?;
    Ok(json!({ "messages": messages, "folder": folder }))
}

fn get_message(
    root: &str,
    input: &GetMessage,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    let include_html = input.include_html.unwrap_or(false);
    let prefer = body_prefer(include_html);
    let name = format!("reading message {}", input.message_id);
    let got = step(
        send,
        &get_with(
            with_query(
                &mailbox_url(
                    root,
                    &input.mailbox,
                    &format!("messages/{}", encode(&input.message_id)),
                ),
                &[("$select", Some(FULL_SELECT.to_string()))],
            ),
            &[("Prefer", prefer.as_str())],
        ),
        &name,
        &json!({}),
    )?;
    Ok(json!(message_view(&message_from(got, &name)?, true)))
}

fn send_message(
    root: &str,
    input: &SendMessage,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    if input.to.is_empty()
        && input.cc.as_ref().is_none_or(Vec::is_empty)
        && input.bcc.as_ref().is_none_or(Vec::is_empty)
    {
        return Err(ToolError::Input(ConnectorError::InvalidInput(
            "sendMessage needs at least one recipient in to, cc, or bcc".into(),
        )));
    }
    let body_type = body_type_or(&input.body_type)?;
    let attachments = match &input.attachments {
        Some(list) => Some(file_attachments(list)?),
        None => None,
    };
    let message = object(&[
        ("subject", Some(json!(input.subject))),
        (
            "body",
            Some(json!({ "contentType": body_type, "content": input.body })),
        ),
        ("toRecipients", Some(recipients(&input.to))),
        ("ccRecipients", input.cc.as_ref().map(|cc| recipients(cc))),
        (
            "bccRecipients",
            input.bcc.as_ref().map(|bcc| recipients(bcc)),
        ),
        ("attachments", attachments),
    ]);
    step(
        send,
        &post_json(
            mailbox_url(root, &input.mailbox, "sendMail"),
            &json!({
                "message": message,
                "saveToSentItems": input.save_to_sent_items.unwrap_or(true),
            }),
        ),
        "sending the message",
        &json!({ "sent": true }),
    )
}

fn reply_to_message(
    root: &str,
    input: &ReplyToMessage,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    let action = if input.reply_all.unwrap_or(false) {
        "replyAll"
    } else {
        "reply"
    };
    let attachments = match &input.attachments {
        Some(list) => Some(file_attachments(list)?),
        None => None,
    };
    let overrides = object(&[
        ("ccRecipients", input.cc.as_ref().map(|cc| recipients(cc))),
        (
            "bccRecipients",
            input.bcc.as_ref().map(|bcc| recipients(bcc)),
        ),
        ("attachments", attachments),
    ]);
    let mut params = object(&[("comment", Some(json!(input.body)))]);
    if !overrides.as_object().is_some_and(serde_json::Map::is_empty) {
        params["message"] = overrides;
    }
    step(
        send,
        &post_json(
            mailbox_url(
                root,
                &input.mailbox,
                &format!("messages/{}/{action}", encode(&input.message_id)),
            ),
            &params,
        ),
        "replying to the message",
        &json!({ "replied": true }),
    )
}

fn forward_message(
    root: &str,
    input: &ForwardMessage,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    let params = object(&[
        ("toRecipients", Some(recipients(&input.to))),
        ("comment", input.body.as_ref().map(|body| json!(body))),
    ]);
    step(
        send,
        &post_json(
            mailbox_url(
                root,
                &input.mailbox,
                &format!("messages/{}/forward", encode(&input.message_id)),
            ),
            &params,
        ),
        "forwarding the message",
        &json!({ "forwarded": true }),
    )
}

/// The create/update body for `saveDraft`: only the fields the agent set.
fn draft_body(input: &SaveDraft) -> Result<Value, ConnectorError> {
    let body_type = body_type_or(&input.body_type)?;
    let attachments = match &input.attachments {
        Some(list) => Some(file_attachments(list)?),
        None => None,
    };
    Ok(object(&[
        (
            "subject",
            input.subject.as_ref().map(|subject| json!(subject)),
        ),
        (
            "body",
            input
                .body
                .as_ref()
                .map(|body| json!({ "contentType": body_type, "content": body })),
        ),
        ("toRecipients", input.to.as_ref().map(|to| recipients(to))),
        ("ccRecipients", input.cc.as_ref().map(|cc| recipients(cc))),
        (
            "bccRecipients",
            input.bcc.as_ref().map(|bcc| recipients(bcc)),
        ),
        ("attachments", attachments),
    ]))
}

fn save_draft(
    root: &str,
    input: &SaveDraft,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    if let Some(original_id) = &input.in_reply_to_message_id {
        let name = format!("creating the reply draft for message {original_id}");
        let created = step(
            send,
            &post_json(
                mailbox_url(
                    root,
                    &input.mailbox,
                    &format!("messages/{}/createReply", encode(original_id)),
                ),
                &json!({}),
            ),
            &name,
            &json!({}),
        )?;
        let draft = message_from(created, &name)?;
        let patch = draft_body(input)?;
        let updated = step(
            send,
            &patch_json(
                mailbox_url(
                    root,
                    &input.mailbox,
                    &format!("messages/{}", encode(&draft.id)),
                ),
                &patch,
            ),
            "updating the draft",
            &json!({}),
        )?;
        let updated = message_from(updated, "updating the draft")?;
        return Ok(json!({ "draftId": updated.id, "messageId": updated.id }));
    }
    let body = draft_body(input)?;
    let (request, step_name) = match &input.draft_id {
        Some(id) => (
            patch_json(
                mailbox_url(root, &input.mailbox, &format!("messages/{}", encode(id))),
                &body,
            ),
            "updating the draft",
        ),
        None => (
            post_json(mailbox_url(root, &input.mailbox, "messages"), &body),
            "creating the draft",
        ),
    };
    let saved = step(send, &request, step_name, &json!({}))?;
    let draft = message_from(saved, step_name)?;
    Ok(json!({ "draftId": draft.id, "messageId": draft.id }))
}

fn send_draft(
    root: &str,
    input: &SendDraft,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    step(
        send,
        &post_json(
            mailbox_url(
                root,
                &input.mailbox,
                &format!("messages/{}/send", encode(&input.draft_id)),
            ),
            &json!({}),
        ),
        "sending the draft",
        &json!({ "sent": true }),
    )
}

fn discard_draft(
    root: &str,
    input: &DiscardDraft,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    step(
        send,
        &delete(mailbox_url(
            root,
            &input.mailbox,
            &format!("messages/{}", encode(&input.draft_id)),
        )),
        "discarding the draft",
        &json!({ "deleted": true }),
    )
}

fn organize_message(
    root: &str,
    input: &OrganizeMessage,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    let action: OrganizeAction = input.action.parse()?;
    if input.message_ids.is_none() && input.query.is_none() {
        return Err(ToolError::Input(ConnectorError::InvalidInput(
            "organizeMessage needs messageIds or query".into(),
        )));
    }
    let mut ids = input.message_ids.clone().unwrap_or_default();
    if let Some(query) = &input.query {
        let prefer = body_prefer(false);
        let headers = [
            ("ConsistencyLevel", "eventual"),
            ("Prefer", prefer.as_str()),
        ];
        let listed = step(
            send,
            &get_with(
                with_query(
                    &mailbox_url(root, &input.mailbox, "messages"),
                    &[
                        ("$search", Some(format!("\"{}\"", escape_search(query)))),
                        ("$top", Some(BULK_LIST_LIMIT.to_string())),
                        ("$select", Some("id".to_string())),
                    ],
                ),
                &headers,
            ),
            "listing messages for the query",
            &json!({}),
        )?;
        for id in message_ids_in(&listed) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    let mut affected: u64 = 0;
    for id in &ids {
        let url = mailbox_url(root, &input.mailbox, &format!("messages/{}", encode(id)));
        let (request, step_name) = action.request_for(url, input.folder_id.as_deref())?;
        step(send, &request, step_name, &json!({}))?;
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
    let base = mailbox_url(
        root,
        &input.mailbox,
        &format!("messages/{}", encode(&input.message_id)),
    );
    let attachment_id = match &input.attachment_id {
        Some(id) => id.clone(),
        None => {
            let name = format!("listing attachments of message {}", input.message_id);
            let listed = step(
                send,
                &get(with_query(
                    &format!("{base}/attachments"),
                    &[("$select", Some("id,name,contentType,size".to_string()))],
                )),
                &name,
                &json!({}),
            )?;
            listed
                .get("value")
                .and_then(Value::as_array)
                .and_then(|all| {
                    all.iter().find(|attachment| {
                        attachment.get("name").and_then(Value::as_str) == input.filename.as_deref()
                    })
                })
                .and_then(|attachment| attachment.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| {
                    ToolError::Step(ToolResponse::error(format!(
                        "{name} failed: no attachment matched"
                    )))
                })?
        }
    };
    let name = format!("reading attachment {attachment_id}");
    let data = step(
        send,
        &get(format!("{base}/attachments/{}", encode(&attachment_id))),
        &name,
        &json!({}),
    )?;
    let attachment = attachment_from(data, &name)?;
    let encoded = attachment.content_bytes.ok_or_else(|| {
        ToolError::Step(ToolResponse::error(format!(
            "{name} failed: the attachment has no downloadable content"
        )))
    })?;
    let bytes = decode_base64(&encoded).map_err(|error| {
        ToolError::Step(ToolResponse::error(format!(
            "{name} failed: the attachment content is not valid base64: {error}"
        )))
    })?;
    Ok(json!({
        "filename": attachment.name,
        "mimeType": attachment.content_type,
        "size": attachment.size,
        "contentBase64": STANDARD.encode(&bytes),
    }))
}

fn list_mail_accounts(
    root: &str,
    _input: &ListMailAccounts,
    send: &mut dyn FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<Value, ToolError> {
    let me = step(
        send,
        &get(with_query(
            &format!("{root}/v1.0/me"),
            &[(
                "$select",
                Some("displayName,mail,userPrincipalName".to_string()),
            )],
        )),
        "reading the account",
        &json!({}),
    )?;
    let email = me
        .get("mail")
        .and_then(Value::as_str)
        .filter(|mail| !mail.is_empty())
        .or_else(|| me.get("userPrincipalName").and_then(Value::as_str))
        .unwrap_or("");
    Ok(json!({
        "accounts": [{
            "mailbox": "me",
            "displayName": me.get("displayName").and_then(Value::as_str).unwrap_or(""),
            "emailAddress": email,
        }],
        "note": "Only the signed-in mailbox can be discovered with these scopes. Pass the address of any other mailbox you know (shared with you or delegated to you) as the mailbox parameter on the mail tools.",
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
            ..Default::default()
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

    /// A recorded outbound request: method, URL, headers, body.
    type SeenRequest = (Method, String, Vec<(String, String)>, Vec<u8>);

    /// A scripted sender: plays back queued responses and records the
    /// requests it was asked to send.
    struct Script {
        responses: VecDeque<Result<Response, ConnectorError>>,
        seen: Vec<SeenRequest>,
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
                self.seen.push((
                    request.method,
                    request.url.clone(),
                    request.headers.clone(),
                    request.body.clone(),
                ));
                self.responses
                    .pop_front()
                    .unwrap_or(Err(ConnectorError::Upstream(
                        "the script ran out of responses".into(),
                    )))
            }
        }

        fn call(&mut self, tool: &str, body: &str) -> Result<ToolResponse, ConnectorError> {
            OutlookMail::call_with(tool_request(tool, body), self.sender())
        }

        fn seen_url(&self, index: usize) -> &str {
            &self.seen[index].1
        }

        fn seen_method(&self, index: usize) -> Method {
            self.seen[index].0
        }

        fn seen_header(&self, index: usize, name: &str) -> Option<&str> {
            self.seen[index]
                .2
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        }

        fn seen_json(&self, index: usize) -> Value {
            serde_json::from_slice(&self.seen[index].3).unwrap()
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

    /// A full Graph message, built with `json!` so the fixture is obviously
    /// well-formed.
    fn message_fixture(
        id: &str,
        subject: &str,
        from_name: &str,
        from_address: &str,
        text: &str,
    ) -> Value {
        json!({
            "id": id,
            "subject": subject,
            "from": { "emailAddress": { "name": from_name, "address": from_address } },
            "toRecipients": [
                { "emailAddress": { "name": "Me", "address": "me@contoso.com" } },
                { "emailAddress": { "name": "", "address": "other@contoso.com" } },
            ],
            "ccRecipients": [
                { "emailAddress": { "name": "Carol", "address": "carol@contoso.com" } },
            ],
            "receivedDateTime": "2026-10-10T10:00:00Z",
            "bodyPreview": format!("{text}..."),
            "body": { "contentType": "text", "content": text },
            "isRead": false,
            "hasAttachments": true,
            "parentFolderId": "inbox-id",
            "conversationId": "conv1",
            "importance": "normal",
        })
    }

    /// A message carrying only an id, to exercise every default.
    fn minimal_message_fixture() -> Value {
        json!({ "id": "m9" })
    }

    fn attachment_input() -> AttachmentInput {
        AttachmentInput {
            filename: "notes.txt".to_string(),
            mime_type: "text/plain".to_string(),
            content_base64: STANDARD.encode("hello"),
        }
    }

    #[test]
    fn mailbox_defaults_to_me() {
        assert_eq!(mailbox_name(&None), "me");
        assert_eq!(mailbox_name(&Some("me".to_string())), "me");
        assert_eq!(
            mailbox_name(&Some("shared@contoso.com".to_string())),
            "shared@contoso.com"
        );
    }

    #[test]
    fn builds_mailbox_urls() {
        assert_eq!(
            mailbox_url("https://graph.microsoft.com", &None, "messages"),
            "https://graph.microsoft.com/v1.0/me/messages"
        );
        assert_eq!(
            mailbox_url(
                "https://graph.microsoft.com",
                &Some("me".to_string()),
                "messages/m1"
            ),
            "https://graph.microsoft.com/v1.0/me/messages/m1"
        );
        assert_eq!(
            mailbox_url(
                "https://graph.microsoft.com",
                &Some("shared@contoso.com".to_string()),
                "messages"
            ),
            "https://graph.microsoft.com/v1.0/users/shared%40contoso.com/messages"
        );
    }

    #[test]
    fn encodes_query_and_paths() {
        assert_eq!(encode("inbox"), "inbox");
        assert_eq!(encode("a b@c="), "a%20b%40c%3D");
        assert_eq!(with_query("https://x.test/y", &[]), "https://x.test/y");
        assert_eq!(
            with_query(
                "https://x.test/y",
                &[
                    ("$top", Some("10".to_string())),
                    ("$skip", None),
                    ("q", Some("a b".to_string())),
                ]
            ),
            "https://x.test/y?$top=10&q=a%20b"
        );
    }

    #[test]
    fn builds_objects_from_present_fields() {
        assert_eq!(
            object(&[("a", Some(json!(1))), ("b", None), ("c", Some(json!("x"))),]),
            json!({ "a": 1, "c": "x" })
        );
    }

    #[test]
    fn escapes_search_terms() {
        assert_eq!(escape_search("plain"), "plain");
        assert_eq!(escape_search("say \"hi\""), "say \\\"hi\\\"");
        assert_eq!(escape_search("a\\b"), "a\\\\b");
    }

    #[test]
    fn prefers_text_or_html_bodies() {
        assert_eq!(body_prefer(false), "outlook.body-content-type=\"text\"");
        assert_eq!(body_prefer(true), "outlook.body-content-type=\"html\"");
    }

    #[test]
    fn normalizes_attachment_base64() {
        assert_eq!(
            normalized_base64(&STANDARD.encode("hello")).unwrap(),
            STANDARD.encode("hello")
        );
        assert_eq!(
            normalized_base64(&URL_SAFE_NO_PAD.encode("hello?")).unwrap(),
            STANDARD.encode("hello?")
        );
        assert!(normalized_base64("!!! not base64 !!!").is_err());
    }

    #[test]
    fn builds_file_attachment_bodies() {
        let bodies = file_attachments(&[attachment_input()]).unwrap();
        assert_eq!(
            bodies,
            json!([{
                "@odata.type": "#microsoft.graph.fileAttachment",
                "name": "notes.txt",
                "contentType": "text/plain",
                "contentBytes": STANDARD.encode("hello"),
            }])
        );
        let bad = AttachmentInput {
            content_base64: "!!!".to_string(),
            ..attachment_input()
        };
        assert!(file_attachments(&[bad]).is_err());
    }

    #[test]
    fn builds_recipients() {
        assert_eq!(
            recipients(&["a@x.test".to_string(), "b@x.test".to_string()]),
            json!([
                { "emailAddress": { "address": "a@x.test" } },
                { "emailAddress": { "address": "b@x.test" } },
            ])
        );
        assert_eq!(recipients(&[]), json!([]));
    }

    #[test]
    fn validates_body_types() {
        assert_eq!(body_type_or(&None).unwrap(), "text");
        assert_eq!(body_type_or(&Some("html".to_string())).unwrap(), "html");
        assert!(body_type_or(&Some("markdown".to_string())).is_err());
    }

    #[test]
    fn shapes_full_messages() {
        let message: ApiMessage = serde_json::from_value(message_fixture(
            "m1",
            "Hello",
            "Alice",
            "alice@x.test",
            "Hi",
        ))
        .unwrap();
        let view = message_view(&message, true);
        assert_eq!(view.id, "m1");
        assert_eq!(view.subject, "Hello");
        assert_eq!(view.from.address, "alice@x.test");
        assert_eq!(view.from.name, "Alice");
        assert_eq!(view.to.len(), 2);
        assert_eq!(view.cc.len(), 1);
        assert_eq!(view.received_date_time, "2026-10-10T10:00:00Z");
        assert_eq!(view.body_preview, "Hi...");
        let body = view.body.as_ref().unwrap();
        assert_eq!(body.content_type, "text");
        assert_eq!(body.text, "Hi");
        assert!(!view.is_read);
        assert!(view.has_attachments);
        assert_eq!(view.folder_id, "inbox-id");
        assert_eq!(view.conversation_id, "conv1");
        assert_eq!(view.importance, "normal");
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["receivedDateTime"], "2026-10-10T10:00:00Z");
        assert_eq!(json["isRead"], false);
        assert_eq!(json["folderId"], "inbox-id");
    }

    #[test]
    fn shapes_compact_messages_without_bodies() {
        let message: ApiMessage = serde_json::from_value(message_fixture(
            "m1",
            "Hello",
            "Alice",
            "alice@x.test",
            "Hi",
        ))
        .unwrap();
        let view = message_view(&message, false);
        assert!(view.body.is_none());
        let json = serde_json::to_value(&view).unwrap();
        assert!(json.get("body").is_none());
    }

    #[test]
    fn shapes_minimal_messages() {
        let message: ApiMessage = serde_json::from_value(minimal_message_fixture()).unwrap();
        let view = message_view(&message, true);
        assert_eq!(view.id, "m9");
        assert_eq!(view.subject, "");
        assert_eq!(view.from.address, "");
        assert!(view.to.is_empty());
        assert!(view.body.unwrap().text.is_empty());
    }

    #[test]
    fn parses_organize_actions() {
        assert_eq!("markread".parse(), Ok(OrganizeAction::MarkRead));
        assert_eq!("MarkUnread".parse(), Ok(OrganizeAction::MarkUnread));
        assert_eq!("ARCHIVE".parse(), Ok(OrganizeAction::Archive));
        assert_eq!("delete".parse(), Ok(OrganizeAction::Delete));
        assert_eq!("move".parse(), Ok(OrganizeAction::Move));
        assert!("explode".parse::<OrganizeAction>().is_err());
    }

    #[test]
    fn names_organize_actions() {
        assert_eq!(OrganizeAction::MarkRead.name(), "markRead");
        assert_eq!(OrganizeAction::MarkUnread.name(), "markUnread");
        assert_eq!(OrganizeAction::Archive.name(), "archive");
        assert_eq!(OrganizeAction::Delete.name(), "delete");
        assert_eq!(OrganizeAction::Move.name(), "move");
    }

    #[test]
    fn builds_organize_requests() {
        let url = "https://graph.microsoft.com/v1.0/me/messages/m1".to_string();
        let (request, step) = OrganizeAction::MarkRead
            .request_for(url.clone(), None)
            .unwrap();
        assert_eq!(request.method, Method::Patch);
        assert_eq!(step, "marking the message read");
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).unwrap(),
            json!({ "isRead": true })
        );

        let (request, step) = OrganizeAction::MarkUnread
            .request_for(url.clone(), None)
            .unwrap();
        assert_eq!(request.method, Method::Patch);
        assert_eq!(step, "marking the message unread");

        let (request, step) = OrganizeAction::Archive
            .request_for(url.clone(), None)
            .unwrap();
        assert_eq!(request.method, Method::Post);
        assert!(request.url.ends_with("/move"));
        assert_eq!(step, "archiving the message");
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).unwrap(),
            json!({ "destinationId": "archive" })
        );

        let (request, step) = OrganizeAction::Delete
            .request_for(url.clone(), None)
            .unwrap();
        assert_eq!(request.method, Method::Delete);
        assert_eq!(step, "deleting the message");

        let (request, step) = OrganizeAction::Move
            .request_for(url.clone(), Some("folder1"))
            .unwrap();
        assert_eq!(request.method, Method::Post);
        assert_eq!(step, "moving the message");
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).unwrap(),
            json!({ "destinationId": "folder1" })
        );

        assert!(OrganizeAction::Move.request_for(url, None).is_err());
    }

    #[test]
    fn converts_connector_errors_to_tool_input_errors() {
        let error: ToolError = ConnectorError::InvalidInput("bad".into()).into();
        assert!(matches!(error, ToolError::Input(_))); // coverage: exclude — matches! false arm is unreachable in a passing test
    }

    #[test]
    fn search_messages_browses_a_folder_by_default() {
        // No query: browse mode lists the inbox, newest first, with full
        // bodies — no list-then-get round trip.
        let mut script = Script::new().will_answer(ok(&json!({
            "value": [
                message_fixture("m1", "Hello", "Alice", "alice@x.test", "Hi"),
                message_fixture("m2", "World", "Bob", "bob@x.test", "Yo"),
            ],
        })));
        let result = script.call("searchMessages", "{}");
        let payload = structured(&result);
        assert_eq!(payload["folder"], "inbox");
        assert_eq!(payload["messages"].as_array().unwrap().len(), 2);
        assert_eq!(payload["messages"][0]["subject"], "Hello");
        assert_eq!(payload["messages"][0]["body"]["text"], "Hi");
        assert_eq!(script.seen_method(0), Method::Get);
        let url = script.seen_url(0);
        assert!(
            url.contains("/v1.0/me/mailFolders/inbox/messages?"),
            "{url}"
        );
        assert!(url.contains("$top=10"), "{url}");
        assert!(url.contains("$select="), "{url}");
        assert!(url.contains("%2Cbody%2C"), "{url}");
        assert!(!url.contains("$search"), "{url}");
        assert!(!url.contains("$filter"), "{url}");
        assert_eq!(script.seen_header(0, "ConsistencyLevel"), None);
        assert_eq!(
            script.seen_header(0, "Prefer"),
            Some("outlook.body-content-type=\"text\"")
        );
    }

    #[test]
    fn search_messages_browses_with_unread_only_and_paging() {
        let mut script = Script::new().will_answer(ok(&json!({})));
        let result = script.call(
            "searchMessages",
            r#"{"folder":"archive","maxResults":100,"skip":25,"unreadOnly":true,"mailbox":"shared@contoso.com"}"#,
        );
        let payload = structured(&result);
        assert!(payload["messages"].as_array().unwrap().is_empty());
        assert_eq!(payload["folder"], "archive");
        let url = script.seen_url(0);
        assert!(
            url.contains("/v1.0/users/shared%40contoso.com/mailFolders/archive/messages?"),
            "{url}"
        );
        assert!(url.contains("$top=50"), "{url}");
        assert!(url.contains("$skip=25"), "{url}");
        assert!(url.contains("$filter=isRead%20eq%20false"), "{url}");
        assert_eq!(script.seen_header(0, "ConsistencyLevel"), None);
    }

    #[test]
    fn search_messages_reports_failed_browse_steps() {
        let mut script = Script::new().will_answer(response(403, r#"{"error":"denied"}"#));
        let result = script.call("searchMessages", "{}");
        assert!(is_tool_error(&result));
    }

    #[test]
    fn searches_messages_end_to_end() {
        let mut script = Script::new().will_answer(ok(&json!({
            "value": [message_fixture("m1", "Invoice", "Alice", "alice@x.test", "Pay up")],
        })));
        let result = script.call("searchMessages", r#"{"query":"from:alice invoice"}"#);
        let payload = structured(&result);
        assert_eq!(payload["messages"].as_array().unwrap().len(), 1);
        assert_eq!(payload["messages"][0]["body"]["text"], "Pay up");
        // Searching without a folder spans every folder: `folder` is null.
        assert_eq!(payload["folder"], Value::Null);
        let url = script.seen_url(0);
        assert!(url.contains("/v1.0/me/messages?"), "{url}");
        assert!(
            url.contains("$search=%22from%3Aalice%20invoice%22"),
            "{url}"
        );
        assert_eq!(script.seen_header(0, "ConsistencyLevel"), Some("eventual"));
        assert_eq!(
            script.seen_header(0, "Prefer"),
            Some("outlook.body-content-type=\"text\"")
        );
    }

    #[test]
    fn search_messages_scopes_search_to_folder() {
        // An explicit folder with a query searches just that folder.
        let mut script = Script::new().will_answer(ok(&json!({
            "value": [message_fixture("m1", "Invoice", "Alice", "alice@x.test", "Pay up")],
        })));
        let result = script.call(
            "searchMessages",
            r#"{"query":"invoice","folder":"sentitems"}"#,
        );
        let payload = structured(&result);
        assert_eq!(payload["folder"], "sentitems");
        assert_eq!(payload["messages"][0]["body"]["text"], "Pay up");
        let url = script.seen_url(0);
        assert!(
            url.contains("/v1.0/me/mailFolders/sentitems/messages?"),
            "{url}"
        );
        assert!(url.contains("$search=%22invoice%22"), "{url}");
        assert_eq!(script.seen_header(0, "ConsistencyLevel"), Some("eventual"));
    }

    #[test]
    fn search_messages_treats_blank_query_as_browse() {
        // Whitespace-only input is not a search: it browses the folder.
        let mut script = Script::new().will_answer(ok(&json!({ "value": [] })));
        let result = script.call("searchMessages", r#"{"query":"   "}"#);
        assert_eq!(structured(&result)["folder"], "inbox");
        let url = script.seen_url(0);
        assert!(
            url.contains("/v1.0/me/mailFolders/inbox/messages?"),
            "{url}"
        );
        assert!(!url.contains("$search"), "{url}");
        assert_eq!(script.seen_header(0, "ConsistencyLevel"), None);
    }

    #[test]
    fn search_messages_applies_unread_only_to_search() {
        let mut script = Script::new().will_answer(ok(&json!({ "value": [] })));
        let _ = script.call("searchMessages", r#"{"query":"x","unreadOnly":true}"#);
        let url = script.seen_url(0);
        assert!(url.contains("$search=%22x%22"), "{url}");
        assert!(url.contains("$filter=isRead%20eq%20false"), "{url}");
        assert_eq!(script.seen_header(0, "ConsistencyLevel"), Some("eventual"));
    }

    #[test]
    fn search_messages_browses_with_html_bodies() {
        let mut script = Script::new().will_answer(ok(&json!({ "value": [] })));
        let _ = script.call("searchMessages", r#"{"includeHtml":true}"#);
        assert_eq!(
            script.seen_header(0, "Prefer"),
            Some("outlook.body-content-type=\"html\"")
        );
    }

    #[test]
    fn search_messages_defaults_and_caps_max_results() {
        let mut script = Script::new()
            .will_answer(ok(&json!({ "value": [] })))
            .will_answer(ok(&json!({ "value": [] })));
        let _ = script.call("searchMessages", r#"{"query":"x","includeHtml":true}"#);
        assert_eq!(
            script.seen_header(0, "Prefer"),
            Some("outlook.body-content-type=\"html\"")
        );
        let _ = script.call("searchMessages", r#"{"query":"x","maxResults":500}"#);
        let capped = script.seen_url(1);
        assert!(capped.contains("$top=50"), "{capped}");
    }

    #[test]
    fn search_messages_reports_failed_steps() {
        let mut script = Script::new().will_fail(ConnectorError::Upstream("boom".into()));
        let result = script.call("searchMessages", r#"{"query":"x"}"#);
        assert!(is_tool_error(&result));
    }

    #[test]
    fn gets_messages_end_to_end() {
        let mut script = Script::new().will_answer(ok(&message_fixture(
            "m1",
            "Hello",
            "Alice",
            "alice@x.test",
            "<b>Hi</b>",
        )));
        let result = script.call("getMessage", r#"{"messageId":"m1","includeHtml":true}"#);
        let payload = structured(&result);
        assert_eq!(payload["id"], "m1");
        assert_eq!(payload["body"]["text"], "<b>Hi</b>");
        let url = script.seen_url(0);
        assert!(url.contains("/v1.0/me/messages/m1?"), "{url}");
        assert_eq!(
            script.seen_header(0, "Prefer"),
            Some("outlook.body-content-type=\"html\"")
        );
    }

    #[test]
    fn get_message_reports_failed_steps() {
        let mut script = Script::new().will_answer(response(404, "not found"));
        let result = script.call("getMessage", r#"{"messageId":"m1"}"#);
        assert!(is_tool_error(&result));
    }

    #[test]
    fn sends_messages_end_to_end() {
        let mut script = Script::new().will_answer(response(202, ""));
        let result = script.call(
            "sendMessage",
            r#"{"to":["bob@x.test"],"cc":["carol@x.test"],"bcc":["dan@x.test"],"subject":"Hi","body":"Hello","attachments":[{"filename":"n.txt","mimeType":"text/plain","contentBase64":"aGk="}]}"#,
        );
        assert_eq!(structured(&result), json!({ "sent": true }));
        assert_eq!(script.seen_method(0), Method::Post);
        assert!(script.seen_url(0).ends_with("/v1.0/me/sendMail"));
        let body = script.seen_json(0);
        assert_eq!(body["saveToSentItems"], true);
        assert_eq!(body["message"]["subject"], "Hi");
        assert_eq!(
            body["message"]["body"],
            json!({ "contentType": "text", "content": "Hello" })
        );
        assert_eq!(
            body["message"]["toRecipients"],
            json!([{ "emailAddress": { "address": "bob@x.test" } }])
        );
        assert_eq!(body["message"]["attachments"][0]["name"], "n.txt");
    }

    #[test]
    fn send_message_needs_a_recipient() {
        let mut script = Script::new();
        let result = script.call("sendMessage", r#"{"to":[],"subject":"Hi","body":"Hello"}"#);
        assert_eq!(
            result.unwrap_err(),
            ConnectorError::InvalidInput(
                "sendMessage needs at least one recipient in to, cc, or bcc".into()
            )
        );
    }

    #[test]
    fn send_message_rejects_bad_body_type() {
        let mut script = Script::new();
        let result = script.call(
            "sendMessage",
            r#"{"to":["b@x.test"],"subject":"Hi","body":"Hello","bodyType":"markdown"}"#,
        );
        let error = result.unwrap_err();
        assert!(matches!(error, ConnectorError::InvalidInput(_))); // coverage: exclude — matches! false arm is unreachable in a passing test
    }

    #[test]
    fn send_message_rejects_bad_attachment_base64() {
        let mut script = Script::new();
        let result = script.call(
            "sendMessage",
            r#"{"to":["b@x.test"],"subject":"Hi","body":"Hello","attachments":[{"filename":"n.txt","mimeType":"text/plain","contentBase64":"!!!"}]}"#,
        );
        let error = result.unwrap_err();
        assert!(matches!(error, ConnectorError::InvalidInput(_))); // coverage: exclude — matches! false arm is unreachable in a passing test
    }

    #[test]
    fn send_message_sends_html_and_skips_sent_items() {
        let mut script = Script::new().will_answer(response(202, ""));
        let result = script.call(
            "sendMessage",
            r#"{"to":["b@x.test"],"subject":"Hi","body":"<b>Hello</b>","bodyType":"html","saveToSentItems":false}"#,
        );
        assert_eq!(structured(&result), json!({ "sent": true }));
        let body = script.seen_json(0);
        assert_eq!(body["saveToSentItems"], false);
        assert_eq!(
            body["message"]["body"],
            json!({ "contentType": "html", "content": "<b>Hello</b>" })
        );
    }

    #[test]
    fn replies_to_messages() {
        let mut script = Script::new().will_answer(response(202, ""));
        let result = script.call(
            "replyToMessage",
            r#"{"messageId":"m1","body":"Thanks!","cc":["carol@x.test"],"bcc":["dan@x.test"]}"#,
        );
        assert_eq!(structured(&result), json!({ "replied": true }));
        assert!(script.seen_url(0).ends_with("/v1.0/me/messages/m1/reply"));
        let body = script.seen_json(0);
        assert_eq!(body["comment"], "Thanks!");
        assert_eq!(
            body["message"]["ccRecipients"],
            json!([{ "emailAddress": { "address": "carol@x.test" } }])
        );
    }

    #[test]
    fn replies_all_without_overrides() {
        let mut script = Script::new().will_answer(response(202, ""));
        let result = script.call(
            "replyToMessage",
            r#"{"messageId":"m1","body":"Thanks all","replyAll":true}"#,
        );
        assert_eq!(structured(&result), json!({ "replied": true }));
        assert!(
            script
                .seen_url(0)
                .ends_with("/v1.0/me/messages/m1/replyAll")
        );
        assert!(script.seen_json(0).get("message").is_none());
    }

    #[test]
    fn reply_to_message_reports_failed_steps() {
        let mut script = Script::new().will_answer(response(400, "bad"));
        let result = script.call("replyToMessage", r#"{"messageId":"m1","body":"x"}"#);
        assert!(is_tool_error(&result));
    }

    #[test]
    fn forwards_messages() {
        let mut script = Script::new().will_answer(response(202, ""));
        let result = script.call(
            "forwardMessage",
            r#"{"messageId":"m1","to":["b@x.test"],"body":"FYI"}"#,
        );
        assert_eq!(structured(&result), json!({ "forwarded": true }));
        assert!(script.seen_url(0).ends_with("/v1.0/me/messages/m1/forward"));
        let body = script.seen_json(0);
        assert_eq!(body["comment"], "FYI");
        assert_eq!(
            body["toRecipients"],
            json!([{ "emailAddress": { "address": "b@x.test" } }])
        );
    }

    #[test]
    fn forwards_messages_without_a_note() {
        let mut script = Script::new().will_answer(response(202, ""));
        let _ = script.call("forwardMessage", r#"{"messageId":"m1","to":["b@x.test"]}"#);
        assert!(script.seen_json(0).get("comment").is_none());
    }

    #[test]
    fn saves_drafts() {
        let mut script = Script::new().will_answer(response(
            201,
            &message_fixture("d1", "Draft", "Me", "me@contoso.com", "").to_string(),
        ));
        let result = script.call(
            "saveDraft",
            r#"{"to":["b@x.test"],"cc":["c@x.test"],"bcc":["d@x.test"],"subject":"Hi","body":"Hello"}"#,
        );
        assert_eq!(
            structured(&result),
            json!({ "draftId": "d1", "messageId": "d1" })
        );
        assert_eq!(script.seen_method(0), Method::Post);
        assert!(script.seen_url(0).ends_with("/v1.0/me/messages"));
        let body = script.seen_json(0);
        assert_eq!(body["subject"], "Hi");
    }

    #[test]
    fn updates_drafts() {
        let mut script = Script::new().will_answer(response(
            200,
            &message_fixture("d1", "Draft", "Me", "me@contoso.com", "").to_string(),
        ));
        let result = script.call("saveDraft", r#"{"draftId":"d1","subject":"New"}"#);
        assert_eq!(
            structured(&result),
            json!({ "draftId": "d1", "messageId": "d1" })
        );
        assert_eq!(script.seen_method(0), Method::Patch);
        assert!(script.seen_url(0).ends_with("/v1.0/me/messages/d1"));
        let body = script.seen_json(0);
        assert_eq!(body, json!({ "subject": "New" }));
    }

    #[test]
    fn save_draft_threads_replies() {
        let mut script = Script::new()
            .will_answer(response(
                201,
                &message_fixture("d2", "Re: Hi", "Me", "me@contoso.com", "").to_string(),
            ))
            .will_answer(response(
                200,
                &message_fixture("d2", "Re: Hi", "Me", "me@contoso.com", "Done").to_string(),
            ));
        let result = script.call("saveDraft", r#"{"body":"Done","inReplyToMessageId":"m1"}"#);
        assert_eq!(
            structured(&result),
            json!({ "draftId": "d2", "messageId": "d2" })
        );
        assert_eq!(script.seen_method(0), Method::Post);
        assert!(
            script
                .seen_url(0)
                .ends_with("/v1.0/me/messages/m1/createReply")
        );
        assert_eq!(script.seen_method(1), Method::Patch);
        assert!(script.seen_url(1).ends_with("/v1.0/me/messages/d2"));
    }

    #[test]
    fn save_draft_rejects_bad_body_type() {
        let mut script = Script::new();
        let result = script.call("saveDraft", r#"{"body":"x","bodyType":"bogus"}"#);
        let error = result.unwrap_err();
        assert!(matches!(error, ConnectorError::InvalidInput(_))); // coverage: exclude — matches! false arm is unreachable in a passing test
    }

    #[test]
    fn sends_drafts() {
        let mut script = Script::new().will_answer(response(202, ""));
        let result = script.call("sendDraft", r#"{"draftId":"d1"}"#);
        assert_eq!(structured(&result), json!({ "sent": true }));
        assert!(script.seen_url(0).ends_with("/v1.0/me/messages/d1/send"));
    }

    #[test]
    fn discards_drafts() {
        let mut script = Script::new().will_answer(response(204, ""));
        let result = script.call("discardDraft", r#"{"draftId":"d1"}"#);
        assert_eq!(structured(&result), json!({ "deleted": true }));
        assert_eq!(script.seen_method(0), Method::Delete);
        assert!(script.seen_url(0).ends_with("/v1.0/me/messages/d1"));
    }

    #[test]
    fn organizes_messages() {
        let mut script = Script::new()
            .will_answer(ok(&json!({})))
            .will_answer(ok(&json!({})));
        let result = script.call(
            "organizeMessage",
            r#"{"action":"markRead","messageIds":["m1","m2"]}"#,
        );
        assert_eq!(
            structured(&result),
            json!({ "action": "markRead", "affectedCount": 2 })
        );
        assert_eq!(script.seen_method(0), Method::Patch);
        assert!(script.seen_url(0).ends_with("/v1.0/me/messages/m1"));
        assert_eq!(script.seen_json(0), json!({ "isRead": true }));
        assert!(script.seen_url(1).ends_with("/v1.0/me/messages/m2"));
    }

    #[test]
    fn organizes_by_query_with_deduplication() {
        let mut script = Script::new()
            .will_answer(ok(&json!({
                "value": [{"id": "m1"}, {"id": "m2"}],
            })))
            .will_answer(ok(&json!({})))
            .will_answer(ok(&json!({})));
        let result = script.call(
            "organizeMessage",
            r#"{"action":"markUnread","messageIds":["m1"],"query":"is:unread"}"#,
        );
        assert_eq!(
            structured(&result),
            json!({ "action": "markUnread", "affectedCount": 2 })
        );
        let url = script.seen_url(0);
        assert!(url.contains("$search=%22is%3Aunread%22"), "{url}");
        assert_eq!(script.seen_header(0, "ConsistencyLevel"), Some("eventual"));
        assert_eq!(script.seen_json(1), json!({ "isRead": false }));
    }

    #[test]
    fn organize_message_moves_to_folders() {
        let mut script = Script::new()
            .will_answer(response(201, &json!({ "id": "m1" }).to_string()))
            .will_answer(response(201, &json!({ "id": "m2" }).to_string()));
        let result = script.call(
            "organizeMessage",
            r#"{"action":"move","messageIds":["m1"],"folderId":"folder9"}"#,
        );
        assert_eq!(
            structured(&result),
            json!({ "action": "move", "affectedCount": 1 })
        );
        assert_eq!(script.seen_method(0), Method::Post);
        assert!(script.seen_url(0).ends_with("/v1.0/me/messages/m1/move"));
        assert_eq!(script.seen_json(0), json!({ "destinationId": "folder9" }));
        let _ = script.call(
            "organizeMessage",
            r#"{"action":"archive","messageIds":["m2"]}"#,
        );
        assert_eq!(script.seen_json(1), json!({ "destinationId": "archive" }));
    }

    #[test]
    fn organize_message_deletes_to_deleted_items() {
        let mut script = Script::new().will_answer(response(204, ""));
        let result = script.call(
            "organizeMessage",
            r#"{"action":"delete","messageIds":["m1"]}"#,
        );
        assert_eq!(
            structured(&result),
            json!({ "action": "delete", "affectedCount": 1 })
        );
        assert_eq!(script.seen_method(0), Method::Delete);
        assert!(script.seen_url(0).ends_with("/v1.0/me/messages/m1"));
    }

    #[test]
    fn organize_message_validates_input() {
        let mut script = Script::new();
        let result = script.call("organizeMessage", r#"{"action":"markRead"}"#);
        assert_eq!(
            result.unwrap_err(),
            ConnectorError::InvalidInput("organizeMessage needs messageIds or query".into())
        );
        let result = script.call(
            "organizeMessage",
            r#"{"action":"explode","messageIds":["m1"]}"#,
        );
        let error = result.unwrap_err();
        assert!(matches!(error, ConnectorError::InvalidInput(_))); // coverage: exclude — matches! false arm is unreachable in a passing test
        let result = script.call(
            "organizeMessage",
            r#"{"action":"move","messageIds":["m1"]}"#,
        );
        assert_eq!(
            result.unwrap_err(),
            ConnectorError::InvalidInput("move needs folderId".into())
        );
    }

    #[test]
    fn organize_message_reports_failed_steps() {
        let mut script = Script::new().will_answer(response(404, "gone"));
        let result = script.call(
            "organizeMessage",
            r#"{"action":"delete","messageIds":["m1"]}"#,
        );
        assert!(is_tool_error(&result));
    }

    #[test]
    fn gets_attachments_by_id() {
        let mut script = Script::new().will_answer(ok(&json!({
            "id": "a1",
            "name": "notes.txt",
            "contentType": "text/plain",
            "size": 5,
            "contentBytes": STANDARD.encode("hello"),
        })));
        let result = script.call("getAttachment", r#"{"messageId":"m1","attachmentId":"a1"}"#);
        assert_eq!(
            structured(&result),
            json!({
                "filename": "notes.txt",
                "mimeType": "text/plain",
                "size": 5,
                "contentBase64": STANDARD.encode("hello"),
            })
        );
        assert!(
            script
                .seen_url(0)
                .ends_with("/v1.0/me/messages/m1/attachments/a1")
        );
    }

    #[test]
    fn gets_attachments_by_filename() {
        let mut script = Script::new()
            .will_answer(ok(&json!({
                "value": [
                    { "id": "a1", "name": "notes.txt", "contentType": "text/plain", "size": 5 },
                    { "id": "a2", "name": "other.pdf", "contentType": "application/pdf", "size": 9 },
                ],
            })))
            .will_answer(ok(&json!({
                "id": "a1",
                "name": "notes.txt",
                "contentType": "text/plain",
                "size": 5,
                "contentBytes": STANDARD.encode("hello"),
            })));
        let result = script.call(
            "getAttachment",
            r#"{"messageId":"m1","filename":"notes.txt"}"#,
        );
        assert_eq!(structured(&result)["filename"], "notes.txt");
        assert!(
            script.seen_url(0).ends_with(
                "/v1.0/me/messages/m1/attachments?$select=id%2Cname%2CcontentType%2Csize"
            )
        );
        assert!(
            script
                .seen_url(1)
                .ends_with("/v1.0/me/messages/m1/attachments/a1")
        );
    }

    #[test]
    fn get_attachment_reports_no_match() {
        let mut script = Script::new().will_answer(ok(&json!({
            "value": [{ "id": "a1", "name": "notes.txt" }],
        })));
        let result = script.call(
            "getAttachment",
            r#"{"messageId":"m1","filename":"missing.txt"}"#,
        );
        assert!(is_tool_error(&result));
    }

    #[test]
    fn get_attachment_rejects_missing_content() {
        let mut script = Script::new().will_answer(ok(&json!({
            "id": "a1",
            "name": "invite.ics",
            "contentType": "text/calendar",
            "size": 5,
        })));
        let result = script.call("getAttachment", r#"{"messageId":"m1","attachmentId":"a1"}"#);
        assert!(is_tool_error(&result));
    }

    #[test]
    fn get_attachment_rejects_bad_base64() {
        let mut script = Script::new().will_answer(ok(&json!({
            "id": "a1",
            "name": "notes.txt",
            "contentType": "text/plain",
            "size": 5,
            "contentBytes": "!!!",
        })));
        let result = script.call("getAttachment", r#"{"messageId":"m1","attachmentId":"a1"}"#);
        assert!(is_tool_error(&result));
    }

    #[test]
    fn get_attachment_validates_input() {
        let mut script = Script::new();
        let result = script.call("getAttachment", r#"{"messageId":"m1"}"#);
        assert_eq!(
            result.unwrap_err(),
            ConnectorError::InvalidInput("getAttachment needs attachmentId or filename".into())
        );
    }

    #[test]
    fn get_attachment_reports_failed_downloads() {
        let mut script = Script::new().will_answer(response(404, "gone"));
        let result = script.call("getAttachment", r#"{"messageId":"m1","attachmentId":"a1"}"#);
        assert!(is_tool_error(&result));
    }

    #[test]
    fn lists_mail_accounts() {
        let mut script = Script::new().will_answer(ok(&json!({
            "displayName": "Ada",
            "mail": "ada@contoso.com",
            "userPrincipalName": "ada@contoso.com",
        })));
        let result = script.call("listMailAccounts", "{}");
        let payload = structured(&result);
        assert_eq!(payload["accounts"].as_array().unwrap().len(), 1);
        assert_eq!(payload["accounts"][0]["mailbox"], "me");
        assert_eq!(payload["accounts"][0]["displayName"], "Ada");
        assert_eq!(payload["accounts"][0]["emailAddress"], "ada@contoso.com");
        assert!(payload["note"].as_str().unwrap().contains("mailbox"));
        assert!(
            script
                .seen_url(0)
                .ends_with("/v1.0/me?$select=displayName%2Cmail%2CuserPrincipalName")
        );
    }

    #[test]
    fn list_mail_accounts_falls_back_to_upn() {
        let mut script = Script::new()
            .will_answer(ok(&json!({
                "displayName": "Ada",
                "mail": "",
                "userPrincipalName": "ada@contoso.com",
            })))
            .will_answer(ok(&json!({ "displayName": "Ada" })));
        let first = script.call("listMailAccounts", "{}");
        assert_eq!(
            structured(&first)["accounts"][0]["emailAddress"],
            "ada@contoso.com"
        );
        let second = script.call("listMailAccounts", "{}");
        assert_eq!(structured(&second)["accounts"][0]["emailAddress"], "");
    }

    #[test]
    fn checked_reports_garbled_bodies() {
        // A 200 with a body that is not JSON at all fails in `checked`,
        // before any message parsing.
        let mut script = Script::new().will_answer(response(200, "not json"));
        let result = script.call("getMessage", r#"{"messageId":"m1"}"#);
        assert!(is_tool_error(&result));
    }

    #[test]
    fn get_message_reports_garbled_message() {
        let mut script = Script::new().will_answer(ok(&json!({ "id": 42 })));
        let result = script.call("getMessage", r#"{"messageId":"m1"}"#);
        assert!(is_tool_error(&result));
    }

    #[test]
    fn search_messages_reports_garbled_browse_messages() {
        let mut script = Script::new().will_answer(ok(&json!({ "value": [42] })));
        let result = script.call("searchMessages", "{}");
        assert!(is_tool_error(&result));
    }

    #[test]
    fn reply_to_message_rejects_bad_attachment_base64() {
        let mut script = Script::new();
        let result = script.call(
            "replyToMessage",
            r#"{"messageId":"m1","body":"x","attachments":[{"filename":"n.txt","mimeType":"text/plain","contentBase64":"!!!"}]}"#,
        );
        let error = result.unwrap_err();
        assert!(matches!(error, ConnectorError::InvalidInput(_))); // coverage: exclude — matches! false arm is unreachable in a passing test
    }

    #[test]
    fn save_draft_reports_failed_steps() {
        let mut script = Script::new().will_answer(response(400, "bad"));
        let result = script.call("saveDraft", r#"{"subject":"x"}"#);
        assert!(is_tool_error(&result));

        // createReply itself fails.
        let mut script = Script::new().will_answer(response(400, "bad"));
        let result = script.call("saveDraft", r#"{"body":"x","inReplyToMessageId":"m1"}"#);
        assert!(is_tool_error(&result));

        // createReply answers 200 with a body that is not a message.
        let mut script = Script::new().will_answer(ok(&json!({ "id": 42 })));
        let result = script.call("saveDraft", r#"{"body":"x","inReplyToMessageId":"m1"}"#);
        assert!(is_tool_error(&result));

        // The threaded draft body is invalid.
        let mut script = Script::new().will_answer(ok(&message_fixture(
            "d2",
            "Re: Hi",
            "Me",
            "me@contoso.com",
            "",
        )));
        let result = script.call(
            "saveDraft",
            r#"{"body":"x","bodyType":"bogus","inReplyToMessageId":"m1"}"#,
        );
        let error = result.unwrap_err();
        assert!(matches!(error, ConnectorError::InvalidInput(_))); // coverage: exclude — matches! false arm is unreachable in a passing test

        // The reply-draft PATCH fails.
        let mut script = Script::new()
            .will_answer(ok(&message_fixture(
                "d2",
                "Re: Hi",
                "Me",
                "me@contoso.com",
                "",
            )))
            .will_answer(response(400, "bad"));
        let result = script.call("saveDraft", r#"{"body":"x","inReplyToMessageId":"m1"}"#);
        assert!(is_tool_error(&result));

        // The reply-draft PATCH answers 200 with a body that is not a message.
        let mut script = Script::new()
            .will_answer(ok(&message_fixture(
                "d2",
                "Re: Hi",
                "Me",
                "me@contoso.com",
                "",
            )))
            .will_answer(ok(&json!({ "id": 42 })));
        let result = script.call("saveDraft", r#"{"body":"x","inReplyToMessageId":"m1"}"#);
        assert!(is_tool_error(&result));

        // A plain save answers 200 with a body that is not a message.
        let mut script = Script::new().will_answer(ok(&json!({ "id": 42 })));
        let result = script.call("saveDraft", r#"{"subject":"x"}"#);
        assert!(is_tool_error(&result));
    }

    #[test]
    fn save_draft_rejects_bad_attachment_base64() {
        let mut script = Script::new();
        let result = script.call(
            "saveDraft",
            r#"{"subject":"Hi","attachments":[{"filename":"n.txt","mimeType":"text/plain","contentBase64":"!!!"}]}"#,
        );
        let error = result.unwrap_err();
        assert!(matches!(error, ConnectorError::InvalidInput(_))); // coverage: exclude — matches! false arm is unreachable in a passing test
    }

    #[test]
    fn saves_drafts_with_attachments() {
        let mut script = Script::new().will_answer(response(
            201,
            &message_fixture("d1", "Draft", "Me", "me@contoso.com", "").to_string(),
        ));
        let result = script.call(
            "saveDraft",
            r#"{"subject":"Hi","attachments":[{"filename":"n.txt","mimeType":"text/plain","contentBase64":"aGk="}]}"#,
        );
        assert_eq!(
            structured(&result),
            json!({ "draftId": "d1", "messageId": "d1" })
        );
        assert_eq!(script.seen_json(0)["attachments"][0]["name"], "n.txt");
    }

    #[test]
    fn organize_message_reports_failed_query_listing() {
        let mut script = Script::new().will_answer(response(400, "bad"));
        let result = script.call("organizeMessage", r#"{"action":"archive","query":"x"}"#);
        assert!(is_tool_error(&result));
    }

    #[test]
    fn get_attachment_reports_failed_listing() {
        let mut script = Script::new().will_answer(response(400, "bad"));
        let result = script.call(
            "getAttachment",
            r#"{"messageId":"m1","filename":"notes.txt"}"#,
        );
        assert!(is_tool_error(&result));
    }

    #[test]
    fn get_attachment_reports_garbled_bodies() {
        // Valid JSON that is not an attachment fails in `attachment_from`,
        // after `checked` has accepted the body.
        let mut script = Script::new().will_answer(ok(&json!({ "name": 42 })));
        let result = script.call("getAttachment", r#"{"messageId":"m1","attachmentId":"a1"}"#);
        assert!(is_tool_error(&result));
    }

    #[test]
    fn list_mail_accounts_reports_failed_steps() {
        let mut script = Script::new().will_answer(response(403, "denied"));
        let result = script.call("listMailAccounts", "{}");
        assert!(is_tool_error(&result));
    }

    #[test]
    fn rejects_bad_input() {
        // Every dispatch arm parses the body with `request.json()?`: malformed
        // JSON must fail input validation before anything is sent.
        for tool in [
            "searchMessages",
            "getMessage",
            "sendMessage",
            "replyToMessage",
            "forwardMessage",
            "saveDraft",
            "sendDraft",
            "discardDraft",
            "organizeMessage",
            "getAttachment",
            "listMailAccounts",
        ] {
            let mut script = Script::new();
            let error = script.call(tool, "{oops").unwrap_err();
            assert!(matches!(error, ConnectorError::InvalidInput(_)), "{tool}"); // coverage: exclude — matches! false arm is unreachable in a passing test
            assert!(script.seen.is_empty(), "{tool} sent nothing");
        }
    }

    #[test]
    fn surfaces_garbled_message_bodies() {
        let mut script = Script::new().will_answer(ok(&json!({ "value": [42] })));
        let result = script.call("searchMessages", r#"{"query":"x"}"#);
        assert!(is_tool_error(&result));
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
        let result = OutlookMail::call_tool(tool_request("searchMessages", r#"{"query":"x"}"#));
        assert!(is_tool_error(&result));
    }

    #[test]
    fn manifest_is_embedded() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["name"], "outlook-mail");
        assert_eq!(manifest["version"], "0.1.0");
        assert_eq!(manifest["auth"]["schemes"][0]["type"], "oauth2");
        assert_eq!(
            manifest["auth"]["schemes"][0]["scopes"][0],
            "https://graph.microsoft.com/Mail.ReadWrite"
        );
        assert_eq!(
            manifest["auth"]["schemes"][0]["scopes"][2],
            "https://graph.microsoft.com/Mail.ReadWrite.Shared"
        );
        assert_eq!(
            manifest["auth"]["schemes"][0]["scopes"][3],
            "https://graph.microsoft.com/Mail.Send.Shared"
        );
        assert_eq!(
            manifest["permissions"]["http"]["allow"][0],
            DEFAULT_API_BASE
        );
        let tools = manifest["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 11);
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "searchMessages",
                "getMessage",
                "sendMessage",
                "replyToMessage",
                "forwardMessage",
                "saveDraft",
                "sendDraft",
                "discardDraft",
                "organizeMessage",
                "getAttachment",
                "listMailAccounts",
            ]
        );
    }
}
