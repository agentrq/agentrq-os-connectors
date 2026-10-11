//! Google Calendar connector: list, read, create, update and delete events
//! through the Google Calendar API v3. The host runs the OAuth 2.0 sign-in,
//! refreshes the token and adds `Authorization: Bearer â¦` after each request
//! leaves the guest: this code never sees it.

use connectors_sdk::http::{Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::Deserialize;
use serde_json::{Value, json};

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://www.googleapis.com";

/// The connector.
#[derive(Debug)]
pub struct Gcal;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListEvents {
    calendar_id: String,
    time_min: Option<String>,
    time_max: Option<String>,
    q: Option<String>,
    max_results: Option<i64>,
    single_events: Option<bool>,
    order_by: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetEvent {
    calendar_id: String,
    event_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateEvent {
    calendar_id: String,
    event: Value,
    send_updates: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateEvent {
    calendar_id: String,
    event_id: String,
    event: Value,
    send_updates: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteEvent {
    calendar_id: String,
    event_id: String,
    send_updates: Option<String>,
}

impl Connector for Gcal {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        let base = host::config("api_base").unwrap_or_else(|| DEFAULT_API_BASE.into());
        answer(&outgoing(&base, &request)?.send()?)
    }
}

connectors_sdk::export_connector!(Gcal);

/// The request a tool call makes to the API at `base`.
fn outgoing(base: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let api = format!("{}/calendar/v3/calendars", base.trim_end_matches('/'));
    let outgoing = match request.tool.as_str() {
        "listEvents" => {
            let input: ListEvents = request.json()?;
            let mut url = format!("{}/{}/events", api, encode(&input.calendar_id));
            let mut query = Vec::new();
            if let Some(v) = input.time_min {
                query.push(format!("timeMin={}", encode(&v)));
            }
            if let Some(v) = input.time_max {
                query.push(format!("timeMax={}", encode(&v)));
            }
            if let Some(v) = input.q {
                query.push(format!("q={}", encode(&v)));
            }
            if let Some(v) = input.max_results {
                query.push(format!("maxResults={v}"));
            }
            if let Some(v) = input.single_events {
                query.push(format!("singleEvents={v}"));
            }
            if let Some(v) = input.order_by {
                query.push(format!("orderBy={}", encode(&v)));
            }
            if !query.is_empty() {
                url = format!("{}?{}", url, query.join("&"));
            }
            Request::get(url)
        }
        "getEvent" => {
            let input: GetEvent = request.json()?;
            Request::get(format!(
                "{}/{}/events/{}",
                api,
                encode(&input.calendar_id),
                encode(&input.event_id)
            ))
        }
        "createEvent" => {
            let input: CreateEvent = request.json()?;
            let mut url = format!("{}/{}/events", api, encode(&input.calendar_id));
            if let Some(v) = input.send_updates {
                url = format!("{}?sendUpdates={}", url, encode(&v));
            }
            Request::post(url).json(&input.event)?
        }
        "updateEvent" => {
            let input: UpdateEvent = request.json()?;
            let mut url = format!(
                "{}/{}/events/{}",
                api,
                encode(&input.calendar_id),
                encode(&input.event_id)
            );
            if let Some(v) = input.send_updates {
                url = format!("{}?sendUpdates={}", url, encode(&v));
            }
            Request::new(connectors_sdk::http::Method::Patch, url).json(&input.event)?
        }
        "deleteEvent" => {
            let input: DeleteEvent = request.json()?;
            let mut url = format!(
                "{}/{}/events/{}",
                api,
                encode(&input.calendar_id),
                encode(&input.event_id)
            );
            if let Some(v) = input.send_updates {
                url = format!("{}?sendUpdates={}", url, encode(&v));
            }
            Request::new(connectors_sdk::http::Method::Delete, url)
        }
        other => return Err(ConnectorError::NotFound(format!("no tool {other}"))),
    };
    Ok(outgoing.header("accept", "application/json"))
}

/// The tool result of the API's response: its JSON (204 for deletes), or a
/// tool error that says what the API answered, so the model can react (a
/// 403 usually means a missing scope).
fn answer(response: &Response) -> Result<ToolResponse, ConnectorError> {
    if response.status == 204 {
        return ToolResponse::structured(&json!({ "deleted": true }));
    }
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
        outgoing("https://www.test/", &request(tool, body)).unwrap()
    }

    fn response(status: u16, body: &str) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    const API: &str = "https://www.test/calendar/v3/calendars";

    #[test]
    fn plans_each_tool() {
        assert_eq!(
            planned("listEvents", r#"{"calendarId":"primary"}"#).url,
            format!("{API}/primary/events")
        );
        assert_eq!(
            planned(
                "listEvents",
                r#"{"calendarId":"primary","timeMin":"2026-10-11T00:00:00-07:00","timeMax":"2026-10-18T00:00:00-07:00","q":"standup","maxResults":25,"singleEvents":true,"orderBy":"startTime"}"#
            )
            .url,
            format!("{API}/primary/events?timeMin=2026-10-11T00%3A00%3A00-07%3A00&timeMax=2026-10-18T00%3A00%3A00-07%3A00&q=standup&maxResults=25&singleEvents=true&orderBy=startTime")
        );

        assert_eq!(
            planned("getEvent", r#"{"calendarId":"primary","eventId":"e1"}"#).url,
            format!("{API}/primary/events/e1")
        );

        let created = planned(
            "createEvent",
            r#"{"calendarId":"primary","event":{"summary":"Focus","start":{"dateTime":"2026-10-12T14:00:00-07:00"},"end":{"dateTime":"2026-10-12T15:00:00-07:00"}},"sendUpdates":"none"}"#,
        );
        assert_eq!(
            created.url,
            format!("{API}/primary/events?sendUpdates=none")
        );
        let body: Value = serde_json::from_slice(&created.body).unwrap();
        assert_eq!(body["summary"], json!("Focus"));

        let updated = planned(
            "updateEvent",
            r#"{"calendarId":"primary","eventId":"e1","event":{"location":"Room A"},"sendUpdates":"all"}"#,
        );
        assert_eq!(
            updated.url,
            format!("{API}/primary/events/e1?sendUpdates=all")
        );
        assert_eq!(updated.method, connectors_sdk::http::Method::Patch);
        let body: Value = serde_json::from_slice(&updated.body).unwrap();
        assert_eq!(body["location"], json!("Room A"));

        let deleted = planned(
            "deleteEvent",
            r#"{"calendarId":"primary","eventId":"e1","sendUpdates":"none"}"#,
        );
        assert_eq!(
            deleted.url,
            format!("{API}/primary/events/e1?sendUpdates=none")
        );
    }

    #[test]
    fn refuses_bad_input() {
        let missing = outgoing(
            DEFAULT_API_BASE,
            &request("getEvent", r#"{"calendarId":"primary"}"#),
        );
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
        let ok = answer(&response(200, r#"{"summary":"Focus"}"#)).unwrap();
        assert_eq!(ok.structured.as_deref(), Some(r#"{"summary":"Focus"}"#));
        let gone = answer(&response(204, "")).unwrap();
        assert_eq!(gone.structured.as_deref(), Some(r#"{"deleted":true}"#));
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
        let sent = Gcal::call_tool(request("listEvents", r#"{"calendarId":"primary"}"#));
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
        assert_eq!(manifest["tools"].as_array().unwrap().len(), 5);
    }
}
