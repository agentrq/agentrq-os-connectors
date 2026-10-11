//! Google Drive connector: manage files and folders through the Google Drive
//! API v3. The host runs the OAuth 2.0 sign-in, refreshes the token and adds
//! `Authorization: Bearer …` after each request leaves the guest: this code
//! never sees it.

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

/// The connector.
#[derive(Debug)]
pub struct Gdrive;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListFiles {
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
struct CreatePermission {
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
        let base = host::config("api_base").unwrap_or_else(|| DEFAULT_API_BASE.into());
        answer(&outgoing(&base, &request)?.send()?)
    }
}

connectors_sdk::export_connector!(Gdrive);

/// The request a tool call makes to the API at `base`.
fn outgoing(base: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let root = base.trim_end_matches('/');
    let api = format!("{root}/drive/v3/files");
    let outgoing = match request.tool.as_str() {
        "listFiles" => {
            let input: ListFiles = request.json()?;
            Request::get(with_query(
                &api,
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
            ))
        }
        "getFile" => {
            let input: GetFile = request.json()?;
            Request::get(with_query(
                &format!("{api}/{}", encode(&input.file_id)),
                &[
                    ("fields", input.fields),
                    (
                        "supportsAllDrives",
                        input.supports_all_drives.map(|b| b.to_string()),
                    ),
                ],
            ))
        }
        "createFile" => {
            let input: CreateFile = request.json()?;
            let metadata = object(&[
                ("name", Some(json!(input.name))),
                ("mimeType", input.mime_type.as_ref().map(|s| json!(s))),
                ("parents", input.parents.as_ref().map(|p| json!(p))),
                ("description", input.description.as_ref().map(|s| json!(s))),
            ]);
            match upload(
                &metadata,
                &input.content_base64,
                input.content_mime_type.as_deref(),
                input.mime_type.as_deref(),
            )? {
                Some((content_type, body)) => {
                    let mut planned =
                        Request::post(format!("{root}/upload/drive/v3/files?uploadType=multipart"));
                    planned
                        .headers
                        .push(("content-type".to_string(), content_type));
                    planned.body = body;
                    planned
                }
                None => Request::post(&api).json(&metadata)?,
            }
        }
        "updateFile" => {
            let input: UpdateFile = request.json()?;
            let metadata = object(&[
                ("name", input.name.as_ref().map(|s| json!(s))),
                ("description", input.description.as_ref().map(|s| json!(s))),
                ("trashed", input.trashed.map(|b| json!(b))),
            ]);
            let url = with_query(
                &format!("{api}/{}", encode(&input.file_id)),
                &[
                    ("addParents", input.add_parents.map(|p| p.join(","))),
                    ("removeParents", input.remove_parents.map(|p| p.join(","))),
                ],
            );
            match upload(
                &metadata,
                &input.content_base64,
                input.content_mime_type.as_deref(),
                None,
            )? {
                Some((content_type, body)) => {
                    let mut planned = Request::new(
                        Method::Patch,
                        format!(
                            "{root}/upload/drive/v3/files/{}?uploadType=multipart",
                            encode(&input.file_id)
                        ),
                    );
                    planned
                        .headers
                        .push(("content-type".to_string(), content_type));
                    planned.body = body;
                    planned
                }
                None => Request::new(Method::Patch, url).json(&metadata)?,
            }
        }
        "copyFile" => {
            let input: CopyFile = request.json()?;
            let body = object(&[
                ("name", input.name.as_ref().map(|s| json!(s))),
                ("parents", input.parents.as_ref().map(|p| json!(p))),
                ("description", input.description.as_ref().map(|s| json!(s))),
            ]);
            Request::post(format!("{api}/{}/copy", encode(&input.file_id))).json(&body)?
        }
        "deleteFile" => {
            let input: DeleteFile = request.json()?;
            Request::new(Method::Delete, format!("{api}/{}", encode(&input.file_id)))
        }
        "createPermission" => {
            let input: CreatePermission = request.json()?;
            let url = with_query(
                &format!("{api}/{}/permissions", encode(&input.file_id)),
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
            Request::post(url).json(&body)?
        }
        other => return Err(ConnectorError::NotFound(format!("no tool {other}"))),
    };
    Ok(outgoing.header("accept", "application/json"))
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

/// If `content_base64` is present, the decoded content plus its multipart
/// content-type and body: the file `metadata` as JSON followed by the content.
fn upload(
    metadata: &Value,
    content_base64: &Option<String>,
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
    let mut map = serde_json::Map::new();
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

    fn request(tool: &str, body: &str) -> ToolRequest {
        ToolRequest {
            tool: tool.into(),
            body: body.into(),
            ..ToolRequest::default()
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

    const API: &str = "https://www.test/drive/v3/files";

    fn header(request: &Request, name: &str) -> Option<String> {
        request
            .headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    }

    #[test]
    fn plans_list_and_get() {
        assert_eq!(planned("listFiles", "{}").url, API.to_string());
        assert_eq!(
            planned(
                "listFiles",
                r#"{"q":"name contains 'budget' and trashed=false","pageSize":20,"orderBy":"modifiedTime desc","includeItemsFromAllDrives":true,"supportsAllDrives":true}"#
            )
            .url,
            format!(
                "{API}?q=name%20contains%20%27budget%27%20and%20trashed%3Dfalse\
                 &pageSize=20&orderBy=modifiedTime%20desc&includeItemsFromAllDrives=true\
                 &supportsAllDrives=true"
            ),
        );
        assert_eq!(
            planned(
                "getFile",
                r#"{"fileId":"abc","fields":"id,name","supportsAllDrives":true}"#
            )
            .url,
            format!("{API}/abc?fields=id%2Cname&supportsAllDrives=true")
        );
    }

    #[test]
    fn plans_create() {
        let folder = planned(
            "createFile",
            r#"{"name":"Q3 Docs","mimeType":"application/vnd.google-apps.folder","parents":["root"],"description":"Quarterly docs"}"#,
        );
        assert_eq!(folder.url, API.to_string());
        let body: Value = serde_json::from_slice(&folder.body).unwrap();
        assert_eq!(
            body,
            json!({
                "name": "Q3 Docs",
                "mimeType": "application/vnd.google-apps.folder",
                "parents": ["root"],
                "description": "Quarterly docs",
            })
        );

        let uploaded = planned(
            "createFile",
            r#"{"name":"notes.txt","mimeType":"text/plain","contentBase64":"aGVsbG8="}"#,
        );
        assert_eq!(
            uploaded.url,
            "https://www.test/upload/drive/v3/files?uploadType=multipart"
        );
        assert_eq!(
            header(&uploaded, "content-type").as_deref(),
            Some("multipart/related; boundary=\"gdrive-connector-boundary\"")
        );
        let body = String::from_utf8(uploaded.body).unwrap();
        assert!(body.contains("--gdrive-connector-boundary\r\n"));
        assert!(body.contains("Content-Type: application/json; charset=UTF-8"));
        assert!(body.contains(r#""name":"notes.txt""#));
        assert!(body.contains(r#""mimeType":"text/plain""#));
        assert!(body.contains("Content-Type: text/plain\r\n"));
        assert!(body.contains("\r\n\r\naGVsbG8=\r\n"));
        assert!(body.ends_with("--gdrive-connector-boundary--\r\n"));

        let default_mime = planned("createFile", r#"{"name":"blob","contentBase64":"AA=="}"#);
        let body = String::from_utf8(default_mime.body).unwrap();
        assert!(body.contains("Content-Type: application/octet-stream\r\n"));

        let bad = outgoing(
            DEFAULT_API_BASE,
            &request("createFile", r#"{"name":"x","contentBase64":"!!!"}"#),
        );
        assert!(
            matches!(bad, Err(ConnectorError::InvalidInput(ref m)) if m.contains("not valid base64")),
            "{bad:?}"
        );
    }

    #[test]
    fn plans_update() {
        let renamed = planned(
            "updateFile",
            r#"{"fileId":"abc","name":"Q3 Budget","description":"The budget","trashed":true}"#,
        );
        assert_eq!(renamed.method, Method::Patch);
        assert_eq!(renamed.url, format!("{API}/abc"));
        let body: Value = serde_json::from_slice(&renamed.body).unwrap();
        assert_eq!(
            body,
            json!({ "name": "Q3 Budget", "description": "The budget", "trashed": true })
        );

        let moved = planned(
            "updateFile",
            r#"{"fileId":"abc","addParents":["def"],"removeParents":["ghi"]}"#,
        );
        assert_eq!(
            moved.url,
            format!("{API}/abc?addParents=def&removeParents=ghi")
        );

        let replaced = planned(
            "updateFile",
            r#"{"fileId":"a b","contentBase64":"aGVsbG8=","contentMimeType":"text/csv"}"#,
        );
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

        let bad = outgoing(
            DEFAULT_API_BASE,
            &request("updateFile", r#"{"fileId":"abc","contentBase64":"!!!"}"#),
        );
        assert!(
            matches!(bad, Err(ConnectorError::InvalidInput(ref m)) if m.contains("not valid base64")),
            "{bad:?}"
        );
    }

    #[test]
    fn plans_copy_delete_and_share() {
        let copied = planned(
            "copyFile",
            r#"{"fileId":"abc","name":"Copy of Q3 Budget","parents":["root"],"description":"A copy"}"#,
        );
        assert_eq!(copied.url, format!("{API}/abc/copy"));
        let body: Value = serde_json::from_slice(&copied.body).unwrap();
        assert_eq!(
            body,
            json!({ "name": "Copy of Q3 Budget", "parents": ["root"], "description": "A copy" })
        );

        let deleted = planned("deleteFile", r#"{"fileId":"abc"}"#);
        assert_eq!(deleted.method, Method::Delete);
        assert_eq!(deleted.url, format!("{API}/abc"));

        let shared = planned(
            "createPermission",
            r#"{"fileId":"abc","type":"user","role":"reader","emailAddress":"alex@example.com","sendNotificationEmail":false,"emailMessage":"FYI"}"#,
        );
        assert_eq!(
            shared.url,
            format!("{API}/abc/permissions?sendNotificationEmail=false")
        );
        let body: Value = serde_json::from_slice(&shared.body).unwrap();
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
    fn refuses_bad_input() {
        let missing = outgoing(DEFAULT_API_BASE, &request("getFile", "{}"));
        assert!(
            matches!(missing, Err(ConnectorError::InvalidInput(_))),
            "{missing:?}"
        );
        let unknown = outgoing(DEFAULT_API_BASE, &request("nope", "{}"));
        assert_eq!(
            unknown.unwrap_err(),
            ConnectorError::NotFound("no tool nope".into())
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
    fn off_wasm_nothing_is_sent() {
        let sent = Gdrive::call_tool(request("listFiles", "{}"));
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
        assert_eq!(manifest["tools"].as_array().unwrap().len(), 7);
    }
}
