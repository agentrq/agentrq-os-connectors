//! Google Calendar connector: task-shaped tools for an agent's scheduling
//! work through the Google Calendar API v3. Listing, reading, creating,
//! updating and deleting events are single calls; `findFreeTime` composes one
//! `freeBusy` query with pure slot computation, so "find a 30-minute slot
//! next week" finishes in one tool call instead of forcing the agent to list
//! events and do interval math by hand.
//!
//! The host runs the OAuth 2.0 sign-in, refreshes the token and adds
//! `Authorization: Bearer …` after each request leaves the guest: this code
//! never sees it.
//!
//! [`Gcal::call_with`] injects the sender so multi-step tools stay
//! unit-testable: the guest sends requests over HTTP, while tests answer
//! from canned responses. Each tool is `plan_*` / `parse_*` / `combine_*`
//! pure helpers around that seam.

use connectors_sdk::http::{Method, Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FindFreeTime {
    time_min: String,
    time_max: String,
    duration_minutes: i64,
    calendar_ids: Option<Vec<String>>,
}

impl Connector for Gcal {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        Self::call_with(request, |request| Ok(request.clone().send()?))
    }
}

impl Gcal {
    /// Dispatches a tool call, sending requests through `send`: the real
    /// `Request::send` in production, a stub in tests so the whole
    /// orchestration is covered end to end without a network.
    fn call_with(
        request: ToolRequest,
        mut send: impl FnMut(&Request) -> Result<Response, ConnectorError>,
    ) -> Result<ToolResponse, ConnectorError> {
        let base = host::config("api_base").unwrap_or_else(|| DEFAULT_API_BASE.into());
        let api = Api::new(&base);
        match request.tool.as_str() {
            "listEvents" => one_call(&api, &request, &mut send, plan_list_events),
            "getEvent" => one_call(&api, &request, &mut send, plan_get_event),
            "createEvent" => one_call(&api, &request, &mut send, plan_create_event),
            "updateEvent" => one_call(&api, &request, &mut send, plan_update_event),
            "deleteEvent" => one_call(&api, &request, &mut send, plan_delete_event),
            "findFreeTime" => find_free_time(&api, &request, &mut send),
            other => Err(ConnectorError::NotFound(format!("no tool {other}"))),
        }
    }
}

connectors_sdk::export_connector!(Gcal);

/// The Calendar API endpoints derived from the configured base URL.
struct Api {
    /// `{base}/calendar/v3`, the root of every endpoint.
    root: String,
}

impl Api {
    fn new(base: &str) -> Self {
        Self {
            root: format!("{}/calendar/v3", base.trim_end_matches('/')),
        }
    }

    /// The URL of a calendar's event collection.
    fn events(&self, calendar_id: &str) -> String {
        format!("{}/calendars/{}/events", self.root, encode(calendar_id))
    }

    /// The URL of one event's resource.
    fn event(&self, calendar_id: &str, event_id: &str) -> String {
        format!("{}/{}", self.events(calendar_id), encode(event_id))
    }

