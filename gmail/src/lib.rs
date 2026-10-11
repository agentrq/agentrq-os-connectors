//! Gmail connector: the Gmail API v1 through OAuth 2.0, minus the
//! settings subtree (labels are covered; filters, send-as identities,
//! forwarding, vacation responder and the rest are intentionally not
//! offered). The host runs the OAuth 2.0 sign-in, refreshes the token and
//! adds `Authorization: Bearer …` after each request leaves the guest: this
//! code never sees it.
//!
//! Every tool takes an optional `userId` (default `"me"`); the remaining
//! parameters mirror the API's camelCase names.

use connectors_sdk::http::{Method, Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::Deserialize;
use serde_json::{Value, json};

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://www.googleapis.com";

/// The connector.
#[derive(Debug)]
pub struct Gmail;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetProfile {
    user_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListMessages {
    user_id: Option<String>,
    q: Option<String>,
    page_token: Option<String>,
    max_results: Option<u32>,
    label_ids: Option<Vec<String>>,
    include_spam_trash: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetMessage {
    user_id: Option<String>,
    id: String,
    format: Option<String>,
    metadata_headers: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendMessage {
    user_id: Option<String>,
    raw: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InsertMessage {
    user_id: Option<String>,
    raw: String,
    internal_date_source: Option<String>,
    deleted: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportMessage {
    user_id: Option<String>,
    raw: String,
    internal_date_source: Option<String>,
    never_mark_spam: Option<bool>,
    process_for_calendar: Option<bool>,
    deleted: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrashMessage {
    user_id: Option<String>,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UntrashMessage {
    user_id: Option<String>,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteMessage {
    user_id: Option<String>,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModifyMessage {
    user_id: Option<String>,
    id: String,
    add_label_ids: Option<Vec<String>>,
    remove_label_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchModifyMessages {
    user_id: Option<String>,
    ids: Vec<String>,
    add_label_ids: Option<Vec<String>>,
    remove_label_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchDeleteMessages {
    user_id: Option<String>,
    ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetMessageAttachment {
    user_id: Option<String>,
    message_id: String,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListThreads {
    user_id: Option<String>,
    q: Option<String>,
    page_token: Option<String>,
    max_results: Option<u32>,
    label_ids: Option<Vec<String>>,
    include_spam_trash: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetThread {
    user_id: Option<String>,
    id: String,
    format: Option<String>,
    metadata_headers: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModifyThread {
    user_id: Option<String>,
    id: String,
    add_label_ids: Option<Vec<String>>,
    remove_label_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrashThread {
    user_id: Option<String>,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UntrashThread {
    user_id: Option<String>,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteThread {
    user_id: Option<String>,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListLabels {
    user_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetLabel {
    user_id: Option<String>,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateLabel {
    user_id: Option<String>,
    name: String,
    message_list_visibility: Option<String>,
    label_list_visibility: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateLabel {
    user_id: Option<String>,
    id: String,
    name: Option<String>,
    message_list_visibility: Option<String>,
    label_list_visibility: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteLabel {
    user_id: Option<String>,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateDraft {
    user_id: Option<String>,
    raw: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateDraft {
    user_id: Option<String>,
    id: String,
    raw: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListDrafts {
    user_id: Option<String>,
    page_token: Option<String>,
    max_results: Option<u32>,
    include_spam_trash: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetDraft {
    user_id: Option<String>,
    id: String,
    format: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendDraft {
    user_id: Option<String>,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteDraft {
    user_id: Option<String>,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListHistory {
    user_id: Option<String>,
    start_history_id: u64,
    page_token: Option<String>,
    max_results: Option<u32>,
    label_id: Option<String>,
    history_types: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WatchMailbox {
    user_id: Option<String>,
    topic_name: String,
    label_ids: Option<Vec<String>>,
    label_filter_behavior: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StopMailbox {
    user_id: Option<String>,
}

impl Connector for Gmail {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        let base = host::config("api_base").unwrap_or_else(|| DEFAULT_API_BASE.into());
        answer(&outgoing(&base, &request)?.send()?)
    }
}

connectors_sdk::export_connector!(Gmail);

/// The user a request acts as: `userId`, or "me".
fn user(user_id: &Option<String>) -> &str {
    user_id.as_deref().unwrap_or("me")
}

/// `{root}/gmail/v1/users/{userId}/{path}`: the endpoint a tool call hits.
fn user_url(root: &str, user_id: &Option<String>, path: &str) -> String {
    format!("{root}/gmail/v1/users/{}/{path}", user(user_id))
}

/// The query parameters shared by `messages.list` and `threads.list`.
fn list_query(
    q: Option<String>,
    page_token: Option<String>,
    max_results: Option<u32>,
    label_ids: Option<Vec<String>>,
    include_spam_trash: Option<bool>,
) -> [(&'static str, Option<String>); 5] {
    [
        ("q", q),
        ("pageToken", page_token),
        ("maxResults", max_results.map(|n| n.to_string())),
        ("labelIds", label_ids.map(|ids| ids.join(","))),
        (
            "includeSpamTrash",
            include_spam_trash.map(|b| b.to_string()),
        ),
    ]
}

/// `{"addLabelIds": …, "removeLabelIds": …}` for the modify endpoints.
fn modify_body(
    add_label_ids: Option<&Vec<String>>,
    remove_label_ids: Option<&Vec<String>>,
) -> Value {
    object(&[
        ("addLabelIds", add_label_ids.map(|ids| json!(ids))),
        ("removeLabelIds", remove_label_ids.map(|ids| json!(ids))),
    ])
}

/// The JSON body for `labels.update` and `labels.patch`: the label fields
/// that are present.
fn label_body(input: &UpdateLabel) -> Value {
    object(&[
        ("name", input.name.as_ref().map(|s| json!(s))),
        (
            "messageListVisibility",
            input.message_list_visibility.as_ref().map(|s| json!(s)),
        ),
        (
            "labelListVisibility",
            input.label_list_visibility.as_ref().map(|s| json!(s)),
        ),
    ])
}

/// The request a tool call makes to the API at `base`.
fn outgoing(base: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let root = base.trim_end_matches('/');
    let outgoing = match request.tool.as_str() {
        "getProfile" => {
            let input: GetProfile = request.json()?;
            Request::get(user_url(root, &input.user_id, "profile"))
        }
        "listMessages" => {
            let input: ListMessages = request.json()?;
            Request::get(with_query(
                &user_url(root, &input.user_id, "messages"),
                &list_query(
                    input.q,
                    input.page_token,
                    input.max_results,
                    input.label_ids,
                    input.include_spam_trash,
                ),
            ))
        }
        "getMessage" => {
            let input: GetMessage = request.json()?;
            Request::get(with_query(
                &user_url(
                    root,
                    &input.user_id,
                    &format!("messages/{}", encode(&input.id)),
                ),
                &[
                    ("format", input.format),
                    (
                        "metadataHeaders",
                        input.metadata_headers.map(|headers| headers.join(",")),
                    ),
                ],
            ))
        }
        "sendMessage" => {
            let input: SendMessage = request.json()?;
            let body = object(&[("raw", Some(json!(input.raw)))]);
            Request::post(user_url(root, &input.user_id, "messages/send")).json(&body)?
        }
        "insertMessage" => {
            let input: InsertMessage = request.json()?;
            let url = with_query(
                &user_url(root, &input.user_id, "messages"),
                &[
                    ("internalDateSource", input.internal_date_source),
                    ("deleted", input.deleted.map(|b| b.to_string())),
                ],
            );
            let body = object(&[("raw", Some(json!(input.raw)))]);
            Request::post(url).json(&body)?
        }
        "importMessage" => {
            let input: ImportMessage = request.json()?;
            let url = with_query(
                &user_url(root, &input.user_id, "messages/import"),
                &[
                    ("internalDateSource", input.internal_date_source),
                    (
                        "neverMarkSpam",
                        input.never_mark_spam.map(|b| b.to_string()),
                    ),
                    (
                        "processForCalendar",
                        input.process_for_calendar.map(|b| b.to_string()),
                    ),
                    ("deleted", input.deleted.map(|b| b.to_string())),
                ],
            );
            let body = object(&[("raw", Some(json!(input.raw)))]);
            Request::post(url).json(&body)?
        }
        "trashMessage" => {
            let input: TrashMessage = request.json()?;
            Request::post(user_url(
                root,
                &input.user_id,
                &format!("messages/{}/trash", encode(&input.id)),
            ))
        }
        "untrashMessage" => {
            let input: UntrashMessage = request.json()?;
            Request::post(user_url(
                root,
                &input.user_id,
                &format!("messages/{}/untrash", encode(&input.id)),
            ))
        }
        "deleteMessage" => {
            let input: DeleteMessage = request.json()?;
            Request::new(
                Method::Delete,
                user_url(
                    root,
                    &input.user_id,
                    &format!("messages/{}", encode(&input.id)),
                ),
            )
        }
        "modifyMessage" => {
            let input: ModifyMessage = request.json()?;
            let body = modify_body(
                input.add_label_ids.as_ref(),
                input.remove_label_ids.as_ref(),
            );
            Request::post(user_url(
                root,
                &input.user_id,
                &format!("messages/{}/modify", encode(&input.id)),
            ))
            .json(&body)?
        }
        "batchModifyMessages" => {
            let input: BatchModifyMessages = request.json()?;
            let mut body = modify_body(
                input.add_label_ids.as_ref(),
                input.remove_label_ids.as_ref(),
            );
            body["ids"] = json!(input.ids);
            Request::post(user_url(root, &input.user_id, "messages/batchModify")).json(&body)?
        }
        "batchDeleteMessages" => {
            let input: BatchDeleteMessages = request.json()?;
            let body = object(&[("ids", Some(json!(input.ids)))]);
            Request::post(user_url(root, &input.user_id, "messages/batchDelete")).json(&body)?
        }
        "getMessageAttachment" => {
            let input: GetMessageAttachment = request.json()?;
            Request::get(user_url(
                root,
                &input.user_id,
                &format!(
                    "messages/{}/attachments/{}",
                    encode(&input.message_id),
                    encode(&input.id)
                ),
            ))
        }
        "listThreads" => {
            let input: ListThreads = request.json()?;
            Request::get(with_query(
                &user_url(root, &input.user_id, "threads"),
                &list_query(
                    input.q,
                    input.page_token,
                    input.max_results,
                    input.label_ids,
                    input.include_spam_trash,
                ),
            ))
        }
        "getThread" => {
            let input: GetThread = request.json()?;
            Request::get(with_query(
                &user_url(
                    root,
                    &input.user_id,
                    &format!("threads/{}", encode(&input.id)),
                ),
                &[
                    ("format", input.format),
                    (
                        "metadataHeaders",
                        input.metadata_headers.map(|headers| headers.join(",")),
                    ),
                ],
            ))
        }
        "modifyThread" => {
            let input: ModifyThread = request.json()?;
            let body = modify_body(
                input.add_label_ids.as_ref(),
                input.remove_label_ids.as_ref(),
            );
            Request::post(user_url(
                root,
                &input.user_id,
                &format!("threads/{}/modify", encode(&input.id)),
            ))
            .json(&body)?
        }
        "trashThread" => {
            let input: TrashThread = request.json()?;
            Request::post(user_url(
                root,
                &input.user_id,
                &format!("threads/{}/trash", encode(&input.id)),
            ))
        }
        "untrashThread" => {
            let input: UntrashThread = request.json()?;
            Request::post(user_url(
                root,
                &input.user_id,
                &format!("threads/{}/untrash", encode(&input.id)),
            ))
        }
        "deleteThread" => {
            let input: DeleteThread = request.json()?;
            Request::new(
                Method::Delete,
                user_url(
                    root,
                    &input.user_id,
                    &format!("threads/{}", encode(&input.id)),
                ),
            )
        }
        "listLabels" => {
            let input: ListLabels = request.json()?;
            Request::get(user_url(root, &input.user_id, "labels"))
        }
        "getLabel" => {
            let input: GetLabel = request.json()?;
            Request::get(user_url(
                root,
                &input.user_id,
                &format!("labels/{}", encode(&input.id)),
            ))
        }
        "createLabel" => {
            let input: CreateLabel = request.json()?;
            let body = object(&[
                ("name", Some(json!(input.name))),
                (
                    "messageListVisibility",
                    input.message_list_visibility.as_ref().map(|s| json!(s)),
                ),
                (
                    "labelListVisibility",
                    input.label_list_visibility.as_ref().map(|s| json!(s)),
                ),
            ]);
            Request::post(user_url(root, &input.user_id, "labels")).json(&body)?
        }
        "updateLabel" => {
            let input: UpdateLabel = request.json()?;
            let body = label_body(&input);
            Request::new(
                Method::Put,
                user_url(
                    root,
                    &input.user_id,
                    &format!("labels/{}", encode(&input.id)),
                ),
            )
            .json(&body)?
        }
        "patchLabel" => {
            let input: UpdateLabel = request.json()?;
            let body = label_body(&input);
            Request::new(
                Method::Patch,
                user_url(
                    root,
                    &input.user_id,
                    &format!("labels/{}", encode(&input.id)),
                ),
            )
            .json(&body)?
        }
        "deleteLabel" => {
            let input: DeleteLabel = request.json()?;
            Request::new(
                Method::Delete,
                user_url(
                    root,
                    &input.user_id,
                    &format!("labels/{}", encode(&input.id)),
                ),
            )
        }
        "createDraft" => {
            let input: CreateDraft = request.json()?;
            let body = object(&[("message", Some(json!({ "raw": input.raw })))]);
            Request::post(user_url(root, &input.user_id, "drafts")).json(&body)?
        }
        "updateDraft" => {
            let input: UpdateDraft = request.json()?;
            let body = object(&[("message", Some(json!({ "raw": input.raw })))]);
            Request::new(
                Method::Put,
                user_url(
                    root,
                    &input.user_id,
                    &format!("drafts/{}", encode(&input.id)),
                ),
            )
            .json(&body)?
        }
        "listDrafts" => {
            let input: ListDrafts = request.json()?;
            Request::get(with_query(
                &user_url(root, &input.user_id, "drafts"),
                &[
                    ("pageToken", input.page_token),
                    ("maxResults", input.max_results.map(|n| n.to_string())),
                    (
                        "includeSpamTrash",
                        input.include_spam_trash.map(|b| b.to_string()),
                    ),
                ],
            ))
        }
        "getDraft" => {
            let input: GetDraft = request.json()?;
            Request::get(with_query(
                &user_url(
                    root,
                    &input.user_id,
                    &format!("drafts/{}", encode(&input.id)),
                ),
                &[("format", input.format)],
            ))
        }
        "sendDraft" => {
            let input: SendDraft = request.json()?;
            let body = object(&[("id", Some(json!(input.id)))]);
            Request::post(user_url(root, &input.user_id, "drafts/send")).json(&body)?
        }
        "deleteDraft" => {
            let input: DeleteDraft = request.json()?;
            Request::new(
                Method::Delete,
                user_url(
                    root,
                    &input.user_id,
                    &format!("drafts/{}", encode(&input.id)),
                ),
            )
        }
        "listHistory" => {
            let input: ListHistory = request.json()?;
            Request::get(with_query(
                &user_url(root, &input.user_id, "history"),
                &[
                    ("startHistoryId", Some(input.start_history_id.to_string())),
                    ("pageToken", input.page_token),
                    ("maxResults", input.max_results.map(|n| n.to_string())),
                    ("labelId", input.label_id),
                    (
                        "historyTypes",
                        input.history_types.map(|types| types.join(",")),
                    ),
                ],
            ))
        }
        "watchMailbox" => {
            let input: WatchMailbox = request.json()?;
            let body = object(&[
                ("topicName", Some(json!(input.topic_name))),
                ("labelIds", input.label_ids.as_ref().map(|ids| json!(ids))),
                (
                    "labelFilterBehavior",
                    input.label_filter_behavior.as_ref().map(|s| json!(s)),
                ),
            ]);
            Request::post(user_url(root, &input.user_id, "watch")).json(&body)?
        }
        "stopMailbox" => {
            let input: StopMailbox = request.json()?;
            Request::post(user_url(root, &input.user_id, "stop")).json(&json!({}))?
        }
        other => return Err(ConnectorError::NotFound(format!("no tool {other}"))),
    };
    Ok(outgoing.header("accept", "application/json"))
}

/// The tool result of the API's response: its JSON, or a tool error that
/// says what the API answered, so the model can react (a 403 usually means
/// a missing scope). `messages.delete`, `messages.batchDelete`,
/// `threads.delete`, `labels.delete` and `drafts.delete` answer 204 with no
/// body.
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

    fn request(tool: &str, body: &str) -> ToolRequest {
        ToolRequest {
            tool: tool.into(),
            body: body.into(),
        }
    }

    fn planned(tool: &str, body: &str) -> Request {
        outgoing("https://www.test/", &request(tool, body)).unwrap()
    }

    fn response(status: u16, body: &str) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    const USERS: &str = "https://www.test/gmail/v1/users";

    fn json_body(request: &Request) -> Value {
        serde_json::from_slice(&request.body).unwrap()
    }

    #[test]
    fn plans_profile() {
        assert_eq!(
            planned("getProfile", "{}").url,
            format!("{USERS}/me/profile")
        );
        assert_eq!(
            planned("getProfile", r#"{"userId":"alex"}"#).url,
            format!("{USERS}/alex/profile")
        );
    }

    #[test]
    fn plans_list_and_get_messages() {
        assert_eq!(
            planned("listMessages", "{}").url,
            format!("{USERS}/me/messages")
        );
        assert_eq!(
            planned(
                "listMessages",
                r#"{"userId":"me","q":"from:boss","pageToken":"tok","maxResults":20,"labelIds":["INBOX","UNREAD"],"includeSpamTrash":true}"#
            )
            .url,
            format!(
                "{USERS}/me/messages?q=from%3Aboss&pageToken=tok&maxResults=20\
                 &labelIds=INBOX%2CUNREAD&includeSpamTrash=true"
            ),
        );
        assert_eq!(
            planned(
                "getMessage",
                r#"{"id":"a b","format":"full","metadataHeaders":["From","Subject"]}"#
            )
            .url,
            format!("{USERS}/me/messages/a%20b?format=full&metadataHeaders=From%2CSubject")
        );
        assert_eq!(
            planned("getMessage", r#"{"userId":"me","id":"m1"}"#).url,
            format!("{USERS}/me/messages/m1")
        );
    }

    #[test]
    fn plans_send_insert_import() {
        let sent = planned("sendMessage", r#"{"userId":"me","raw":"cmF3"}"#);
        assert_eq!(sent.method, Method::Post);
        assert_eq!(sent.url, format!("{USERS}/me/messages/send"));
        assert_eq!(json_body(&sent), json!({ "raw": "cmF3" }));

        let inserted = planned(
            "insertMessage",
            r#"{"raw":"cmF3","internalDateSource":"dateHeader","deleted":false}"#,
        );
        assert_eq!(inserted.method, Method::Post);
        assert_eq!(
            inserted.url,
            format!("{USERS}/me/messages?internalDateSource=dateHeader&deleted=false")
        );
        assert_eq!(json_body(&inserted), json!({ "raw": "cmF3" }));

        let imported = planned(
            "importMessage",
            r#"{"userId":"me","raw":"cmF3","internalDateSource":"dateHeader","neverMarkSpam":true,"processForCalendar":true,"deleted":false}"#,
        );
        assert_eq!(imported.method, Method::Post);
        assert_eq!(
            imported.url,
            format!(
                "{USERS}/me/messages/import?internalDateSource=dateHeader\
                 &neverMarkSpam=true&processForCalendar=true&deleted=false"
            ),
        );
        assert_eq!(json_body(&imported), json!({ "raw": "cmF3" }));
    }

    #[test]
    fn plans_message_state() {
        let trashed = planned("trashMessage", r#"{"id":"m1"}"#);
        assert_eq!(trashed.method, Method::Post);
        assert_eq!(trashed.url, format!("{USERS}/me/messages/m1/trash"));

        let untrashed = planned("untrashMessage", r#"{"userId":"me","id":"m1"}"#);
        assert_eq!(untrashed.method, Method::Post);
        assert_eq!(untrashed.url, format!("{USERS}/me/messages/m1/untrash"));

        let deleted = planned("deleteMessage", r#"{"id":"m1"}"#);
        assert_eq!(deleted.method, Method::Delete);
        assert_eq!(deleted.url, format!("{USERS}/me/messages/m1"));

        let modified = planned(
            "modifyMessage",
            r#"{"userId":"me","id":"m1","addLabelIds":["STARRED"],"removeLabelIds":["UNREAD"]}"#,
        );
        assert_eq!(modified.method, Method::Post);
        assert_eq!(modified.url, format!("{USERS}/me/messages/m1/modify"));
        assert_eq!(
            json_body(&modified),
            json!({ "addLabelIds": ["STARRED"], "removeLabelIds": ["UNREAD"] })
        );

        let batch_modified = planned(
            "batchModifyMessages",
            r#"{"ids":["m1","m2"],"addLabelIds":["STARRED"],"removeLabelIds":["UNREAD"]}"#,
        );
        assert_eq!(batch_modified.method, Method::Post);
        assert_eq!(
            batch_modified.url,
            format!("{USERS}/me/messages/batchModify")
        );
        assert_eq!(
            json_body(&batch_modified),
            json!({
                "ids": ["m1", "m2"],
                "addLabelIds": ["STARRED"],
                "removeLabelIds": ["UNREAD"],
            })
        );

        let batch_deleted = planned("batchDeleteMessages", r#"{"userId":"me","ids":["m1"]}"#);
        assert_eq!(batch_deleted.method, Method::Post);
        assert_eq!(
            batch_deleted.url,
            format!("{USERS}/me/messages/batchDelete")
        );
        assert_eq!(json_body(&batch_deleted), json!({ "ids": ["m1"] }));

        let attachment = planned(
            "getMessageAttachment",
            r#"{"userId":"me","messageId":"m1","id":"a1"}"#,
        );
        assert_eq!(attachment.method, Method::Get);
        assert_eq!(
            attachment.url,
            format!("{USERS}/me/messages/m1/attachments/a1")
        );
    }

    #[test]
    fn plans_threads() {
        assert_eq!(
            planned("listThreads", "{}").url,
            format!("{USERS}/me/threads")
        );
        assert_eq!(
            planned(
                "listThreads",
                r#"{"q":"in:inbox","pageToken":"tok","maxResults":10,"labelIds":["INBOX"],"includeSpamTrash":false}"#
            )
            .url,
            format!(
                "{USERS}/me/threads?q=in%3Ainbox&pageToken=tok&maxResults=10\
                 &labelIds=INBOX&includeSpamTrash=false"
            ),
        );
        assert_eq!(
            planned(
                "getThread",
                r#"{"userId":"me","id":"t1","format":"metadata","metadataHeaders":["Subject"]}"#
            )
            .url,
            format!("{USERS}/me/threads/t1?format=metadata&metadataHeaders=Subject")
        );

        let modified = planned(
            "modifyThread",
            r#"{"id":"t1","addLabelIds":["STARRED"],"removeLabelIds":["UNREAD"]}"#,
        );
        assert_eq!(modified.method, Method::Post);
        assert_eq!(modified.url, format!("{USERS}/me/threads/t1/modify"));
        assert_eq!(
            json_body(&modified),
            json!({ "addLabelIds": ["STARRED"], "removeLabelIds": ["UNREAD"] })
        );

        let trashed = planned("trashThread", r#"{"id":"t1"}"#);
        assert_eq!(trashed.url, format!("{USERS}/me/threads/t1/trash"));

        let untrashed = planned("untrashThread", r#"{"userId":"me","id":"t1"}"#);
        assert_eq!(untrashed.url, format!("{USERS}/me/threads/t1/untrash"));

        let deleted = planned("deleteThread", r#"{"id":"t1"}"#);
        assert_eq!(deleted.method, Method::Delete);
        assert_eq!(deleted.url, format!("{USERS}/me/threads/t1"));
    }

    #[test]
    fn plans_labels() {
        assert_eq!(
            planned("listLabels", r#"{"userId":"me"}"#).url,
            format!("{USERS}/me/labels")
        );
        assert_eq!(
            planned("getLabel", r#"{"id":"INBOX"}"#).url,
            format!("{USERS}/me/labels/INBOX")
        );

        let created = planned(
            "createLabel",
            r#"{"userId":"me","name":"Receipts","messageListVisibility":"show","labelListVisibility":"labelShow"}"#,
        );
        assert_eq!(created.method, Method::Post);
        assert_eq!(created.url, format!("{USERS}/me/labels"));
        assert_eq!(
            json_body(&created),
            json!({
                "name": "Receipts",
                "messageListVisibility": "show",
                "labelListVisibility": "labelShow",
            })
        );

        let updated = planned(
            "updateLabel",
            r#"{"id":"L1","name":"Receipts 2","messageListVisibility":"hide","labelListVisibility":"labelHide"}"#,
        );
        assert_eq!(updated.method, Method::Put);
        assert_eq!(updated.url, format!("{USERS}/me/labels/L1"));
        assert_eq!(
            json_body(&updated),
            json!({
                "name": "Receipts 2",
                "messageListVisibility": "hide",
                "labelListVisibility": "labelHide",
            })
        );

        let patched = planned("patchLabel", r#"{"userId":"me","id":"L1"}"#);
        assert_eq!(patched.method, Method::Patch);
        assert_eq!(patched.url, format!("{USERS}/me/labels/L1"));
        assert_eq!(json_body(&patched), json!({}));

        let deleted = planned("deleteLabel", r#"{"userId":"me","id":"L1"}"#);
        assert_eq!(deleted.method, Method::Delete);
        assert_eq!(deleted.url, format!("{USERS}/me/labels/L1"));
    }

    #[test]
    fn plans_drafts() {
        let created = planned("createDraft", r#"{"userId":"me","raw":"cmF3"}"#);
        assert_eq!(created.method, Method::Post);
        assert_eq!(created.url, format!("{USERS}/me/drafts"));
        assert_eq!(json_body(&created), json!({ "message": { "raw": "cmF3" } }));

        let updated = planned("updateDraft", r#"{"id":"d1","raw":"bmV3"}"#);
        assert_eq!(updated.method, Method::Put);
        assert_eq!(updated.url, format!("{USERS}/me/drafts/d1"));
        assert_eq!(json_body(&updated), json!({ "message": { "raw": "bmV3" } }));

        assert_eq!(
            planned("listDrafts", "{}").url,
            format!("{USERS}/me/drafts")
        );
        assert_eq!(
            planned(
                "listDrafts",
                r#"{"userId":"me","pageToken":"t","maxResults":10,"includeSpamTrash":true}"#
            )
            .url,
            format!("{USERS}/me/drafts?pageToken=t&maxResults=10&includeSpamTrash=true")
        );
        assert_eq!(
            planned("getDraft", r#"{"id":"d1","format":"full"}"#).url,
            format!("{USERS}/me/drafts/d1?format=full")
        );

        let sent = planned("sendDraft", r#"{"userId":"me","id":"d1"}"#);
        assert_eq!(sent.method, Method::Post);
        assert_eq!(sent.url, format!("{USERS}/me/drafts/send"));
        assert_eq!(json_body(&sent), json!({ "id": "d1" }));

        let deleted = planned("deleteDraft", r#"{"id":"d1"}"#);
        assert_eq!(deleted.method, Method::Delete);
        assert_eq!(deleted.url, format!("{USERS}/me/drafts/d1"));
    }

    #[test]
    fn plans_history_watch_stop() {
        assert_eq!(
            planned(
                "listHistory",
                r#"{"userId":"me","startHistoryId":12345,"pageToken":"p","maxResults":50,"labelId":"INBOX","historyTypes":["messageAdded","messageDeleted"]}"#
            )
            .url,
            format!(
                "{USERS}/me/history?startHistoryId=12345&pageToken=p&maxResults=50\
                 &labelId=INBOX&historyTypes=messageAdded%2CmessageDeleted"
            ),
        );

        let watched = planned(
            "watchMailbox",
            r#"{"userId":"me","topicName":"projects/x/topics/y","labelIds":["INBOX"],"labelFilterBehavior":"include"}"#,
        );
        assert_eq!(watched.method, Method::Post);
        assert_eq!(watched.url, format!("{USERS}/me/watch"));
        assert_eq!(
            json_body(&watched),
            json!({
                "topicName": "projects/x/topics/y",
                "labelIds": ["INBOX"],
                "labelFilterBehavior": "include",
            })
        );

        let stopped = planned("stopMailbox", r#"{"userId":"me"}"#);
        assert_eq!(stopped.method, Method::Post);
        assert_eq!(stopped.url, format!("{USERS}/me/stop"));
        assert_eq!(json_body(&stopped), json!({}));
    }

    #[test]
    fn refuses_bad_input() {
        let missing = outgoing(DEFAULT_API_BASE, &request("getMessage", "{}"));
        assert!(
            matches!(missing, Err(ConnectorError::InvalidInput(_))),
            "{missing:?}"
        );
        let garbled = outgoing(DEFAULT_API_BASE, &request("getMessage", "not json"));
        assert!(
            matches!(garbled, Err(ConnectorError::InvalidInput(_))),
            "{garbled:?}"
        );
        let unknown = outgoing(DEFAULT_API_BASE, &request("nope", "{}"));
        assert_eq!(
            unknown.unwrap_err(),
            ConnectorError::NotFound("no tool nope".into())
        );
    }

    #[test]
    fn maps_responses() {
        let ok = answer(&response(200, r#"{"id":"m1","labelIds":["INBOX"]}"#)).unwrap();
        assert_eq!(
            ok.structured.as_deref(),
            Some(r#"{"id":"m1","labelIds":["INBOX"]}"#)
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
    fn off_wasm_nothing_is_sent() {
        let sent = Gmail::call_tool(request("listMessages", "{}"));
        assert!(matches!(&sent, Err(ConnectorError::Upstream(m)) if m.contains("wasm32-wasip2")));
    }

    #[test]
    fn manifest_is_embedded() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["name"], "gmail");
        assert_eq!(manifest["auth"]["schemes"][0]["type"], "oauth2");
        assert_eq!(
            manifest["auth"]["schemes"][0]["scopes"][0],
            "https://www.googleapis.com/auth/gmail.modify"
        );
        assert_eq!(
            manifest["permissions"]["http"]["allow"][0],
            DEFAULT_API_BASE
        );
        assert_eq!(manifest["tools"].as_array().unwrap().len(), 34);
    }
}
