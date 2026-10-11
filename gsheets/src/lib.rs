//! Google Sheets connector: task-shaped tools for the Sheets API v4, not a
//! 1:1 endpoint mapping.
//!
//! The tools are shaped around what an agent wants to accomplish. `readTable`
//! returns rows as objects with the first row as headers instead of raw
//! nested arrays; `createSpreadsheet` creates the spreadsheet *and* writes
//! the initial rows in one call instead of pushing that choreography onto
//! the agent. A tool may therefore make several HTTP calls to finish its
//! job; tests inject a stub sender so the whole orchestration is covered
//! without a network.
//!
//! The host runs the OAuth 2.0 sign-in, refreshes the token and adds
//! `Authorization: Bearer …` after each request leaves the guest: this code
//! never sees it.

use connectors_sdk::http::{Method, Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::Deserialize;
use serde_json::{Value, json};

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://sheets.googleapis.com";

/// How values are interpreted when the caller does not say.
const DEFAULT_VALUE_INPUT_OPTION: &str = "USER_ENTERED";

/// The connector.
#[derive(Debug)]
pub struct Gsheets;

// ---------------------------------------------------------------------------
// Tool inputs. Field names are the API's camelCase parameter names.
// ---------------------------------------------------------------------------

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
#[serde(rename_all = "camelCase")]
struct NewSheet {
    title: String,
    rows: Option<Vec<Vec<Value>>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSpreadsheet {
    title: String,
    sheets: Option<Vec<NewSheet>>,
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
        Self::call_with(request, |request| request.send())
    }
}

impl Gsheets {
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
            "getValues" => one_call(&api, &request, &mut send, plan_get_values),
            "readTable" => read_table(&api, &request, &mut send),
            "updateValues" => one_call(&api, &request, &mut send, plan_update_values),
            "appendValues" => one_call(&api, &request, &mut send, plan_append_values),
            "clearValues" => one_call(&api, &request, &mut send, plan_clear_values),
            "createSpreadsheet" => create_spreadsheet(&api, &request, &mut send),
            "getSpreadsheet" => one_call(&api, &request, &mut send, plan_get_spreadsheet),
            "batchUpdate" => one_call(&api, &request, &mut send, plan_batch_update),
            other => Err(ConnectorError::NotFound(format!("unknown tool: {other}"))),
        }
    }
}

connectors_sdk::export_connector!(Gsheets);

/// The Sheets API endpoints derived from the configured base URL.
struct Api {
    spreadsheets: String,
}

impl Api {
    fn new(base: &str) -> Self {
        let root = base.trim_end_matches('/').to_string();
        Self {
            spreadsheets: format!("{root}/v4/spreadsheets"),
        }
    }

    /// The URL of one spreadsheet's resource.
    fn spreadsheet(&self, spreadsheet_id: &str) -> String {
        format!("{}/{}", self.spreadsheets, encode(spreadsheet_id))
    }

    /// The URL of a range's values resource.
    fn values(&self, spreadsheet_id: &str, range: &str) -> String {
        format!(
            "{}/values/{}",
            self.spreadsheet(spreadsheet_id),
            encode(range)
        )
    }
}

// ---------------------------------------------------------------------------
// Tool runners
// ---------------------------------------------------------------------------

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

/// Runs `readTable`: one `values.get` call, shaped into headers plus row
/// objects instead of the API's nested arrays.
fn read_table(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let planned = plan_get_values(api, request)?;
    let response = send(&planned)?;
    if let Err(error) = step_ok(&response, "reading the table") {
        return Ok(error);
    }
    combine_read_table(&response)
}