    /// The URL of the free/busy query endpoint.
    fn free_busy(&self) -> String {
        format!("{}/freeBusy", self.root)
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

/// Plans `listEvents`.
fn plan_list_events(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: ListEvents = request.json()?;
    Ok(json_get(with_query(
        &api.events(&input.calendar_id),
        &[
            ("timeMin", input.time_min),
            ("timeMax", input.time_max),
            ("q", input.q),
            ("maxResults", input.max_results.map(|n| n.to_string())),
            (
                "singleEvents",
                input.single_events.map(|single| single.to_string()),
            ),
            ("orderBy", input.order_by),
        ],
    )))
}

/// Plans `getEvent`.
fn plan_get_event(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: GetEvent = request.json()?;
    Ok(json_get(api.event(&input.calendar_id, &input.event_id)))
}

/// Plans `createEvent`.
fn plan_create_event(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: CreateEvent = request.json()?;
    let url = with_query(
        &api.events(&input.calendar_id),
        &[("sendUpdates", input.send_updates)],
    );
    Ok(Request::post(url)
        .header("accept", "application/json")
        .json(&input.event)?)
}

/// Plans `updateEvent`.
fn plan_update_event(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: UpdateEvent = request.json()?;
    let url = with_query(
        &api.event(&input.calendar_id, &input.event_id),
        &[("sendUpdates", input.send_updates)],
    );
    Ok(Request::new(Method::Patch, url)
        .header("accept", "application/json")
        .json(&input.event)?)
}

/// Plans `deleteEvent`.
fn plan_delete_event(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: DeleteEvent = request.json()?;
    let url = with_query(
        &api.event(&input.calendar_id, &input.event_id),
        &[("sendUpdates", input.send_updates)],
    );
    Ok(Request::new(Method::Delete, url).header("accept", "application/json"))
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

// ---------------------------------------------------------------------------
// findFreeTime: one freeBusy query, then pure slot computation
// ---------------------------------------------------------------------------

/// Runs `findFreeTime`: validates the search, asks free/busy once, and
/// answers the free slots of at least the wanted length.
fn find_free_time(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: FindFreeTime = request.json()?;
    let query = FreeTimeQuery::of(input)?;
    let response = send(&query.request(api)?)?; // coverage: exclude — the freeBusy plan serializes a serde_json::Value, which is infallible
    if let Err(error) = step_ok(&response, "finding free time") {
        return Ok(error);
    }
    let free_busy: FreeBusy = response.json()?;
    if let Some(error) = calendar_error(&free_busy) {
        return Ok(error);
    }
    let busy = busy_intervals(&free_busy)?;
    combine_free_slots(&busy, &query)
}

/// A validated `findFreeTime` search: the window as epoch seconds, plus the
/// offset its answers are formatted in.
struct FreeTimeQuery {
    time_min: String,
    time_max: String,
    calendars: Vec<String>,
    start: i64,
    end: i64,
    offset_minutes: i32,
    duration_secs: i64,
}

impl FreeTimeQuery {
    /// Parses the tool input into a validated search. Anything the agent got
    /// wrong is [`ConnectorError::InvalidInput`].
    fn of(input: FindFreeTime) -> Result<Self, ConnectorError> {
        let Some(start) = parse_moment(&input.time_min) else {
            return Err(invalid(
                "timeMin must be RFC3339, e.g. 2026-10-11T00:00:00-07:00",
            ));
        };
        let Some(end) = parse_moment(&input.time_max) else {
            return Err(invalid(
                "timeMax must be RFC3339, e.g. 2026-10-18T00:00:00-07:00",
            ));
        };
        if end.epoch <= start.epoch {
            return Err(invalid("timeMax must be after timeMin"));
        }
        if input.duration_minutes <= 0 {
            return Err(invalid("durationMinutes must be positive"));
        }
        let duration_secs = input
            .duration_minutes
            .checked_mul(60)
            .ok_or_else(|| invalid("durationMinutes is too large"))?;
        let calendars = input
            .calendar_ids
            .unwrap_or_else(|| vec!["primary".to_string()]);
        if calendars.is_empty() {
            return Err(invalid("calendarIds must name at least one calendar"));
        }
        Ok(Self {
            time_min: input.time_min,
            time_max: input.time_max,
            calendars,
            start: start.epoch,
            end: end.epoch,
            offset_minutes: start.offset_minutes,
            duration_secs,
        })
    }

    /// Plans the search's single request: one `freeBusy.query` for every
    /// calendar.
    fn request(&self, api: &Api) -> Result<Request, ConnectorError> {
        let body = json!({
            "timeMin": self.time_min,
            "timeMax": self.time_max,
            "items": self.calendars.iter().map(|id| json!({"id": id})).collect::<Vec<_>>(),
        });
        Ok(Request::post(api.free_busy())
            .header("accept", "application/json")
            .json(&body)?)
    }
}

/// The response of the search's query step: 2xx continues the tool, anything
/// else becomes a tool error naming the failed step.
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

/// The free/busy answer for the searched calendars.
#[derive(Debug, Deserialize)]
struct FreeBusy {
    #[serde(default)]
    calendars: BTreeMap<String, FreeBusyCalendar>,
}

/// One calendar's free/busy answer: its busy periods, or the errors that
/// kept the API from computing them.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FreeBusyCalendar {
    #[serde(default)]
    busy: Vec<BusyPeriod>,
    #[serde(default)]
    errors: Vec<Value>,
}

/// One busy period of a calendar.
#[derive(Debug, Deserialize)]
struct BusyPeriod {
    start: String,
    end: String,
}

/// The first calendar the API reported an error for, as a tool error: its
/// busy data is unknown, so any "free" slot would be a lie.
fn calendar_error(free_busy: &FreeBusy) -> Option<ToolResponse> {
    free_busy.calendars.iter().find_map(|(id, calendar)| {
        calendar.errors.first().map(|error| {
            ToolResponse::error(format!(
                "finding free time failed: the API reported an error for calendar {id}: {error}"
            ))
        })
    })
}

/// Busy epoch-second intervals across all calendars. Zero-width blocks are
/// dropped; a timestamp that is not RFC3339 is upstream garbage.
fn busy_intervals(free_busy: &FreeBusy) -> Result<Vec<(i64, i64)>, ConnectorError> {
    let mut intervals = Vec::new();
    for calendar in free_busy.calendars.values() {
        for period in &calendar.busy {
            let start = timestamp(&period.start)?;
            let end = timestamp(&period.end)?;
            if end > start {
                intervals.push((start, end));
            }
        }
    }
    Ok(intervals)
}

/// Parses one timestamp of the API's answer.
fn timestamp(text: &str) -> Result<i64, ConnectorError> {
    parse_moment(text)
        .map(|moment| moment.epoch)
        .ok_or_else(|| {
            ConnectorError::Upstream(format!(
                "the API answered an unparseable timestamp: {text:?}"
            ))
        })
}

/// Combines the search into its answer: the free slots of at least the
/// wanted length inside the window, in chronological order, formatted in
/// the window's offset.
fn combine_free_slots(
    busy: &[(i64, i64)],
    query: &FreeTimeQuery,
) -> Result<ToolResponse, ConnectorError> {
    let slots: Vec<Value> = free_slots(busy, query.start, query.end, query.duration_secs)
        .into_iter()
        .map(|(start, end)| {
            json!({
                "start": format_moment(start, query.offset_minutes),
                "end": format_moment(end, query.offset_minutes),
            })
        })
        .collect();
    ToolResponse::structured(&json!({ "slots": slots }))
}

/// The free gaps of at least `duration` seconds inside `[start, end)`,
/// chronological. Busy intervals are clipped to the window and merged when
/// they overlap or touch, so adjacent meetings leave no phantom slot.
fn free_slots(busy: &[(i64, i64)], start: i64, end: i64, duration: i64) -> Vec<(i64, i64)> {
    let mut spans: Vec<(i64, i64)> = busy
        .iter()
        .map(|&(busy_start, busy_end)| (busy_start.max(start), busy_end.min(end)))
        .filter(|&(busy_start, busy_end)| busy_end > busy_start)
        .collect();
    spans.sort_unstable();
    let mut slots = Vec::new();
    let mut cursor = start;
    for (busy_start, busy_end) in spans {
        if busy_start > cursor {
            if busy_start - cursor >= duration {
                slots.push((cursor, busy_start));
            }
            cursor = busy_end;
        } else if busy_end > cursor {
            cursor = busy_end;
        }
    }
    if end - cursor >= duration {
        slots.push((cursor, end));
    }
    slots
}

// ---------------------------------------------------------------------------
// RFC3339 moments, parsed and formatted without extra dependencies
// ---------------------------------------------------------------------------

/// A moment as seconds since the Unix epoch, plus the wall-clock offset it
/// was written in, so answers echo the agent's own offset back.
#[derive(Debug, PartialEq)]
struct Moment {
    epoch: i64,
    offset_minutes: i32,
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.frac](Z|±HH:MM)`. Returns `None` for
/// anything that is not a valid timestamp in that shape.
fn parse_moment(text: &str) -> Option<Moment> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 || !matches!(bytes[10], b'T' | b't') {
        return None;
    }
    let year = digits(bytes, 0, 4)?;
    let month = digits(bytes, 5, 2)?;
    let day = digits(bytes, 8, 2)?;
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let hour = digits(bytes, 11, 2)?;
    let minute = digits(bytes, 14, 2)?;
    let second = digits(bytes, 17, 2)?;
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    // An optional fractional second, ignored.
    let mut at = 19;
    if matches!(bytes.get(at), Some(b'.')) {
        at += 1;
        let fraction = at;
        while matches!(bytes.get(at), Some(b'0'..=b'9')) {
            at += 1;
        }
        if at == fraction {
            return None;
        }
    }
    let offset_minutes = match bytes.get(at) {
        Some(b'Z' | b'z') => {
            at += 1;
            0
        }
        Some(sign @ (b'+' | b'-')) => {
            let offset_hours = digits(bytes, at + 1, 2)?;
            if bytes.get(at + 3) != Some(&b':') {
                return None;
            }
            let offset_mins = digits(bytes, at + 4, 2)?;
            if offset_hours > 23 || offset_mins > 59 {
                return None;
            }
            at += 6;
            let total = offset_hours * 60 + offset_mins;
            if *sign == b'+' { total } else { -total }
        }
        _ => return None,
    };
    if at != bytes.len() {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let month_days = days_in_month(month, year)?;
    if day < 1 || day > month_days {
        return None;
    }
    let days = days_from_civil(i64::from(year), i64::from(month), i64::from(day));
    let epoch =
        days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second)
            - i64::from(offset_minutes) * 60;
    Some(Moment {
        epoch,
        offset_minutes,
    })
}

/// Reads `len` ASCII digits at `at`.
fn digits(bytes: &[u8], at: usize, len: usize) -> Option<i32> {
    let mut value = 0;
    for &byte in bytes.get(at..at + len)? {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10 + i32::from(byte - b'0');
    }
    Some(value)
}

/// Days in a month, honoring leap years. `None` for a month that is not one.
fn days_in_month(month: i32, year: i32) -> Option<i32> {
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => return None,
    })
}

/// The Gregorian leap-year rule.
fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Days since the Unix epoch for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted = if month <= 2 { year - 1 } else { year };
    let era = adjusted.div_euclid(400);
    let year_of_era = adjusted - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The civil date of days since the Unix epoch (Hinnant's inverse).
fn civil_from_days(days: i64) -> (i64, i32, i32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let full_year = if month <= 2 { year + 1 } else { year };
    (full_year, month as i32, day as i32)
}

/// Formats epoch seconds as RFC3339 in the given wall-clock offset: `Z`
/// for UTC, `±HH:MM` otherwise.
fn format_moment(epoch: i64, offset_minutes: i32) -> String {
    let local = epoch + i64::from(offset_minutes) * 60;
    let (year, month, day) = civil_from_days(local.div_euclid(86_400));
    let seconds = local.rem_euclid(86_400);
    let zone = if offset_minutes == 0 {
        "Z".to_string()
    } else {
        let sign = if offset_minutes < 0 { '-' } else { '+' };
        let abs = offset_minutes.abs();
        format!("{sign}{:02}:{:02}", abs / 60, abs % 60)
    };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}{zone}",
        seconds / 3_600,
        seconds % 3_600 / 60,
        seconds % 60,
    )
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// A GET that asks for JSON back.
fn json_get(url: String) -> Request {
    Request::get(url).header("accept", "application/json")
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

/// An [`ConnectorError::InvalidInput`] with a lowercase message.
fn invalid(message: &str) -> ConnectorError {
    ConnectorError::InvalidInput(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

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
        Gcal::call_with(request(tool, body), |request| stub.send(request))
    }

    const API: &str = "https://www.googleapis.com/calendar/v3";

    #[test]
    fn lists_events_with_every_filter() {
        // Arrange: a search with every parameter set.
        let mut stub = Stub::of(&[ok_json(r#"{"items":[]}"#)]);

        // Act.
        let answered = call(
            "listEvents",
            r#"{"calendarId":"primary","timeMin":"2026-10-11T00:00:00-07:00","timeMax":"2026-10-18T00:00:00-07:00","q":"standup","maxResults":25,"singleEvents":true,"orderBy":"startTime"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(answered.structured.as_deref(), Some(r#"{"items":[]}"#));
        assert_eq!(stub.seen.len(), 1);
        assert_eq!(stub.seen[0].method, Method::Get);
        assert_eq!(
            stub.seen[0].url,
            format!(
                "{API}/calendars/primary/events?timeMin=2026-10-11T00%3A00%3A00-07%3A00&timeMax=2026-10-18T00%3A00%3A00-07%3A00&q=standup&maxResults=25&singleEvents=true&orderBy=startTime"
            )
        );
    }

    #[test]
    fn lists_events_with_defaults() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"items":[]}"#)]);

        // Act.
        call("listEvents", r#"{"calendarId":"primary"}"#, &mut stub).unwrap();

        // Assert: no query string at all, and JSON is asked for.
        assert_eq!(stub.seen[0].url, format!("{API}/calendars/primary/events"));
        assert_eq!(
            stub.seen[0]
                .headers
                .iter()
                .find(|(name, _)| name == "accept")
                .map(|(_, value)| value.as_str()),
            Some("application/json")
        );
    }

    #[test]
    fn gets_an_event() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"summary":"Focus"}"#)]);

        // Act.
        let answered = call(
            "getEvent",
            r#"{"calendarId":"primary","eventId":"e1"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            answered.structured.as_deref(),
            Some(r#"{"summary":"Focus"}"#)
        );
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/calendars/primary/events/e1")
        );
    }

    #[test]
    fn creates_an_event() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"id":"e9"}"#)]);

        // Act.
        let answered = call(
            "createEvent",
            r#"{"calendarId":"primary","event":{"summary":"Focus","start":{"dateTime":"2026-10-12T14:00:00-07:00"},"end":{"dateTime":"2026-10-12T15:00:00-07:00"}},"sendUpdates":"none"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(answered.structured.as_deref(), Some(r#"{"id":"e9"}"#));
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/calendars/primary/events?sendUpdates=none")
        );
        assert_eq!(stub.seen[0].method, Method::Post);
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(body["summary"], json!("Focus"));
    }

    #[test]
    fn updates_an_event() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"location":"Room A"}"#)]);

        // Act.
        call(
            "updateEvent",
            r#"{"calendarId":"primary","eventId":"e1","event":{"location":"Room A"},"sendUpdates":"all"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/calendars/primary/events/e1?sendUpdates=all")
        );
        assert_eq!(stub.seen[0].method, Method::Patch);
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(body["location"], json!("Room A"));
    }

    #[test]
    fn deletes_an_event() {
        // Arrange.
        let mut stub = Stub::of(&[response(204, "")]);

        // Act.
        let answered = call(
            "deleteEvent",
            r#"{"calendarId":"primary","eventId":"e1","sendUpdates":"none"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the API's 204 becomes a deletion confirmation.
        assert_eq!(answered.structured.as_deref(), Some(r#"{"deleted":true}"#));
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/calendars/primary/events/e1?sendUpdates=none")
        );
        assert_eq!(stub.seen[0].method, Method::Delete);
    }

    #[test]
    fn refuses_unknown_tools() {
        // Arrange.
        let mut stub = Stub::of(&[]);

        // Act.
        let unknown = call("nope", "{}", &mut stub);

        // Assert.
        assert_eq!(
            unknown.unwrap_err(),
            ConnectorError::NotFound("no tool nope".into())
        );
        assert!(stub.seen.is_empty());
    }

    #[test]
    fn refuses_invalid_input() {
        // Every tool rejects a body that is not JSON, before sending anything.
        for tool in [
            "listEvents",
            "getEvent",
            "createEvent",
            "updateEvent",
            "deleteEvent",
            "findFreeTime",
        ] {
            // Arrange.
            let mut stub = Stub::of(&[]);

            // Act.
            let bad = call(tool, "{oops", &mut stub);

            // Assert.
            assert!(
                matches!(bad, Err(ConnectorError::InvalidInput(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
                "{tool}: {bad:?}"
            );
            assert!(stub.seen.is_empty(), "{tool}");
        }
    }

    #[test]
    fn maps_api_answers() {
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
            matches!(garbled, Err(ConnectorError::Upstream(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{garbled:?}"
        );
    }

    #[test]
    fn surfaces_garbled_responses() {
        // Arrange: valid input, but the API answers HTML instead of JSON.
        let mut stub = Stub::of(&[response(200, "<html>")]);

        // Act.
        let garbled = call(
            "findFreeTime",
            r#"{"timeMin":"2026-10-12T09:00:00-07:00","timeMax":"2026-10-12T17:00:00-07:00","durationMinutes":30}"#,
            &mut stub,
        );

        // Assert: upstream garbage stays an upstream error.
        assert!(
            matches!(garbled, Err(ConnectorError::Upstream(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{garbled:?}"
        );
    }

    const FREE_BUSY: &str = r#"{"calendars":{
        "primary": {"busy": [{"start": "2026-10-12T10:00:00-07:00", "end": "2026-10-12T11:00:00-07:00"}]},
        "team@example.com": {"busy": [
            {"start": "2026-10-12T10:30:00-07:00", "end": "2026-10-12T12:00:00-07:00"},
            {"start": "2026-10-12T13:00:00-07:00", "end": "2026-10-12T13:30:00-07:00"}
        ]}
    }}"#;

    #[test]
    fn finds_free_time_across_calendars() {
        // Arrange: overlapping busy blocks on two calendars.
        let mut stub = Stub::of(&[ok_json(FREE_BUSY)]);

        // Act.
        let answered = call(
            "findFreeTime",
            r#"{"timeMin":"2026-10-12T09:00:00-07:00","timeMax":"2026-10-12T17:00:00-07:00","durationMinutes":60,"calendarIds":["primary","team@example.com"]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: one freeBusy query, then the merged free slots.
        assert_eq!(stub.seen.len(), 1);
        assert_eq!(stub.seen[0].method, Method::Post);
        assert_eq!(stub.seen[0].url, format!("{API}/freeBusy"));
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(body["timeMin"], json!("2026-10-12T09:00:00-07:00"));
        assert_eq!(body["timeMax"], json!("2026-10-12T17:00:00-07:00"));
        assert_eq!(
            body["items"],
            json!([{"id": "primary"}, {"id": "team@example.com"}])
        );
        assert_eq!(
            serde_json::from_str::<Value>(answered.structured.as_deref().unwrap()).unwrap(),
            json!({"slots":[
                {"start":"2026-10-12T09:00:00-07:00","end":"2026-10-12T10:00:00-07:00"},
                {"start":"2026-10-12T12:00:00-07:00","end":"2026-10-12T13:00:00-07:00"},
                {"start":"2026-10-12T13:30:00-07:00","end":"2026-10-12T17:00:00-07:00"},
            ]})
        );
    }

    #[test]
    fn find_free_time_defaults_to_primary() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"calendars":{"primary":{"busy":[]}}}"#)]);

        // Act: no calendarIds given.
        let answered = call(
            "findFreeTime",
            r#"{"timeMin":"2026-10-12T16:00:00Z","timeMax":"2026-10-12T18:00:00Z","durationMinutes":30}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: primary is queried, the empty day is one UTC slot.
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(body["items"], json!([{"id": "primary"}]));
        assert_eq!(
            serde_json::from_str::<Value>(answered.structured.as_deref().unwrap()).unwrap(),
            json!({"slots":[{"start":"2026-10-12T16:00:00Z","end":"2026-10-12T18:00:00Z"}]})
        );
    }

    #[test]
    fn find_free_time_rejects_bad_searches() {
        // Arrange: every invalid shape the validation guards.
        let cases = [
            // timeMin is not RFC3339.
            (
                r#"{"timeMin":"tomorrow","timeMax":"2026-10-18T00:00:00-07:00","durationMinutes":30}"#,
                "timeMin must be RFC3339",
            ),
            // timeMax is not RFC3339.
            (
                r#"{"timeMin":"2026-10-11T00:00:00-07:00","timeMax":"next week","durationMinutes":30}"#,
                "timeMax must be RFC3339",
            ),
            // The window ends when it starts.
            (
                r#"{"timeMin":"2026-10-11T00:00:00-07:00","timeMax":"2026-10-11T00:00:00-07:00","durationMinutes":30}"#,
                "timeMax must be after timeMin",
            ),
            // The window runs backwards.
            (
                r#"{"timeMin":"2026-10-18T00:00:00-07:00","timeMax":"2026-10-11T00:00:00-07:00","durationMinutes":30}"#,
                "timeMax must be after timeMin",
            ),
            // A zero-length slot is not a slot.
            (
                r#"{"timeMin":"2026-10-11T00:00:00-07:00","timeMax":"2026-10-18T00:00:00-07:00","durationMinutes":0}"#,
                "durationMinutes must be positive",
            ),
            // Neither is a negative one.
            (
                r#"{"timeMin":"2026-10-11T00:00:00-07:00","timeMax":"2026-10-18T00:00:00-07:00","durationMinutes":-15}"#,
                "durationMinutes must be positive",
            ),
            // Minutes times sixty must fit.
            (
                r#"{"timeMin":"2026-10-11T00:00:00-07:00","timeMax":"2026-10-18T00:00:00-07:00","durationMinutes":153722867280912931}"#,
                "durationMinutes is too large",
            ),
            // Searching no calendars searches nothing.
            (
                r#"{"timeMin":"2026-10-11T00:00:00-07:00","timeMax":"2026-10-18T00:00:00-07:00","durationMinutes":30,"calendarIds":[]}"#,
                "calendarIds must name at least one calendar",
            ),
            // A missing timeMin is not valid input either.
            (
                r#"{"timeMax":"2026-10-18T00:00:00-07:00","durationMinutes":30}"#,
                "missing field",
            ),
        ];

        for (body, message) in cases {
            // Act.
            let mut stub = Stub::of(&[]);
            let rejected = call("findFreeTime", body, &mut stub);

            // Assert: invalid input, and nothing was sent.
            assert!(
                matches!(&rejected, Err(ConnectorError::InvalidInput(error)) if error.contains(message)),
                "{body}: {rejected:?}"
            );
            assert!(stub.seen.is_empty(), "{body}");
        }
    }

    #[test]
    fn find_free_time_reports_api_failures() {
        // Arrange: the freeBusy query is refused.
        let mut stub = Stub::of(&[response(403, "insufficient scope")]);

        // Act.
        let answered = call(
            "findFreeTime",
            r#"{"timeMin":"2026-10-11T00:00:00-07:00","timeMax":"2026-10-18T00:00:00-07:00","durationMinutes":30}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: a tool error naming the failed step.
        assert_eq!(
            answered,
            ToolResponse::error(
                "finding free time failed: the API answered 403: insufficient scope"
            )
        );
    }

    #[test]
    fn find_free_time_reports_calendar_errors() {
        // Arrange: the API could not read one calendar.
        let mut stub = Stub::of(&[ok_json(
            r#"{"calendars":{"primary":{"busy":[]},"gone@example.com":{"errors":[{"domain":"global","reason":"notFound"}]}}}"#,
        )]);

        // Act.
        let answered = call(
            "findFreeTime",
            r#"{"timeMin":"2026-10-11T00:00:00-07:00","timeMax":"2026-10-18T00:00:00-07:00","durationMinutes":30,"calendarIds":["primary","gone@example.com"]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: a tool error (no silent "free" slots over unknown busy data).
        assert!(answered.structured.is_none());
    }

    #[test]
    fn find_free_time_rejects_unparseable_busy_periods() {
        // Arrange: a busy period the API sent but RFC3339 cannot describe.
        let mut stub = Stub::of(&[ok_json(
            r#"{"calendars":{"primary":{"busy":[{"start":"not-a-time","end":"2026-10-12T11:00:00-07:00"}]}}}"#,
        )]);

        // Act.
        let failed = call(
            "findFreeTime",
            r#"{"timeMin":"2026-10-12T09:00:00-07:00","timeMax":"2026-10-12T17:00:00-07:00","durationMinutes":30}"#,
            &mut stub,
        );

        // Assert: upstream garbage stays an upstream error.
        assert!(
            matches!(&failed, Err(ConnectorError::Upstream(message)) if message.contains("unparseable timestamp")),
            "{failed:?}"
        );
    }

    #[test]
    fn find_free_time_rejects_an_unparseable_busy_end() {
        // Arrange: a busy period whose end the API sent but RFC3339 cannot describe.
        let mut stub = Stub::of(&[ok_json(
            r#"{"calendars":{"primary":{"busy":[{"start":"2026-10-12T10:00:00-07:00","end":"not-a-time"}]}}}"#,
        )]);

        // Act.
        let failed = call(
            "findFreeTime",
            r#"{"timeMin":"2026-10-12T09:00:00-07:00","timeMax":"2026-10-12T17:00:00-07:00","durationMinutes":30}"#,
            &mut stub,
        );

        // Assert: upstream garbage stays an upstream error.
        assert!(
            matches!(&failed, Err(ConnectorError::Upstream(message)) if message.contains("unparseable timestamp")),
            "{failed:?}"
        );
    }

    #[test]
    fn find_free_time_drops_zero_width_busy_periods() {
        // Arrange: a busy period that starts and ends at the same moment.
        let mut stub = Stub::of(&[ok_json(
            r#"{"calendars":{"primary":{"busy":[{"start":"2026-10-12T10:00:00-07:00","end":"2026-10-12T10:00:00-07:00"}]}}}"#,
        )]);

        // Act.
        let answered = call(
            "findFreeTime",
            r#"{"timeMin":"2026-10-12T09:00:00-07:00","timeMax":"2026-10-12T17:00:00-07:00","durationMinutes":60}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the zero-width block holds nothing back; the whole window is free.
        assert_eq!(
            serde_json::from_str::<Value>(answered.structured.as_deref().unwrap()).unwrap(),
            json!({"slots":[{"start":"2026-10-12T09:00:00-07:00","end":"2026-10-12T17:00:00-07:00"}]})
        );
    }

    #[test]
    fn computes_free_slots() {
        // An empty day is one slot when it fits, none when it does not.
        assert_eq!(free_slots(&[], 0, 600, 60), vec![(0, 600)]);
        assert_eq!(free_slots(&[], 0, 30, 60), Vec::new());

        // A fully busy window has no slots.
        assert_eq!(free_slots(&[(0, 600)], 0, 600, 60), Vec::new());

        // Adjacent meetings merge: no phantom slot between them.
        assert_eq!(
            free_slots(&[(10, 20), (20, 30)], 0, 100, 5),
            vec![(0, 10), (30, 100)]
        );

        // Overlapping meetings merge, even given out of order.
        assert_eq!(
            free_slots(&[(40, 70), (10, 50)], 0, 100, 5),
            vec![(0, 10), (70, 100)]
        );

        // A gap exactly the wanted length counts; a shorter one does not.
        assert_eq!(free_slots(&[(0, 30)], 0, 90, 60), vec![(30, 90)]);
        assert_eq!(free_slots(&[(0, 40)], 0, 90, 60), Vec::new());

        // A gap shorter than the wanted duration yields no slot before the meeting.
        assert_eq!(free_slots(&[(30, 40)], 0, 100, 60), vec![(40, 100)]);

        // A meeting fully inside another changes nothing.
        assert_eq!(
            free_slots(&[(10, 50), (20, 30)], 0, 200, 60),
            vec![(50, 200)]
        );

        // Busy blocks outside the window are clipped or dropped.
        assert_eq!(
            free_slots(&[(-100, -50), (200, 300)], 0, 100, 60),
            vec![(0, 100)]
        );
        assert_eq!(free_slots(&[(-10, 20)], 0, 100, 60), vec![(20, 100)]);

        // Zero-width blocks never open or close a slot.
        assert_eq!(free_slots(&[(30, 30)], 0, 100, 60), vec![(0, 100)]);
    }

    #[test]
    fn parses_rfc3339_moments() {
        // Arrange: valid timestamps with their epoch seconds and offsets.
        let valid = [
            ("1970-01-01T00:00:00Z", 0, 0),
            ("2026-10-12T09:00:00-07:00", 1_791_820_800, -420),
            ("2026-10-12T09:00:00.125-07:00", 1_791_820_800, -420),
            ("2026-10-12t09:00:00z", 1_791_795_600, 0),
            ("2026-01-05T08:30:00+05:30", 1_767_582_000, 330),
            ("2024-02-29T12:00:00Z", 1_709_208_000, 0),
            ("2000-02-29T00:00:00Z", 951_782_400, 0),
            ("2026-04-15T00:00:00Z", 1_776_211_200, 0),
        ];
        for (text, epoch, offset) in valid {
            // Act.
            let moment = parse_moment(text);

            // Assert.
            assert!(
                matches!(&moment, Some(m) if m.epoch == epoch && m.offset_minutes == offset),
                "{text}: {moment:?}"
            );
        }

        // Arrange: everything that is not a timestamp in the parsed shape.
        let invalid = [
            "",
            "tomorrow",
            "2026-10-12 09:00:00Z",      // no T separator
            "20x6-10-12T09:00:00Z",      // non-digit year
            "2026/10-12T09:00:00Z",      // wrong date separator
            "2026-10-12Txx:00:00Z",      // non-digit hour
            "2026-10-12T09-00:00Z",      // wrong time separator
            "2026-10-12T09:00:00.Z",     // fraction without digits
            "2026-10-12T09:00:00X",      // unknown zone
            "2026-1x-12T09:00:00Z",      // non-digit month
            "2026-10-1xT09:00:00Z",      // non-digit day
            "2026-10-12T09:x0:00Z",      // non-digit minute
            "2026-10-12T09:00:x0Z",      // non-digit second
            "2026-10-12T09:00:00+7",     // truncated zone hour
            "2026-10-12T09:00:00+07:1",  // truncated zone minute
            "2026-10-12T09:00:00+xx:00", // non-digit zone hour
            "2026-10-12T09:00:00+0700",  // zone without colon
            "2026-10-12T09:00:00+07:xx", // non-digit zone minute
            "2026-10-12T09:00:00Zjunk",  // trailing input
            "2026-10-12T09:00:00",       // missing zone
            "2026-13-01T00:00:00Z",      // month 13
            "2023-02-29T00:00:00Z",      // no leap day in 2023
            "1900-02-29T00:00:00Z",      // no leap day in 1900
            "2026-04-31T00:00:00Z",      // April has 30 days
            "2026-10-00T00:00:00Z",      // day zero
            "2026-10-12T24:00:00Z",      // hour 24
            "2026-10-12T09:60:00Z",      // minute 60
            "2026-10-12T09:00:61Z",      // second 61
            "2026-10-12T09:00:00+24:00", // zone hour 24
            "2026-10-12T09:00:00+07:60", // zone minute 60
        ];
        for text in invalid {
            // Act + assert.
            assert_eq!(parse_moment(text), None, "{text}");
        }
    }

    #[test]
    fn formats_moments_in_their_offset() {
        // A moment formats back in the offset it was parsed in.
        let moment = parse_moment("2026-10-12T09:00:00-07:00").unwrap();
        assert_eq!(
            format_moment(moment.epoch, moment.offset_minutes),
            "2026-10-12T09:00:00-07:00"
        );
        // UTC formats with Z; positive offsets keep their sign.
        assert_eq!(format_moment(0, 0), "1970-01-01T00:00:00Z");
        assert_eq!(
            format_moment(1_767_582_000, 330),
            "2026-01-05T08:30:00+05:30"
        );
        // The wall clock can roll over into the next day.
        assert_eq!(
            format_moment(1_791_824_399, -420),
            "2026-10-12T09:59:59-07:00"
        );
    }

    #[test]
    fn propagates_sender_errors() {
        // Arrange: the sender itself fails.
        let failed = Gcal::call_with(
            request("getEvent", r#"{"calendarId":"primary","eventId":"e1"}"#),
            |_| Err(ConnectorError::Upstream("network down".into())),
        );

        // Assert.
        assert_eq!(
            failed.unwrap_err(),
            ConnectorError::Upstream("network down".into())
        );

        // Arrange: a stub with no responses queued.
        let mut stub = Stub::of(&[]);

        // Act.
        let exhausted = call(
            "getEvent",
            r#"{"calendarId":"primary","eventId":"e1"}"#,
            &mut stub,
        );

        // Assert.
        assert_eq!(
            exhausted.unwrap_err(),
            ConnectorError::Upstream("stub sender ran out of responses".into())
        );

        // Every tool's first send surfaces the sender's failure the same way.
        let tools = [
            ("listEvents", r#"{"calendarId":"primary"}"#),
            ("getEvent", r#"{"calendarId":"primary","eventId":"e1"}"#),
            (
                "createEvent",
                r#"{"calendarId":"primary","event":{"summary":"Focus"}}"#,
            ),
            (
                "updateEvent",
                r#"{"calendarId":"primary","eventId":"e1","event":{"summary":"Focus"}}"#,
            ),
            ("deleteEvent", r#"{"calendarId":"primary","eventId":"e1"}"#),
            (
                "findFreeTime",
                r#"{"timeMin":"2026-10-12T09:00:00-07:00","timeMax":"2026-10-12T17:00:00-07:00","durationMinutes":30}"#,
            ),
        ];
        for (tool, body) in tools {
            // Arrange.
            let mut stub = Stub::of(&[]);

            // Act.
            let exhausted = call(tool, body, &mut stub);

            // Assert.
            assert_eq!(
                exhausted.unwrap_err(),
                ConnectorError::Upstream("stub sender ran out of responses".into()),
                "{tool}"
            );
        }
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
        let tools = manifest["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 6);
        let free_time = tools
            .iter()
            .find(|tool| tool["name"] == "findFreeTime")
            .unwrap();
        assert_eq!(
            free_time["inputSchema"]["required"],
            json!(["timeMin", "timeMax", "durationMinutes"])
        );
    }
}
