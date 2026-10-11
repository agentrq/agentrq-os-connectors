//! Google Sheets connector: read, write and manage spreadsheets through the
//! Google Sheets API v4. The host runs the OAuth 2.0 sign-in, refreshes the
//! token and adds `Authorization: Bearer â¦` after each request leaves the
//! guest: this code never sees it.

use connectors_sdk::http::{Method, Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::Deserialize;
use serde_json::{Value, json};

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://sheets.googleapis.com";

/// The connector.
#[derive(Debug)]
pub struct Gsheets;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetValues {
    spreadsheet_id: String,
    range: String,
    value_render_option: Option<String>,
    major_dimension: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateValues {
    spreadsheet_id: String,
    range: String,
    values: Vec<Vec<Value>>,
    value_input_option: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppendValues {
    spreadsheet_id: String,
    range: String,
    values: Vec<Vec<Value>>,
    value_input_option: Option<String>,
    insert_data_option: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClearValues {
    spreadsheet_id: String,
    range: String,
}

#[derive(Debug, Deserialize)]
struct CreateSpreadsheet {
    title: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetSpreadsheet {
    spreadsheet_id: String,
    ranges: Option<Vec<String>>,
    include_grid_data: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchUpdate {
    spreadsheet_id: String,
    requests: Vec<Value>,
}

impl Connector for Gsheets {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        let base = host::config("api_base").unwrap_or_else(|| DEFAULT_API_BASE.into());
        answer(&outgoing(&base, &request)?.send()?)
    }
}

connectors_sdk::export_connector!(Gsheets);

/// The request a tool call makes to the API at `base`.
fn outgoing(base: &str, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let api = format!("{}/v4/spreadsheets", base.trim_end_matches('/'));
    let outgoing = match request.tool.as_str() {
        "getValues" => {
            let input: GetValues = request.json()?;
            let mut url = format!(
                "{}/{}/values/{}",
                api,
                encode(&input.spreadsheet_id),
                encode(&input.range)
            );
            let mut query = Vec::new();
            if let Some(option) = input.value_render_option {
                query.push(format!("valueRenderOption={}", encode(&option)));
            }
            if let Some(dimension) = input.major_dimension {
                query.push(format!("majorDimension={}", encode(&dimension)));
            }
            if !query.is_empty() {
                url = format!("{}?{}", url, query.join("&"));
            }
            Request::get(url)
        }
        "updateValues" => {
            let input: UpdateValues = request.json()?;
            let value_input_option = input
                .value_input_option
                .unwrap_or_else(|| "USER_ENTERED".to_string());
            let url = format!(
                "{}/{}/values/{}?valueInputOption={}",
                api,
                encode(&input.spreadsheet_id),
                encode(&input.range),
                encode(&value_input_option)
            );
            Request::new(Method::Put, url).json(&json!({ "values": input.values }))?
        }
        "appendValues" => {
            let input: AppendValues = request.json()?;
            let value_input_option = input
                .value_input_option
                .unwrap_or_else(|| "USER_ENTERED".to_string());
            let mut url = format!(
                "{}/{}/values/{}:append?valueInputOption={}",
                api,
                encode(&input.spreadsheet_id),
                encode(&input.range),
                encode(&value_input_option)
            );
            if let Some(option) = input.insert_data_option {
                url = format!("{}&insertDataOption={}", url, encode(&option));
            }
            Request::post(url).json(&json!({ "values": input.values }))?
        }
        "clearValues" => {
            let input: ClearValues = request.json()?;
            let url = format!(
                "{}/{}/values/{}:clear",
                api,
                encode(&input.spreadsheet_id),
                encode(&input.range)
            );
            Request::post(url).json(&json!({}))?
        }
        "createSpreadsheet" => {
            let input: CreateSpreadsheet = request.json()?;
            Request::post(api).json(&json!({ "properties": { "title": input.title } }))?
        }
        "getSpreadsheet" => {
            let input: GetSpreadsheet = request.json()?;
            let mut url = format!("{}/{}", api, encode(&input.spreadsheet_id));
            let mut query = Vec::new();
            for range in input.ranges.unwrap_or_default() {
                query.push(format!("ranges={}", encode(&range)));
            }
            if let Some(include) = input.include_grid_data {
                query.push(format!("includeGridData={include}"));
            }
            if !query.is_empty() {
                url = format!("{}?{}", url, query.join("&"));
            }
            Request::get(url)
        }
        "batchUpdate" => {
            let input: BatchUpdate = request.json()?;
            let url = format!("{}/{}:batchUpdate", api, encode(&input.spreadsheet_id));
            Request::post(url).json(&json!({ "requests": input.requests }))?
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
        outgoing("https://sheets.test/", &request(tool, body)).unwrap()
    }

    fn response(status: u16, body: &str) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    const API: &str = "https://sheets.test/v4/spreadsheets";

    #[test]
    fn plans_each_tool() {
        assert_eq!(
            planned(
                "getValues",
                r#"{"spreadsheetId":"abc","range":"Sheet1!A1:D10"}"#
            )
            .url,
            format!("{API}/abc/values/Sheet1%21A1%3AD10")
        );
        assert_eq!(
            planned(
                "getValues",
                r#"{"spreadsheetId":"abc","range":"A1:B2","valueRenderOption":"FORMULA","majorDimension":"COLUMNS"}"#
            )
            .url,
            format!("{API}/abc/values/A1%3AB2?valueRenderOption=FORMULA&majorDimension=COLUMNS")
        );
        let updated = planned(
            "updateValues",
            r#"{"spreadsheetId":"abc","range":"A1:B1","values":[["a","b"]]}"#,
        );
        assert_eq!(
            updated.url,
            format!("{API}/abc/values/A1%3AB1?valueInputOption=USER_ENTERED")
        );
        let body: Value = serde_json::from_slice(&updated.body).unwrap();
        assert_eq!(body, json!({ "values": [["a", "b"]] }));

        let appended = planned(
            "appendValues",
            r#"{"spreadsheetId":"abc","range":"Sheet1!A:D","values":[[1]],"insertDataOption":"OVERWRITE"}"#,
        );
        assert_eq!(
            appended.url,
            format!(
                "{API}/abc/values/Sheet1%21A%3AD:append?valueInputOption=USER_ENTERED&insertDataOption=OVERWRITE"
            )
        );

        assert_eq!(
            planned("clearValues", r#"{"spreadsheetId":"abc","range":"A1:D10"}"#).url,
            format!("{API}/abc/values/A1%3AD10:clear")
        );

        let created = planned("createSpreadsheet", r#"{"title":"Budget"}"#);
        assert_eq!(created.url, API.to_string());
        let body: Value = serde_json::from_slice(&created.body).unwrap();
        assert_eq!(body, json!({ "properties": { "title": "Budget" } }));

        assert_eq!(
            planned(
                "getSpreadsheet",
                r#"{"spreadsheetId":"abc","ranges":["Sheet1","Sheet2"],"includeGridData":true}"#
            )
            .url,
            format!("{API}/abc?ranges=Sheet1&ranges=Sheet2&includeGridData=true")
        );

        let batched = planned(
            "batchUpdate",
            r#"{"spreadsheetId":"abc","requests":[{"addSheet":{"properties":{"title":"Q2"}}}]}"#,
        );
        assert_eq!(batched.url, format!("{API}/abc:batchUpdate"));
    }

    #[test]
    fn refuses_bad_input() {
        let missing = outgoing(
            DEFAULT_API_BASE,
            &request("getValues", r#"{"spreadsheetId":"abc"}"#),
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
        let ok = answer(&response(200, r#"{"values":[["a"]]}"#)).unwrap();
        assert_eq!(ok.structured.as_deref(), Some(r#"{"values":[["a"]]}"#));
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
        let sent = Gsheets::call_tool(request("getValues", r#"{"spreadsheetId":"a","range":"b"}"#));
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
        assert_eq!(manifest["tools"].as_array().unwrap().len(), 7);
    }
}
