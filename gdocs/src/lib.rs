//! Google Docs connector: read, create and edit documents through the
//! Google Docs API v1. The host runs the OAuth 2.0 sign-in, refreshes the
//! token and adds `Authorization: Bearer â¦` after each request leaves the
//! guest: this code never sees it.

use connectors_sdk::http::{Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::Deserialize;
use serde_json::{Value, json};

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://docs.googleapis.com";

/// The connector.
#[derive(Debug)]
pub struct Gdocs;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetDocument {
    document_id: String,
    suggestions_view_mode: Option<String>,
    include_tabs_content: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CreateDocument {
    title: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchUpdate {
    document_id: String,
    requests: Vec<Value>,
    write_control: Option<Value>,
}

impl Connector for Gdocs {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        let base = host::config("api_base").unwrap_or_else(|| DEFAULT_API_BASE.into());
        answer(&outgoing(&base, &request)?.send()?)
    }
}

connectors_sdk::export_connector!(Gdocs);

/// The request a tool call makes to the API at `base`.
fn outgoing(base: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let api = format!("{}/v1/documents", base.trim_end_matches('/'));
    let outgoing = match request.tool.as_str() {
        "getDocument" => {
            let input: GetDocument = request.json()?;
            Request::get(with_query(
                &format!("{}/{}", api, encode(&input.document_id)),
                &[
                    ("suggestionsViewMode", input.suggestions_view_mode),
                    (
                        "includeTabsContent",
                        input
                            .include_tabs_content
                            .map(|include| include.to_string()),
                    ),
                ],
            ))
        }
        "createDocument" => {
            let input: CreateDocument = request.json()?;
            Request::post(api).json(&json!({ "title": input.title }))?
        }
        "batchUpdate" => {
            let input: BatchUpdate = request.json()?;
            let url = format!("{}/{}:batchUpdate", api, encode(&input.document_id));
            let mut body = json!({ "requests": input.requests });
            if let Some(write_control) = input.write_control {
                body["writeControl"] = write_control;
            }
            Request::post(url).json(&body)?
        }
        other => return Err(ConnectorError::NotFound(format!("no tool {other}"))),
    };
    Ok(outgoing.header("accept", "application/json"))
}

/// The tool result of the API's response: its JSON, or a tool error that
/// says what the API answered, so the model can react (a 403 usually means
/// a missing scope).
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

    fn request(tool: &str, body: &str) -> ToolRequest {
        ToolRequest {
            tool: tool.into(),
            body: body.into(),
        }
    }

    fn planned(tool: &str, body: &str) -> Request {
        outgoing("https://docs.test/", &request(tool, body)).unwrap()
    }

    fn response(status: u16, body: &str) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    const API: &str = "https://docs.test/v1/documents";

    #[test]
    fn plans_each_tool() {
        assert_eq!(
            planned("getDocument", r#"{"documentId":"abc"}"#).url,
            format!("{API}/abc")
        );
        assert_eq!(
            planned("getDocument", r#"{"documentId":"a/b c"}"#).url,
            format!("{API}/a%2Fb%20c")
        );
        assert_eq!(
            planned(
                "getDocument",
                r#"{"documentId":"abc","suggestionsViewMode":"PREVIEW_WITHOUT_SUGGESTIONS","includeTabsContent":true}"#
            )
            .url,
            format!(
                "{API}/abc?suggestionsViewMode=PREVIEW_WITHOUT_SUGGESTIONS&includeTabsContent=true"
            )
        );

        let created = planned("createDocument", r#"{"title":"Brief"}"#);
        assert_eq!(created.url, API.to_string());
        let body: Value = serde_json::from_slice(&created.body).unwrap();
        assert_eq!(body, json!({ "title": "Brief" }));

        let edited = planned(
            "batchUpdate",
            r#"{"documentId":"abc","requests":[{"insertText":{"location":{"index":1},"text":"Hi"}}]}"#,
        );
        assert_eq!(edited.url, format!("{API}/abc:batchUpdate"));
        let body: Value = serde_json::from_slice(&edited.body).unwrap();
        assert_eq!(
            body["requests"],
            json!([{"insertText":{"location":{"index":1},"text":"Hi"}}])
        );
        assert!(body.get("writeControl").is_none());

        let guarded = planned(
            "batchUpdate",
            r#"{"documentId":"abc","requests":[],"writeControl":{"requiredRevisionId":"r1"}}"#,
        );
        let body: Value = serde_json::from_slice(&guarded.body).unwrap();
        assert_eq!(body["writeControl"], json!({ "requiredRevisionId": "r1" }));
    }

    #[test]
    fn refuses_bad_input() {
        let missing = outgoing(DEFAULT_API_BASE, &request("getDocument", "{}"));
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
        let ok = answer(&response(200, r#"{"title":"Brief"}"#)).unwrap();
        assert_eq!(ok.structured.as_deref(), Some(r#"{"title":"Brief"}"#));
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
        let sent = Gdocs::call_tool(request("getDocument", r#"{"documentId":"a"}"#));
        assert!(matches!(&sent, Err(ConnectorError::Upstream(m)) if m.contains("wasm32-wasip2")));
    }

    #[test]
    fn manifest_is_embedded() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["auth"]["schemes"][0]["type"], "oauth2");
        assert_eq!(
            manifest["permissions"]["http"]["allow"][0],
            DEFAULT_API_BASE
        );
        assert_eq!(manifest["tools"].as_array().unwrap().len(), 3);
    }
}