/// Runs `createSpreadsheet`: creates the spreadsheet with its sheets, then
/// writes each sheet's initial rows top-left — what used to be 1+N agent
/// calls in one tool call.
fn create_spreadsheet(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: CreateSpreadsheet = request.json()?;

    let created = send(&plan_create_spreadsheet(api, &input)?)?;
    if let Err(error) = step_ok(&created, "creating the spreadsheet") {
        return Ok(error);
    }
    let created: Value = created.json()?;
    let id = spreadsheet_id_of(&created)?;

    for sheet in input.sheets.iter().flatten() {
        let Some(rows) = sheet.rows.as_deref() else {
            continue;
        };
        if rows.is_empty() {
            continue;
        }
        let filled = send(&plan_values_update(
            api,
            &id,
            &a1_sheet_range(&sheet.title),
            rows,
            None,
        )?)?;
        if let Err(error) = step_ok(&filled, &format!("writing rows to sheet {:?}", sheet.title)) {
            return Ok(error);
        }
    }

    let requested: Vec<String> = input
        .sheets
        .iter()
        .flatten()
        .map(|sheet| sheet.title.clone())
        .collect();
    combine_created_spreadsheet(&id, &created, &requested)
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

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

/// A GET request that expects a JSON answer.
fn json_get(url: String) -> Request {
    Request::get(url).header("accept", "application/json")
}

/// Plans `getValues` — and `readTable`, which is the same fetch with a
/// shaped answer.
fn plan_get_values(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: GetValues = request.json()?;
    Ok(json_get(with_query(
        &api.values(&input.spreadsheet_id, &input.range),
        &[
            ("valueRenderOption", input.value_render_option),
            ("majorDimension", input.major_dimension),
        ],
    )))
}

/// Plans `updateValues`.
fn plan_update_values(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: UpdateValues = request.json()?;
    plan_values_update(
        api,
        &input.spreadsheet_id,
        &input.range,
        &input.values,
        input.value_input_option,
    )
}

/// Plans a `values.update` call; shared by `updateValues` and the per-sheet
/// fills inside `createSpreadsheet`.
fn plan_values_update(
    api: &Api,
    spreadsheet_id: &str,
    range: &str,
    values: &[Vec<Value>],
    value_input_option: Option<String>,
) -> Result<Request, ConnectorError> {
    let option = value_input_option.unwrap_or_else(|| DEFAULT_VALUE_INPUT_OPTION.to_string());
    let url = format!(
        "{}?valueInputOption={}",
        api.values(spreadsheet_id, range),
        encode(&option)
    );
    Request::new(Method::Put, url)
        .header("accept", "application/json")
        .json(&json!({ "values": values }))
}

/// Plans `appendValues`.
fn plan_append_values(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: AppendValues = request.json()?;
    let url = with_query(
        &format!("{}:append", api.values(&input.spreadsheet_id, &input.range)),
        &[
            (
                "valueInputOption",
                Some(
                    input
                        .value_input_option
                        .unwrap_or_else(|| DEFAULT_VALUE_INPUT_OPTION.to_string()),
                ),
            ),
            ("insertDataOption", input.insert_data_option),
        ],
    );
    Request::post(url)
        .header("accept", "application/json")
        .json(&json!({ "values": input.values }))
}

/// Plans `clearValues`.
fn plan_clear_values(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: ClearValues = request.json()?;
    let url = format!("{}:clear", api.values(&input.spreadsheet_id, &input.range));
    Request::post(url)
        .header("accept", "application/json")
        .json(&json!({}))
}

/// Plans `createSpreadsheet`: the spreadsheet plus a sheet per requested
/// title; the rows go in afterwards, one `values.update` each.
fn plan_create_spreadsheet(
    api: &Api,
    input: &CreateSpreadsheet,
) -> Result<Request, ConnectorError> {
    let sheets: Vec<Value> = input
        .sheets
        .iter()
        .flatten()
        .map(|sheet| json!({ "properties": { "title": &sheet.title } }))
        .collect();
    let mut body = json!({ "properties": { "title": &input.title } });
    if !sheets.is_empty() {
        body["sheets"] = Value::Array(sheets);
    }
    Request::post(&api.spreadsheets)
        .header("accept", "application/json")
        .json(&body)
}

/// Plans `getSpreadsheet`.
fn plan_get_spreadsheet(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: GetSpreadsheet = request.json()?;
    let mut params: Vec<(&str, Option<String>)> = input
        .ranges
        .unwrap_or_default()
        .into_iter()
        .map(|range| ("ranges", Some(range)))
        .collect();
    if let Some(include) = input.include_grid_data {
        params.push(("includeGridData", Some(include.to_string())));
    }
    Ok(json_get(with_query(
        &api.spreadsheet(&input.spreadsheet_id),
        &params,
    )))
}

/// Plans `batchUpdate`.
fn plan_batch_update(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: BatchUpdate = request.json()?;
    let url = format!("{}:batchUpdate", api.spreadsheet(&input.spreadsheet_id));
    Request::post(url)
        .header("accept", "application/json")
        .json(&json!({ "requests": input.requests }))
}

// ---------------------------------------------------------------------------
// Parsing and combining
// ---------------------------------------------------------------------------

/// Combines `readTable`'s `values.get` answer into the tool's answer.
fn combine_read_table(response: &Response) -> Result<ToolResponse, ConnectorError> {
    let body: Value = response.json()?;
    ToolResponse::structured(&table_of(&body))
}

/// Shapes a `values.get` answer into `{headers, rows, rowCount}`: the first
/// row becomes the column names, each later row becomes one object keyed by
/// them. Rows shorter than the header row leave keys out; cells under an
/// empty header are dropped rather than keyed by `""`.
fn table_of(body: &Value) -> Value {
    let mut values = body
        .get("values")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter();
    let Some(header_row) = values.next() else {
        return json!({ "headers": [], "rows": [], "rowCount": 0 });
    };
    let headers: Vec<String> = header_row
        .as_array()
        .map(|cells| cells.iter().map(cell_text).collect())
        .unwrap_or_default();
    let mut rows: Vec<Value> = Vec::new();
    for row in values {
        let cells = row.as_array().cloned().unwrap_or_default();
        let mut object = serde_json::Map::with_capacity(headers.len());
        for (header, cell) in headers.iter().zip(cells.iter()) {
            if !header.is_empty() {
                object.insert(header.clone(), cell.clone());
            }
        }
        rows.push(Value::Object(object));
    }
    json!({
        "headers": headers,
        "rows": rows,
        "rowCount": rows.len(),
    })
}

/// A values cell as a header name: text as-is, numbers and booleans as they
/// print, nothing for empty cells.
fn cell_text(cell: &Value) -> String {
    match cell {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The new spreadsheet's ID from the `spreadsheets.create` answer.
fn spreadsheet_id_of(created: &Value) -> Result<String, ConnectorError> {
    created
        .get("spreadsheetId")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| ConnectorError::Upstream("the create answer had no spreadsheetId".into()))
}

/// The created sheets' titles: the API's answer when it lists them, the
/// requested titles when it does not.
fn sheet_titles(created: &Value, requested: &[String]) -> Vec<String> {
    created
        .get("sheets")
        .and_then(Value::as_array)
        .map(|sheets| {
            sheets
                .iter()
                .filter_map(|sheet| {
                    sheet
                        .get("properties")
                        .and_then(|properties| properties.get("title"))
                        .and_then(Value::as_str)
                })
                .map(String::from)
                .collect()
        })
        .filter(|titles: &Vec<String>| !titles.is_empty())
        .unwrap_or_else(|| requested.to_vec())
}

/// Combines `createSpreadsheet`'s create answer into the tool's answer.
fn combine_created_spreadsheet(
    id: &str,
    created: &Value,
    requested: &[String],
) -> Result<ToolResponse, ConnectorError> {
    let sheets: Vec<Value> = sheet_titles(created, requested)
        .into_iter()
        .map(|title| json!({ "title": title }))
        .collect();
    ToolResponse::structured(&json!({
        "spreadsheetId": id,
        "spreadsheetUrl": format!("https://docs.google.com/spreadsheets/d/{id}"),
        "sheets": sheets,
    }))
}

/// The A1 range covering a whole sheet by title, starting at the top-left
/// cell. Titles with characters outside `[A-Za-z0-9_]` are single-quoted,
/// with embedded quotes doubled, as A1 notation requires.
fn a1_sheet_range(title: &str) -> String {
    if title
        .chars()
        .all(|char| char.is_ascii_alphanumeric() || char == '_')
    {
        format!("{title}!A1")
    } else {
        format!("'{}'!A1", title.replace('\'', "''"))
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

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
    use std::collections::VecDeque;

    /// The default base, which the shimmed `host::config` leaves unset under
    /// `cargo test`, so `call_with` plans against it.
    const API: &str = "https://sheets.googleapis.com/v4/spreadsheets";

    fn api() -> Api {
        Api::new("https://sheets.test/")
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
        Gsheets::call_with(request(tool, body), |request| stub.send(request))
    }

    fn header(request: &Request, name: &str) -> Option<String> {
        request
            .headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.clone())
    }

    fn structured(response: &ToolResponse) -> &str {
        response.structured.as_deref().unwrap()
    }

    fn body_of(request: &Request) -> Value {
        serde_json::from_slice(&request.body).unwrap()
    }

    #[test]
    fn plans_get_values() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"values":[["a"]]}"#)]);

        // Act.
        let answered = call(
            "getValues",
            r#"{"spreadsheetId":"abc","range":"Sheet1!A1:D10","valueRenderOption":"FORMULA","majorDimension":"COLUMNS"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(structured(&answered), r#"{"values":[["a"]]}"#);
        assert_eq!(stub.seen.len(), 1);
        assert_eq!(stub.seen[0].method, Method::Get);
        assert_eq!(
            stub.seen[0].url,
            format!(
                "{API}/abc/values/Sheet1%21A1%3AD10?valueRenderOption=FORMULA&majorDimension=COLUMNS"
            )
        );
        assert_eq!(
            header(&stub.seen[0], "accept"),
            Some("application/json".to_string())
        );
    }

    #[test]
    fn plans_get_values_without_options() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json("{}")]);

        // Act.
        call(
            "getValues",
            r#"{"spreadsheetId":"abc","range":"A1:B2"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: no query string at all.
        assert_eq!(stub.seen[0].url, format!("{API}/abc/values/A1%3AB2"));
    }

    #[test]
    fn reads_table_as_headers_and_row_objects() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(
            r#"{"range":"Sheet1!A1:B3","values":[["name","age"],["ada",36],["bob"]]}"#,
        )]);

        // Act.
        let answered = call(
            "readTable",
            r#"{"spreadsheetId":"abc","range":"Sheet1!A1:B"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            structured(&answered),
            r#"{"headers":["name","age"],"rows":[{"name":"ada","age":36},{"name":"bob"}],"rowCount":2}"#
        );
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/abc/values/Sheet1%21A1%3AB")
        );
    }

    #[test]
    fn reads_table_with_render_options() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"values":[["a"]]}"#)]);

        // Act.
        call(
            "readTable",
            r#"{"spreadsheetId":"abc","range":"A1:B2","valueRenderOption":"UNFORMATTED_VALUE"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/abc/values/A1%3AB2?valueRenderOption=UNFORMATTED_VALUE")
        );
    }

    #[test]
    fn reads_empty_table() {
        // Arrange: no values key at all.
        let mut stub = Stub::of(&[ok_json("{}")]);

        // Act.
        let answered = call(
            "readTable",
            r#"{"spreadsheetId":"abc","range":"A1:B"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            structured(&answered),
            r#"{"headers":[],"rows":[],"rowCount":0}"#
        );
    }

    #[test]
    fn reads_headerless_table() {
        // Arrange: a values array with no rows.
        let mut stub = Stub::of(&[ok_json(r#"{"values":[]}"#)]);

        // Act.
        let answered = call(
            "readTable",
            r#"{"spreadsheetId":"abc","range":"A1:B"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            structured(&answered),
            r#"{"headers":[],"rows":[],"rowCount":0}"#
        );
    }

    #[test]
    fn shapes_ragged_table_rows() {
        // Arrange: a numeric header, an empty (null) header cell, a short row
        // and a long row.
        let mut stub = Stub::of(&[ok_json(
            r#"{"values":[[2024,null,true],["a"],["b","c","d"],["e","f"]]}"#,
        )]);

        // Act.
        let answered = call(
            "readTable",
            r#"{"spreadsheetId":"abc","range":"A1:C"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the long row's extra cell and the empty header's column are
        // dropped; missing cells leave their keys out.
        assert_eq!(
            structured(&answered),
            r#"{"headers":["2024","","true"],"rows":[{"2024":"a"},{"2024":"b","true":"d"},{"2024":"e"}],"rowCount":3}"#
        );
    }

    #[test]
    fn shapes_table_with_non_array_rows() {
        // Arrange: header and data rows that are not arrays.
        let body = json!({ "values": ["oops", 42] });

        // Act.
        let table = table_of(&body);

        // Assert: no headers and empty row objects.
        assert_eq!(table, json!({"headers": [], "rows": [{}], "rowCount": 1}));
    }

    #[test]
    fn reads_table_through_plan_get_values() {
        // The fetch `readTable` makes is exactly `getValues`' fetch.
        let get = plan_get_values(
            &api(),
            &request("getValues", r#"{"spreadsheetId":"a","range":"b"}"#),
        )
        .unwrap();
        let table = plan_get_values(
            &api(),
            &request("readTable", r#"{"spreadsheetId":"a","range":"b"}"#),
        )
        .unwrap();
        assert_eq!(get.url, table.url);
        assert_eq!(get.method, table.method);
    }

    #[test]
    fn read_table_reports_a_failed_fetch() {
        // Arrange.
        let mut stub = Stub::of(&[response(403, "insufficient scope")]);

        // Act.
        let answered = call(
            "readTable",
            r#"{"spreadsheetId":"abc","range":"A:B"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: still a tool answer, not a call error.
        assert_eq!(answered.structured, None);
    }

    #[test]
    fn updates_values_in_place() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"updatedCells":2}"#)]);

        // Act.
        let answered = call(
            "updateValues",
            r#"{"spreadsheetId":"abc","range":"A1:B1","values":[["a","b"]],"valueInputOption":"RAW"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(structured(&answered), r#"{"updatedCells":2}"#);
        assert_eq!(stub.seen[0].method, Method::Put);
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/abc/values/A1%3AB1?valueInputOption=RAW")
        );
        assert_eq!(body_of(&stub.seen[0]), json!({ "values": [["a", "b"]] }));
    }

    #[test]
    fn updates_values_with_the_default_input_option() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json("{}")]);

        // Act.
        call(
            "updateValues",
            r#"{"spreadsheetId":"abc","range":"A1:B1","values":[[1]]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/abc/values/A1%3AB1?valueInputOption=USER_ENTERED")
        );
    }

    #[test]
    fn appends_rows() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"updates":{}}"#)]);

        // Act.
        call(
            "appendValues",
            r#"{"spreadsheetId":"abc","range":"Sheet1!A:D","values":[[1]],"insertDataOption":"OVERWRITE"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(stub.seen[0].method, Method::Post);
        assert_eq!(
            stub.seen[0].url,
            format!(
                "{API}/abc/values/Sheet1%21A%3AD:append?valueInputOption=USER_ENTERED&insertDataOption=OVERWRITE"
            )
        );
        assert_eq!(body_of(&stub.seen[0]), json!({ "values": [[1]] }));
    }

    #[test]
    fn clears_a_range() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"clearedRange":"Sheet1!A1:D10"}"#)]);

        // Act.
        let answered = call(
            "clearValues",
            r#"{"spreadsheetId":"abc","range":"A1:D10"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(structured(&answered), r#"{"clearedRange":"Sheet1!A1:D10"}"#);
        assert_eq!(stub.seen[0].method, Method::Post);
        assert_eq!(stub.seen[0].url, format!("{API}/abc/values/A1%3AD10:clear"));
        assert_eq!(body_of(&stub.seen[0]), json!({}));
    }

    #[test]
    fn creates_a_spreadsheet_with_sheets_and_rows() {
        // Arrange: create, then one values.update per sheet that has rows.
        let mut stub = Stub::of(&[
            ok_json(
                r#"{"spreadsheetId":"new-id","sheets":[{"properties":{"title":"Expenses"}},{"properties":{"title":"Notes"}}]}"#,
            ),
            ok_json(r#"{"updatedCells":4}"#),
        ]);

        // Act.
        let answered = call(
            "createSpreadsheet",
            r#"{"title":"Budget","sheets":[{"title":"Expenses","rows":[["item","cost"],["tea",2]]},{"title":"Notes"}]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the create carries the sheet titles.
        assert_eq!(stub.seen.len(), 2);
        assert_eq!(stub.seen[0].method, Method::Post);
        assert_eq!(stub.seen[0].url, API);
        assert_eq!(
            body_of(&stub.seen[0]),
            json!({
                "properties": { "title": "Budget" },
                "sheets": [
                    { "properties": { "title": "Expenses" } },
                    { "properties": { "title": "Notes" } },
                ],
            })
        );

        // Assert: only the sheet with rows gets a values.update, top-left.
        assert_eq!(stub.seen[1].method, Method::Put);
        assert_eq!(
            stub.seen[1].url,
            format!("{API}/new-id/values/Expenses%21A1?valueInputOption=USER_ENTERED")
        );
        assert_eq!(
            body_of(&stub.seen[1]),
            json!({ "values": [["item", "cost"], ["tea", 2]] })
        );

        // Assert: the tool answers with the ID, URL and sheet titles.
        assert_eq!(
            structured(&answered),
            r#"{"spreadsheetId":"new-id","spreadsheetUrl":"https://docs.google.com/spreadsheets/d/new-id","sheets":[{"title":"Expenses"},{"title":"Notes"}]}"#
        );
    }

    #[test]
    fn quotes_sheet_titles_when_filling_rows() {
        // Arrange.
        let mut stub = Stub::of(&[
            ok_json(r#"{"spreadsheetId":"new-id"}"#),
            ok_json("{}"),
            ok_json("{}"),
        ]);

        // Act.
        let answered = call(
            "createSpreadsheet",
            r#"{"title":"Q","sheets":[{"title":"Q 1","rows":[[1]]},{"title":"Bob's","rows":[[2]]}]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: titles needing quotes get them, with quotes doubled.
        assert_eq!(
            stub.seen[1].url,
            format!("{API}/new-id/values/%27Q%201%27%21A1?valueInputOption=USER_ENTERED")
        );
        assert_eq!(
            stub.seen[2].url,
            format!("{API}/new-id/values/%27Bob%27%27s%27%21A1?valueInputOption=USER_ENTERED")
        );
        // Assert: the API answered no sheets, so the requested titles are used.
        assert_eq!(
            structured(&answered),
            r#"{"spreadsheetId":"new-id","spreadsheetUrl":"https://docs.google.com/spreadsheets/d/new-id","sheets":[{"title":"Q 1"},{"title":"Bob's"}]}"#
        );
    }

    #[test]
    fn skips_sheets_without_rows() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"spreadsheetId":"new-id"}"#)]);

        // Act.
        call(
            "createSpreadsheet",
            r#"{"title":"Q","sheets":[{"title":"Empty"},{"title":"Blank","rows":[]}]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: no values.update calls at all.
        assert_eq!(stub.seen.len(), 1);
    }

    #[test]
    fn creates_a_spreadsheet_without_sheets() {
        // Arrange: the old single-sheet shape.
        let mut stub = Stub::of(&[ok_json(
            r#"{"spreadsheetId":"new-id","sheets":[{"properties":{"title":"Sheet1"}}]}"#,
        )]);

        // Act.
        let answered = call("createSpreadsheet", r#"{"title":"Budget"}"#, &mut stub).unwrap();

        // Assert.
        assert_eq!(
            body_of(&stub.seen[0]),
            json!({ "properties": { "title": "Budget" } })
        );
        assert_eq!(
            structured(&answered),
            r#"{"spreadsheetId":"new-id","spreadsheetUrl":"https://docs.google.com/spreadsheets/d/new-id","sheets":[{"title":"Sheet1"}]}"#
        );
    }

    #[test]
    fn create_spreadsheet_reports_a_failed_create() {
        // Arrange.
        let mut stub = Stub::of(&[response(403, "insufficient scope")]);

        // Act.
        let answered = call("createSpreadsheet", r#"{"title":"Budget"}"#, &mut stub).unwrap();

        // Assert: still a tool answer, not a call error.
        assert_eq!(answered.structured, None);
    }

    #[test]
    fn create_spreadsheet_reports_a_failed_fill() {
        // Arrange: the create succeeds but a sheet fill fails.
        let mut stub = Stub::of(&[
            ok_json(r#"{"spreadsheetId":"new-id"}"#),
            response(400, "bad range"),
        ]);

        // Act.
        let answered = call(
            "createSpreadsheet",
            r#"{"title":"Budget","sheets":[{"title":"Q2","rows":[[1]]}]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(answered.structured, None);
    }

    #[test]
    fn create_spreadsheet_rejects_an_answer_without_an_id() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json("{}")]);

        // Act.
        let missing = call("createSpreadsheet", r#"{"title":"Budget"}"#, &mut stub);

        // Assert.
        assert!(
            matches!(&missing, Err(ConnectorError::Upstream(message)) if message.contains("spreadsheetId")),
            "{missing:?}"
        );
    }

    #[test]
    fn lists_sheet_titles() {
        let created = json!({
            "sheets": [
                { "properties": { "title": "A" } },
                { "no-properties": true },
                { "properties": {} },
            ]
        });
        assert_eq!(
            sheet_titles(&created, &["Requested".to_string()]),
            ["A".to_string()]
        );

        // A sheets array with no usable titles falls back to the request.
        assert_eq!(
            sheet_titles(&json!({"sheets": []}), &["Requested".to_string()]),
            ["Requested".to_string()]
        );

        // No sheets at all falls back to the request.
        assert_eq!(
            sheet_titles(&json!({}), &["Requested".to_string()]),
            ["Requested".to_string()]
        );

        // Nothing anywhere means no sheets.
        assert_eq!(sheet_titles(&json!({}), &[]), Vec::<String>::new());
    }

    #[test]
    fn inspects_a_spreadsheet() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"spreadsheetId":"abc"}"#)]);

        // Act.
        call(
            "getSpreadsheet",
            r#"{"spreadsheetId":"abc","ranges":["Sheet1","Sheet2"],"includeGridData":true}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(stub.seen[0].method, Method::Get);
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/abc?ranges=Sheet1&ranges=Sheet2&includeGridData=true")
        );
    }

    #[test]
    fn inspects_a_spreadsheet_without_options() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json("{}")]);

        // Act.
        call("getSpreadsheet", r#"{"spreadsheetId":"abc"}"#, &mut stub).unwrap();

        // Assert.
        assert_eq!(stub.seen[0].url, format!("{API}/abc"));
    }

    #[test]
    fn runs_batch_update() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"spreadsheetId":"abc"}"#)]);

        // Act.
        call(
            "batchUpdate",
            r#"{"spreadsheetId":"abc","requests":[{"addSheet":{"properties":{"title":"Q2"}}}]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(stub.seen[0].method, Method::Post);
        assert_eq!(stub.seen[0].url, format!("{API}/abc:batchUpdate"));
        assert_eq!(
            body_of(&stub.seen[0]),
            json!({ "requests": [{ "addSheet": { "properties": { "title": "Q2" } } }] })
        );
    }

    #[test]
    fn trims_a_trailing_slash_off_the_base() {
        assert_eq!(
            Api::new("https://sheets.test/").spreadsheets,
            "https://sheets.test/v4/spreadsheets"
        );
    }

    #[test]
    fn refuses_bad_input() {
        let mut stub = Stub::of(&[]);

        let malformed = call("getValues", "{oops", &mut stub);
        assert!(
            matches!(&malformed, Err(ConnectorError::InvalidInput(_))),
            "{malformed:?}"
        );

        let missing = call("getValues", r#"{"spreadsheetId":"abc"}"#, &mut stub);
        assert!(
            matches!(&missing, Err(ConnectorError::InvalidInput(_))),
            "{missing:?}"
        );

        let unknown = call("nope", "{}", &mut stub);
        assert_eq!(
            unknown.unwrap_err(),
            ConnectorError::NotFound("unknown tool: nope".into())
        );
    }

    #[test]
    fn propagates_sender_errors() {
        // Arrange: no responses queued, so every send fails.
        let mut stub = Stub::of(&[]);

        // Act + assert: each tool's send sites surface the failure.
        for (tool, body) in [
            ("getValues", r#"{"spreadsheetId":"a","range":"b"}"#),
            ("readTable", r#"{"spreadsheetId":"a","range":"b"}"#),
            (
                "createSpreadsheet",
                r#"{"title":"Q","sheets":[{"title":"S","rows":[[1]]}]}"#,
            ),
        ] {
            let failed = call(tool, body, &mut stub);
            assert!(
                matches!(&failed, Err(ConnectorError::Upstream(message)) if message.contains("stub sender ran out")),
                "{tool}: {failed:?}"
            );
        }
    }

    #[test]
    fn propagates_sender_errors_on_the_fill() {
        // Arrange: the create succeeds, the fill's send fails.
        let mut stub = Stub::of(&[ok_json(r#"{"spreadsheetId":"new-id"}"#)]);

        // Act.
        let failed = call(
            "createSpreadsheet",
            r#"{"title":"Q","sheets":[{"title":"S","rows":[[1]]}]}"#,
            &mut stub,
        );

        // Assert.
        assert!(
            matches!(&failed, Err(ConnectorError::Upstream(message)) if message.contains("stub sender ran out")),
            "{failed:?}"
        );
    }

    #[test]
    fn maps_responses() {
        let ok = answer(&response(200, r#"{"values":[["a"]]}"#)).unwrap();
        assert_eq!(ok.structured.as_deref(), Some(r#"{"values":[["a"]]}"#));
        let refused = answer(&response(403, "insufficient scope")).unwrap();
        assert_eq!(refused.structured, None);
        let garbled = answer(&response(200, "<html>"));
        assert!(
            matches!(&garbled, Err(ConnectorError::Upstream(_))),
            "{garbled:?}"
        );
    }

    #[test]
    fn checks_intermediate_steps() {
        assert!(step_ok(&ok_json("{}"), "anything").is_ok());

        let failed = step_ok(&response(403, "denied"), "reading the table");
        let error = failed.unwrap_err();
        assert_eq!(error.structured, None);
    }

    #[test]
    fn off_wasm_nothing_is_sent() {
        let sent = Gsheets::call_tool(request("getValues", r#"{"spreadsheetId":"a","range":"b"}"#));
        assert!(
            matches!(&sent, Err(ConnectorError::Upstream(message)) if message.contains("wasm32-wasip2"))
        );
    }

    #[test]
    fn manifest_is_embedded() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["name"], "gsheets");
        assert_eq!(manifest["version"], "0.2.0");
        assert_eq!(manifest["auth"]["schemes"][0]["type"], "oauth2");
        assert_eq!(
            manifest["auth"]["schemes"][0]["scopes"][0],
            "https://www.googleapis.com/auth/spreadsheets"
        );
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
        assert_eq!(
            tools,
            [
                "getValues",
                "readTable",
                "updateValues",
                "appendValues",
                "clearValues",
                "createSpreadsheet",
                "getSpreadsheet",
                "batchUpdate",
            ]
        );
        let create = &manifest["tools"].as_array().unwrap()[5];
        assert!(create["inputSchema"]["properties"]["sheets"].is_object());
        let table = &manifest["tools"].as_array().unwrap()[1];
        assert_eq!(
            table["inputSchema"]["required"].as_array().unwrap().len(),
            2
        );
    }
}
