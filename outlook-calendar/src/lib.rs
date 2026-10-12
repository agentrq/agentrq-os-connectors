//! Outlook Calendar connector: task-shaped tools for an agent's scheduling
//! work through the Microsoft Graph calendar API (v1.0). Listing, reading,
//! creating, updating and deleting events are single calls; `findFreeTime`
//! composes one `calendarView` query per calendar with pure slot computation,
//! so "find a 30-minute slot next week" finishes in one tool call instead of
//! forcing the agent to list events and do interval math by hand.
//!
//! Every endpoint shape, permission and response code below was verified in
//! the official Microsoft Graph docs (see README.md); nothing is assumed.
//! `getSchedule` is deliberately NOT used: the docs mark it as unsupported
//! for delegated personal Microsoft accounts, while `calendarView` serves
//! both account types under the same scope.
//!
//! The host runs the OAuth 2.0 sign-in, refreshes the token and adds
//! `Authorization: Bearer …` after each request leaves the guest: this code
//! never sees it.
//!
//! [`OutlookCalendar::call_with`] injects the sender so multi-step tools stay
//! unit-testable: the guest sends requests over HTTP, while tests answer
//! from canned responses. Each tool is `plan_*` / `shape_*` / `combine_*`
//! pure helpers around that seam.

use connectors_sdk::http::{Method, Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::Deserialize;
use serde_json::{Map, Value, json};

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://graph.microsoft.com";

/// The connector.
#[derive(Debug)]
pub struct OutlookCalendar;

fn default_calendar() -> String {
    "primary".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListEvents {
    #[serde(default = "default_calendar")]
    calendar_id: String,
    time_min: Option<String>,
    time_max: Option<String>,
    max_results: Option<i64>,
    time_zone: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetEvent {
    #[serde(default = "default_calendar")]
    calendar_id: String,
    event_id: String,
    time_zone: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttendeeInput {
    email: String,
    name: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateEvent {
    #[serde(default = "default_calendar")]
    calendar_id: String,
    subject: String,
    start: String,
    end: String,
    time_zone: String,
    location: Option<String>,
    description: Option<String>,
    attendees: Option<Vec<AttendeeInput>>,
    is_all_day: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateEvent {
    #[serde(default = "default_calendar")]
    calendar_id: String,
    event_id: String,
    subject: Option<String>,
    start: Option<String>,
    end: Option<String>,
    time_zone: Option<String>,
    location: Option<String>,
    description: Option<String>,
    attendees: Option<Vec<AttendeeInput>>,
    is_all_day: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteEvent {
    #[serde(default = "default_calendar")]
    calendar_id: String,
    event_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FindFreeTime {
    time_min: String,
    time_max: String,
    duration_minutes: i64,
    time_zone: String,
    calendar_ids: Option<Vec<String>>,
}

impl Connector for OutlookCalendar {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        Self::call_with(request, |request| Ok(request.clone().send()?))
    }
}

impl OutlookCalendar {
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
            "listEvents" => one_call(
                &api,
                &request,
                &mut send,
                plan_list_events,
                shape_event_list,
            ),
            "getEvent" => one_call(
                &api,
                &request,
                &mut send,
                plan_get_event,
                shape_event_single,
            ),
            "createEvent" => one_call(
                &api,
                &request,
                &mut send,
                plan_create_event,
                shape_event_single,
            ),
            "updateEvent" => one_call(
                &api,
                &request,
                &mut send,
                plan_update_event,
                shape_event_single,
            ),
            "deleteEvent" => one_call(&api, &request, &mut send, plan_delete_event, shape_deleted),
            "findFreeTime" => find_free_time(&api, &request, &mut send),
            other => Err(ConnectorError::NotFound(format!("no tool {other}"))),
        }
    }
}

connectors_sdk::export_connector!(OutlookCalendar);

/// The Graph calendar endpoints derived from the configured base URL.
struct Api {
    /// `{base}/v1.0`, the root of every endpoint.
    root: String,
}

impl Api {
    fn new(base: &str) -> Self {
        Self {
            root: format!("{}/v1.0", base.trim_end_matches('/')),
        }
    }

    /// The URL of a calendar: the default calendar for `"primary"`.
    fn calendar(&self, calendar_id: &str) -> String {
        if calendar_id == "primary" {
            format!("{}/me/calendar", self.root)
        } else {
            format!("{}/me/calendars/{}", self.root, encode(calendar_id))
        }
    }

    /// The URL of a calendar's event collection.
    fn events(&self, calendar_id: &str) -> String {
        format!("{}/events", self.calendar(calendar_id))
    }

    /// The URL of one event's resource.
    fn event(&self, calendar_id: &str, event_id: &str) -> String {
        format!("{}/{}", self.events(calendar_id), encode(event_id))
    }

    /// The URL of a calendar's expanded view over a time range: occurrences,
    /// exceptions and single instances.
    fn calendar_view(&self, calendar_id: &str) -> String {
        format!("{}/calendarView", self.calendar(calendar_id))
    }
}

/// Plans a single-request tool's call, sends it and answers the shaped
/// response.
fn one_call(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
    plan: fn(&Api, &ToolRequest) -> Result<Request, ConnectorError>,
    shape: fn(&Value) -> Result<Value, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let planned = plan(api, request)?;
    answer_shaped(&send(&planned)?, shape)
}

/// Plans `listEvents`: `calendarView` for a time window (recurring events
/// expanded into occurrences), the plain event collection otherwise.
fn plan_list_events(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: ListEvents = request.json()?;
    let select = "id,subject,bodyPreview,start,end,location,attendees,isAllDay,showAs";
    let mut planned = if input.time_min.is_some() || input.time_max.is_some() {
        let (Some(time_min), Some(time_max)) = (&input.time_min, &input.time_max) else {
            return Err(invalid("timeMin and timeMax must be given together"));
        };
        json_get(with_query(
            &api.calendar_view(&input.calendar_id),
            &[
                ("startDateTime", Some(time_min.clone())),
                ("endDateTime", Some(time_max.clone())),
                ("$select", Some(select.to_string())),
                ("$top", input.max_results.map(|n| n.to_string())),
            ],
        ))
    } else {
        json_get(with_query(
            &api.events(&input.calendar_id),
            &[
                ("$select", Some(select.to_string())),
                ("$top", input.max_results.map(|n| n.to_string())),
                ("$orderby", Some("createdDateTime desc".to_string())),
            ],
        ))
    };
    if let Some(time_zone) = &input.time_zone {
        planned = planned.header("Prefer", &format!("outlook.timezone=\"{time_zone}\""));
    }
    Ok(planned)
}

/// Plans `getEvent`: the full event, with the description as plain text.
fn plan_get_event(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: GetEvent = request.json()?;
    let url = with_query(
        &api.event(&input.calendar_id, &input.event_id),
        &[(
            "$select",
            Some(
                "id,subject,body,start,end,location,attendees,organizer,isAllDay,showAs,webLink"
                    .to_string(),
            ),
        )],
    );
    let mut planned = json_get(url).header("Prefer", "outlook.body-content-type=\"text\"");
    if let Some(time_zone) = &input.time_zone {
        planned = planned.header("Prefer", &format!("outlook.timezone=\"{time_zone}\""));
    }
    Ok(planned)
}

/// Plans `createEvent`: the flat tool fields become a Graph event.
fn plan_create_event(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: CreateEvent = request.json()?;
    let body = create_body(&input)?;
    Ok(Request::post(api.events(&input.calendar_id))
        .header("accept", "application/json")
        .json(&body)?) // coverage: exclude — Request::json serializes a serde_json::Value here, which is infallible
}

/// Plans `updateEvent`: only the given fields become the PATCH body.
fn plan_update_event(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: UpdateEvent = request.json()?;
    let body = update_body(&input)?;
    Ok(Request::new(
        Method::Patch,
        api.event(&input.calendar_id, &input.event_id),
    )
    .header("accept", "application/json")
    .json(&body)?) // coverage: exclude — Request::json serializes a serde_json::Value here, which is infallible
}

/// Plans `deleteEvent`.
fn plan_delete_event(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: DeleteEvent = request.json()?;
    Ok(Request::new(
        Method::Delete,
        api.event(&input.calendar_id, &input.event_id),
    )
    .header("accept", "application/json"))
}

/// The tool result of the API's response: 204 becomes a deletion
/// confirmation, other 2xx answers are shaped, and anything else is a tool
/// error naming the status so the model can react (a 403 usually means a
/// missing scope).
fn answer_shaped(
    response: &Response,
    shape: fn(&Value) -> Result<Value, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
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
    ToolResponse::structured(&shape(&body)?)
}

// ---------------------------------------------------------------------------
// Shaping: raw event JSON never leaves the connector
// ---------------------------------------------------------------------------

/// Shapes a `listEvents` answer into `{events: [...]}`.
fn shape_event_list(body: &Value) -> Result<Value, ConnectorError> {
    let Some(events) = body.get("value").and_then(Value::as_array) else {
        return Err(ConnectorError::Upstream(
            "the API answered an event list without \"value\"".into(),
        ));
    };
    Ok(json!({ "events": events.iter().map(shape_event).collect::<Vec<_>>() }))
}

/// Shapes a single event answer (get/create/update).
fn shape_event_single(body: &Value) -> Result<Value, ConnectorError> {
    Ok(shape_event(body))
}

/// Shapes a `deleteEvent` 200-with-body answer (204 is handled earlier).
fn shape_deleted(_body: &Value) -> Result<Value, ConnectorError> {
    Ok(json!({ "deleted": true }))
}

/// Flattens one Graph event into the agent-facing shape, skipping whatever
/// the API did not return.
fn shape_event(event: &Value) -> Value {
    let mut out = Map::new();
    put(&mut out, "id", &event["id"]);
    put(&mut out, "subject", &event["subject"]);
    put(&mut out, "start", &event["start"]);
    put(&mut out, "end", &event["end"]);
    put(&mut out, "isAllDay", &event["isAllDay"]);
    put(&mut out, "showAs", &event["showAs"]);
    if let Some(name) = event
        .get("location")
        .and_then(|location| location.get("displayName"))
    {
        put(&mut out, "location", name);
    }
    if let Some(attendees) = event.get("attendees").and_then(Value::as_array) {
        out.insert(
            "attendees".into(),
            attendees.iter().map(shape_attendee).collect(),
        );
    }
    if let Some(body) = event.get("body")
        && let Some(content) = body.get("content").and_then(Value::as_str)
    {
        out.insert("description".into(), content.into());
        if let Some(content_type) = body.get("contentType").and_then(Value::as_str) {
            out.insert("descriptionType".into(), content_type.into());
        }
    }
    if let Some(preview) = event.get("bodyPreview").and_then(Value::as_str) {
        out.insert("preview".into(), preview.into());
    }
    if let Some(link) = event.get("webLink").and_then(Value::as_str) {
        out.insert("webLink".into(), link.into());
    }
    Value::Object(out)
}

/// Flattens one Graph attendee.
fn shape_attendee(attendee: &Value) -> Value {
    let mut out = Map::new();
    if let Some(email) = attendee.get("emailAddress") {
        put(&mut out, "name", &email["name"]);
        put(&mut out, "address", &email["address"]);
    }
    put(&mut out, "type", &attendee["type"]);
    Value::Object(out)
}

/// Inserts `value` unless it is null.
fn put(out: &mut Map<String, Value>, key: &str, value: &Value) {
    if !value.is_null() {
        out.insert(key.to_string(), value.clone());
    }
}

// ---------------------------------------------------------------------------
// Writing events: flat tool fields become Graph request bodies
// ---------------------------------------------------------------------------

/// Builds the `createEvent` Graph body from the flat tool fields.
fn create_body(input: &CreateEvent) -> Result<Value, ConnectorError> {
    let all_day = input.is_all_day.unwrap_or(false);
    let mut body = Map::new();
    body.insert("subject".into(), input.subject.clone().into());
    if let Some(description) = &input.description {
        body.insert(
            "body".into(),
            json!({ "contentType": "Text", "content": description }),
        );
    }
    body.insert(
        "start".into(),
        json!({
            "dateTime": event_time(&input.start, all_day, "start")?,
            "timeZone": input.time_zone,
        }),
    );
    body.insert(
        "end".into(),
        json!({
            "dateTime": event_time(&input.end, all_day, "end")?,
            "timeZone": input.time_zone,
        }),
    );
    if let Some(location) = &input.location {
        body.insert("location".into(), json!({ "displayName": location }));
    }
    if let Some(attendees) = &input.attendees {
        body.insert(
            "attendees".into(),
            attendees
                .iter()
                .map(graph_attendee)
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        );
    }
    if all_day {
        body.insert("isAllDay".into(), true.into());
    }
    Ok(Value::Object(body))
}

/// Builds the `updateEvent` PATCH body: only the given fields.
fn update_body(input: &UpdateEvent) -> Result<Value, ConnectorError> {
    let mut body = Map::new();
    if let Some(subject) = &input.subject {
        body.insert("subject".into(), subject.clone().into());
    }
    if let Some(description) = &input.description {
        body.insert(
            "body".into(),
            json!({ "contentType": "Text", "content": description }),
        );
    }
    if input.start.is_some() || input.end.is_some() {
        let time_zone = input
            .time_zone
            .as_deref()
            .ok_or_else(|| invalid("timeZone is required when start or end is given"))?;
        let all_day = input.is_all_day.unwrap_or(false);
        if let Some(start) = &input.start {
            body.insert(
                "start".into(),
                json!({
                    "dateTime": event_time(start, all_day, "start")?,
                    "timeZone": time_zone,
                }),
            );
        }
        if let Some(end) = &input.end {
            body.insert(
                "end".into(),
                json!({
                    "dateTime": event_time(end, all_day, "end")?,
                    "timeZone": time_zone,
                }),
            );
        }
    }
    if let Some(location) = &input.location {
        body.insert("location".into(), json!({ "displayName": location }));
    }
    if let Some(attendees) = &input.attendees {
        body.insert(
            "attendees".into(),
            attendees
                .iter()
                .map(graph_attendee)
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        );
    }
    if let Some(all_day) = input.is_all_day {
        body.insert("isAllDay".into(), all_day.into());
    }
    Ok(Value::Object(body))
}

/// Converts an RFC3339 timestamp (or a date for all-day events) into the
/// wall time Graph's `dateTime` wants.
fn event_time(text: &str, all_day: bool, field: &str) -> Result<String, ConnectorError> {
    if all_day {
        if valid_date(text) {
            Ok(text.to_string())
        } else {
            Err(invalid(format!(
                "{field} must be a date (YYYY-MM-DD) for an all-day event"
            )))
        }
    } else if parse_moment(text).is_some() {
        Ok(text[..19].to_string())
    } else {
        Err(invalid(format!(
            "{field} must be RFC3339, e.g. 2026-10-12T14:00:00-07:00"
        )))
    }
}

/// Whether `text` is a real calendar date in `YYYY-MM-DD` shape.
fn valid_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let (Some(year), Some(month), Some(day)) = (
        digits(bytes, 0, 4),
        digits(bytes, 5, 2),
        digits(bytes, 8, 2),
    ) else {
        return false;
    };
    let Some(month_days) = days_in_month(month, year) else {
        return false;
    };
    day >= 1 && day <= month_days
}

/// Converts one flat attendee into the Graph shape.
fn graph_attendee(attendee: &AttendeeInput) -> Result<Value, ConnectorError> {
    let kind = attendee.kind.as_deref().unwrap_or("required");
    if kind != "required" && kind != "optional" {
        return Err(invalid(
            "attendee type must be \"required\" or \"optional\"",
        ));
    }
    let mut email = Map::new();
    email.insert("address".into(), attendee.email.clone().into());
    if let Some(name) = &attendee.name {
        email.insert("name".into(), name.clone().into());
    }
    Ok(json!({ "emailAddress": Value::Object(email), "type": kind }))
}

// ---------------------------------------------------------------------------
// findFreeTime: one calendarView query per calendar, then pure slot computation
// ---------------------------------------------------------------------------

/// Runs `findFreeTime`: validates the search, asks `calendarView` once per
/// calendar, and answers the free slots of at least the wanted length.
fn find_free_time(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: FindFreeTime = request.json()?;
    let search = FreeTimeSearch::of(input)?;
    let mut busy = Vec::new();
    for calendar in &search.calendars {
        let response = send(&search.view_request(api, calendar))?;
        let events = match step_events(&response)? {
            Ok(events) => events,
            Err(error) => return Ok(error),
        };
        busy.extend(event_intervals(&events)?);
    }
    combine_free_slots(&busy, &search)
}

/// A validated `findFreeTime` search: the window in zone-epoch seconds (the
/// wall clock read as UTC, so it compares directly with the event wall times
/// the API renders in the requested time zone), plus the window's own offset
/// for formatting answers back as RFC3339.
struct FreeTimeSearch {
    time_min: String,
    time_max: String,
    time_zone: String,
    calendars: Vec<String>,
    start_zone: i64,
    end_zone: i64,
    offset_minutes: i32,
    duration_secs: i64,
}

impl FreeTimeSearch {
    /// Parses the tool input into a validated search. Anything the agent got
    /// wrong is [`ConnectorError::InvalidInput`].
    fn of(input: FindFreeTime) -> Result<Self, ConnectorError> {
        let start = parse_moment(&input.time_min)
            .ok_or_else(|| invalid("timeMin must be RFC3339, e.g. 2026-10-12T09:00:00-07:00"))?;
        let end = parse_moment(&input.time_max)
            .ok_or_else(|| invalid("timeMax must be RFC3339, e.g. 2026-10-12T17:00:00-07:00"))?;
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
            time_zone: input.time_zone,
            calendars,
            // The wall clock as UTC: epoch + offset undoes parse_moment's
            // conversion, leaving the wall time the answers share.
            start_zone: start.epoch + i64::from(start.offset_minutes) * 60,
            end_zone: end.epoch + i64::from(end.offset_minutes) * 60,
            offset_minutes: start.offset_minutes,
            duration_secs,
        })
    }

    /// Plans the search's `calendarView` query for one calendar, asking the
    /// API to render event wall times in the requested time zone.
    fn view_request(&self, api: &Api, calendar: &str) -> Request {
        let time_zone = &self.time_zone;
        let url = with_query(
            &api.calendar_view(calendar),
            &[
                ("startDateTime", Some(self.time_min.clone())),
                ("endDateTime", Some(self.time_max.clone())),
                (
                    "$select",
                    Some("id,start,end,showAs,isCancelled".to_string()),
                ),
                ("$top", Some("100".to_string())),
            ],
        );
        json_get(url).header("Prefer", &format!("outlook.timezone=\"{time_zone}\""))
    }
}

/// The event collection of one `calendarView` answer: 2xx continues the
/// tool, anything else becomes a tool error naming the failed step.
fn step_events(response: &Response) -> Result<Result<Value, ToolResponse>, ConnectorError> {
    if !(200..300).contains(&response.status) {
        let body = String::from_utf8_lossy(&response.body);
        return Ok(Err(ToolResponse::error(format!(
            "finding free time failed: the API answered {}: {body}",
            response.status
        ))));
    }
    Ok(Ok(response.json()?))
}

/// Busy zone-epoch intervals of one `calendarView` answer. Events the user
/// marked free and cancelled events never block a slot; an unparseable
/// timestamp is upstream garbage.
fn event_intervals(events: &Value) -> Result<Vec<(i64, i64)>, ConnectorError> {
    let Some(list) = events.get("value").and_then(Value::as_array) else {
        return Err(ConnectorError::Upstream(
            "the API answered an event list without \"value\"".into(),
        ));
    };
    let mut intervals = Vec::new();
    for event in list {
        if event.get("showAs").and_then(Value::as_str) == Some("free") {
            continue;
        }
        if event.get("isCancelled").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let start = event_moment(event, "start")?;
        let end = event_moment(event, "end")?;
        if end > start {
            intervals.push((start, end));
        }
    }
    Ok(intervals)
}

/// One event's start or end as zone-epoch seconds.
fn event_moment(event: &Value, field: &str) -> Result<i64, ConnectorError> {
    let text = event
        .get(field)
        .and_then(|moment| moment.get("dateTime"))
        .and_then(Value::as_str);
    wall_to_zone_epoch(text.unwrap_or("")).ok_or_else(|| {
        ConnectorError::Upstream(format!(
            "the API answered an unparseable event {field}: {}",
            text.unwrap_or("missing")
        ))
    })
}

/// The zone-epoch of an event's `dateTime` wall time: seconds since the Unix
/// epoch of the wall clock read as UTC, so wall times the API rendered in
/// the requested time zone compare directly. `None` for anything that is not
/// a `YYYY-MM-DDTHH:MM:SS` wall time (a fractional second may follow).
fn wall_to_zone_epoch(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 || bytes[10] != b'T' {
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
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let month_days = days_in_month(month, year)?;
    if day < 1 || day > month_days {
        return None;
    }
    Some(
        days_from_civil(i64::from(year), i64::from(month), i64::from(day)) * 86_400
            + i64::from(hour) * 3_600
            + i64::from(minute) * 60
            + i64::from(second),
    )
}

/// Combines the search into its answer: the free slots of at least the
/// wanted length inside the window, in chronological order, formatted as
/// RFC3339 in the window's offset.
fn combine_free_slots(
    busy: &[(i64, i64)],
    search: &FreeTimeSearch,
) -> Result<ToolResponse, ConnectorError> {
    let offset = i64::from(search.offset_minutes);
    let slots: Vec<Value> = free_slots(
        busy,
        search.start_zone,
        search.end_zone,
        search.duration_secs,
    )
    .into_iter()
    .map(|(start, end)| {
        json!({
            // Zone-epoch minus the offset is the true epoch whose wall
            // clock in the window's offset prints the slot's wall time.
            "start": format_moment(start - offset * 60, search.offset_minutes),
            "end": format_moment(end - offset * 60, search.offset_minutes),
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
fn invalid(message: impl Into<String>) -> ConnectorError {
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
        OutlookCalendar::call_with(request(tool, body), |request| stub.send(request))
    }

    fn prefer(headers: &[(String, String)], name: &str) -> Vec<String> {
        headers
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .collect()
    }

    const API: &str = "https://graph.microsoft.com/v1.0";

    #[test]
    fn lists_events_in_a_window() {
        // Arrange: a window search with every parameter set.
        let mut stub = Stub::of(&[ok_json(r#"{"value":[]}"#)]);

        // Act.
        let answered = call(
            "listEvents",
            r#"{"calendarId":"primary","timeMin":"2026-10-12T00:00:00-07:00","timeMax":"2026-10-19T00:00:00-07:00","maxResults":25,"timeZone":"Pacific Standard Time"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: calendarView with the window, rendered in the given zone.
        assert_eq!(answered.structured.as_deref(), Some(r#"{"events":[]}"#));
        assert_eq!(stub.seen.len(), 1);
        assert_eq!(stub.seen[0].method, Method::Get);
        assert_eq!(
            stub.seen[0].url,
            format!(
                "{API}/me/calendar/calendarView?startDateTime=2026-10-12T00%3A00%3A00-07%3A00&endDateTime=2026-10-19T00%3A00%3A00-07%3A00&$select=id%2Csubject%2CbodyPreview%2Cstart%2Cend%2Clocation%2Cattendees%2CisAllDay%2CshowAs&$top=25"
            )
        );
        assert_eq!(
            prefer(&stub.seen[0].headers, "Prefer"),
            vec!["outlook.timezone=\"Pacific Standard Time\"".to_string()]
        );
    }

    #[test]
    fn lists_events_in_a_named_calendar() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"value":[]}"#)]);

        // Act: a calendar ID needing percent-encoding, no time zone.
        call(
            "listEvents",
            r#"{"calendarId":"work calendar","timeMin":"2026-10-12T00:00:00-07:00","timeMax":"2026-10-19T00:00:00-07:00"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the ID is encoded, and no Prefer header is sent.
        assert!(
            stub.seen[0]
                .url
                .starts_with(&format!("{API}/me/calendars/work%20calendar/calendarView?"))
        );
        assert!(prefer(&stub.seen[0].headers, "Prefer").is_empty());
    }

    #[test]
    fn lists_recent_events_without_a_window() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"value":[]}"#)]);

        // Act.
        call(
            "listEvents",
            r#"{"calendarId":"primary","maxResults":10}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the plain event collection, newest first.
        assert_eq!(
            stub.seen[0].url,
            format!(
                "{API}/me/calendar/events?$select=id%2Csubject%2CbodyPreview%2Cstart%2Cend%2Clocation%2Cattendees%2CisAllDay%2CshowAs&$top=10&$orderby=createdDateTime%20desc"
            )
        );
    }

    #[test]
    fn lists_events_with_defaults() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"value":[]}"#)]);

        // Act: no calendarId either, so the default applies.
        call("listEvents", r#"{}"#, &mut stub).unwrap();

        // Assert: primary calendar, no $top, JSON asked for.
        assert_eq!(
            stub.seen[0].url,
            format!(
                "{API}/me/calendar/events?$select=id%2Csubject%2CbodyPreview%2Cstart%2Cend%2Clocation%2Cattendees%2CisAllDay%2CshowAs&$orderby=createdDateTime%20desc"
            )
        );
        assert_eq!(
            prefer(&stub.seen[0].headers, "accept"),
            vec!["application/json".to_string()]
        );
    }

    #[test]
    fn list_rejects_a_half_window() {
        // Arrange.
        let mut stub = Stub::of(&[]);

        // Act: timeMin without timeMax.
        let rejected = call(
            "listEvents",
            r#"{"timeMin":"2026-10-12T00:00:00-07:00"}"#,
            &mut stub,
        );

        // Assert: invalid input, and nothing was sent.
        assert_eq!(
            rejected.unwrap_err(),
            ConnectorError::InvalidInput("timeMin and timeMax must be given together".into())
        );
        assert!(stub.seen.is_empty());
    }

    #[test]
    fn gets_an_event() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(
            r#"{"id":"e1","subject":"Focus","body":{"contentType":"text","content":"Deep work"},"start":{"dateTime":"2026-10-12T14:00:00.0000000","timeZone":"Pacific Standard Time"},"end":{"dateTime":"2026-10-12T15:00:00.0000000","timeZone":"Pacific Standard Time"},"isAllDay":false}"#,
        )]);

        // Act.
        let answered = call(
            "getEvent",
            r#"{"calendarId":"primary","eventId":"e1","timeZone":"Pacific Standard Time"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the event URL, both Prefer headers, and the shaped answer.
        assert_eq!(
            stub.seen[0].url,
            format!(
                "{API}/me/calendar/events/e1?$select=id%2Csubject%2Cbody%2Cstart%2Cend%2Clocation%2Cattendees%2Corganizer%2CisAllDay%2CshowAs%2CwebLink"
            )
        );
        assert_eq!(
            prefer(&stub.seen[0].headers, "Prefer"),
            vec![
                "outlook.body-content-type=\"text\"".to_string(),
                "outlook.timezone=\"Pacific Standard Time\"".to_string(),
            ]
        );
        let shaped: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(shaped["id"], json!("e1"));
        assert_eq!(shaped["subject"], json!("Focus"));
        assert_eq!(shaped["description"], json!("Deep work"));
        assert_eq!(shaped["descriptionType"], json!("text"));
        assert_eq!(shaped["isAllDay"], json!(false));
    }

    #[test]
    fn gets_an_event_from_a_named_calendar() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"id":"e1","subject":"Focus"}"#)]);

        // Act: no time zone, so only the body-content-type Prefer header.
        call(
            "getEvent",
            r#"{"calendarId":"work","eventId":"e1"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert!(
            stub.seen[0]
                .url
                .starts_with(&format!("{API}/me/calendars/work/events/e1?"))
        );
        assert_eq!(
            prefer(&stub.seen[0].headers, "Prefer"),
            vec!["outlook.body-content-type=\"text\"".to_string()]
        );
    }

    #[test]
    fn creates_an_event() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"id":"e9","subject":"Focus"}"#)]);

        // Act.
        let answered = call(
            "createEvent",
            r#"{"calendarId":"primary","subject":"Focus","start":"2026-10-12T14:00:00-07:00","end":"2026-10-12T15:00:00-07:00","timeZone":"Pacific Standard Time","location":"Room A","description":"Deep work","attendees":[{"email":"a@example.com","name":"A","type":"required"},{"email":"b@example.com","type":"optional"}]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: POST to the collection, and the flat fields became Graph.
        assert_eq!(stub.seen[0].url, format!("{API}/me/calendar/events"));
        assert_eq!(stub.seen[0].method, Method::Post);
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(body["subject"], json!("Focus"));
        assert_eq!(
            body["start"],
            json!({"dateTime": "2026-10-12T14:00:00", "timeZone": "Pacific Standard Time"})
        );
        assert_eq!(
            body["end"],
            json!({"dateTime": "2026-10-12T15:00:00", "timeZone": "Pacific Standard Time"})
        );
        assert_eq!(body["location"], json!({"displayName": "Room A"}));
        assert_eq!(
            body["body"],
            json!({"contentType": "Text", "content": "Deep work"})
        );
        assert_eq!(
            body["attendees"],
            json!([
                {"emailAddress": {"address": "a@example.com", "name": "A"}, "type": "required"},
                {"emailAddress": {"address": "b@example.com"}, "type": "optional"},
            ])
        );
        assert!(body.get("isAllDay").is_none());
        // The 201 answer is shaped.
        let shaped: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(shaped["id"], json!("e9"));
    }

    #[test]
    fn creates_a_minimal_event() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"id":"e9"}"#)]);

        // Act: only the required fields.
        call(
            "createEvent",
            r#"{"subject":"Focus","start":"2026-10-12T14:00:00-07:00","end":"2026-10-12T15:00:00-07:00","timeZone":"Pacific Standard Time"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: optional fields stay out of the body, default calendar used.
        assert_eq!(stub.seen[0].url, format!("{API}/me/calendar/events"));
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert!(body.get("location").is_none());
        assert!(body.get("body").is_none());
        assert!(body.get("attendees").is_none());
        assert!(body.get("isAllDay").is_none());
    }

    #[test]
    fn creates_an_all_day_event() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"id":"e9"}"#)]);

        // Act.
        call(
            "createEvent",
            r#"{"subject":"Holiday","start":"2026-12-25","end":"2026-12-26","timeZone":"Pacific Standard Time","isAllDay":true}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: dates stay dates, and the all-day flag is set.
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            body["start"],
            json!({"dateTime": "2026-12-25", "timeZone": "Pacific Standard Time"})
        );
        assert_eq!(
            body["end"],
            json!({"dateTime": "2026-12-26", "timeZone": "Pacific Standard Time"})
        );
        assert_eq!(body["isAllDay"], json!(true));
    }

    #[test]
    fn create_rejects_bad_input() {
        // Arrange: every invalid shape the validation guards.
        let cases = [
            // start is not RFC3339.
            (
                r#"{"subject":"x","start":"tomorrow","end":"2026-10-12T15:00:00-07:00","timeZone":"Pacific Standard Time"}"#,
                "start must be RFC3339, e.g. 2026-10-12T14:00:00-07:00",
            ),
            // end is not RFC3339.
            (
                r#"{"subject":"x","start":"2026-10-12T14:00:00-07:00","end":"later","timeZone":"Pacific Standard Time"}"#,
                "end must be RFC3339, e.g. 2026-10-12T14:00:00-07:00",
            ),
            // An all-day start must be a date.
            (
                r#"{"subject":"x","start":"2026-10-12T14:00:00-07:00","end":"2026-12-26","timeZone":"Pacific Standard Time","isAllDay":true}"#,
                "start must be a date (YYYY-MM-DD) for an all-day event",
            ),
            // An all-day end must be a real date.
            (
                r#"{"subject":"x","start":"2026-12-25","end":"2026-12-40","timeZone":"Pacific Standard Time","isAllDay":true}"#,
                "end must be a date (YYYY-MM-DD) for an all-day event",
            ),
            // Attendee types are closed.
            (
                r#"{"subject":"x","start":"2026-10-12T14:00:00-07:00","end":"2026-10-12T15:00:00-07:00","timeZone":"Pacific Standard Time","attendees":[{"email":"a@example.com","type":"maybe"}]}"#,
                "attendee type must be \"required\" or \"optional\"",
            ),
        ];
        for (body, message) in cases {
            // Arrange.
            let mut stub = Stub::of(&[]);

            // Act.
            let rejected = call("createEvent", body, &mut stub);

            // Assert: invalid input, and nothing was sent.
            assert_eq!(
                rejected.unwrap_err(),
                ConnectorError::InvalidInput(message.into()),
                "{body}"
            );
            assert!(stub.seen.is_empty(), "{body}");
        }
    }

    #[test]
    fn updates_an_event() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(
            r#"{"id":"e1","location":{"displayName":"Room B"}}"#,
        )]);

        // Act: every updatable field at once.
        let answered = call(
            "updateEvent",
            r#"{"calendarId":"primary","eventId":"e1","subject":"New","start":"2026-10-13T14:00:00-07:00","end":"2026-10-13T15:00:00-07:00","timeZone":"Pacific Standard Time","location":"Room B","description":"New desc","attendees":[{"email":"c@example.com"}],"isAllDay":false}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: PATCH with only the given fields, shaped answer.
        assert_eq!(stub.seen[0].url, format!("{API}/me/calendar/events/e1"));
        assert_eq!(stub.seen[0].method, Method::Patch);
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(body["subject"], json!("New"));
        assert_eq!(
            body["start"],
            json!({"dateTime": "2026-10-13T14:00:00", "timeZone": "Pacific Standard Time"})
        );
        assert_eq!(
            body["end"],
            json!({"dateTime": "2026-10-13T15:00:00", "timeZone": "Pacific Standard Time"})
        );
        assert_eq!(body["location"], json!({"displayName": "Room B"}));
        assert_eq!(
            body["body"],
            json!({"contentType": "Text", "content": "New desc"})
        );
        assert_eq!(
            body["attendees"],
            json!([{"emailAddress": {"address": "c@example.com"}, "type": "required"}])
        );
        assert_eq!(body["isAllDay"], json!(false));
        let shaped: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(shaped["location"], json!("Room B"));
    }

    #[test]
    fn updates_only_the_given_fields() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"id":"e1"}"#)]);

        // Act: a subject-only patch.
        call(
            "updateEvent",
            r#"{"eventId":"e1","subject":"New"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the body carries nothing else.
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(body, json!({"subject": "New"}));
    }

    #[test]
    fn updates_just_the_start() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"id":"e1"}"#)]);

        // Act: start without end.
        call(
            "updateEvent",
            r#"{"eventId":"e1","start":"2026-10-13T14:00:00-07:00","timeZone":"Pacific Standard Time"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            body,
            json!({"start": {"dateTime": "2026-10-13T14:00:00", "timeZone": "Pacific Standard Time"}})
        );
    }

    #[test]
    fn updates_just_the_end() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"id":"e1"}"#)]);

        // Act: end without start still needs the time zone.
        call(
            "updateEvent",
            r#"{"eventId":"e1","end":"2026-10-13T16:00:00-07:00","timeZone":"Pacific Standard Time"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            body,
            json!({"end": {"dateTime": "2026-10-13T16:00:00", "timeZone": "Pacific Standard Time"}})
        );
    }

    #[test]
    fn update_rejects_bad_input() {
        // Arrange: every invalid shape the validation guards.
        let cases = [
            // A changed start needs its time zone.
            (
                r#"{"eventId":"e1","start":"2026-10-13T14:00:00-07:00"}"#,
                "timeZone is required when start or end is given",
            ),
            // A changed end needs its time zone too.
            (
                r#"{"eventId":"e1","end":"2026-10-13T15:00:00-07:00"}"#,
                "timeZone is required when start or end is given",
            ),
            // The start must parse.
            (
                r#"{"eventId":"e1","start":"soon","timeZone":"Pacific Standard Time"}"#,
                "start must be RFC3339, e.g. 2026-10-12T14:00:00-07:00",
            ),
            // The end must parse.
            (
                r#"{"eventId":"e1","end":"soon","timeZone":"Pacific Standard Time"}"#,
                "end must be RFC3339, e.g. 2026-10-12T14:00:00-07:00",
            ),
            // Attendee types are closed here too.
            (
                r#"{"eventId":"e1","attendees":[{"email":"a@example.com","type":"maybe"}]}"#,
                "attendee type must be \"required\" or \"optional\"",
            ),
        ];
        for (body, message) in cases {
            // Arrange.
            let mut stub = Stub::of(&[]);

            // Act.
            let rejected = call("updateEvent", body, &mut stub);

            // Assert: invalid input, and nothing was sent.
            assert_eq!(
                rejected.unwrap_err(),
                ConnectorError::InvalidInput(message.into()),
                "{body}"
            );
            assert!(stub.seen.is_empty(), "{body}");
        }
    }

    #[test]
    fn deletes_an_event() {
        // Arrange.
        let mut stub = Stub::of(&[response(204, "")]);

        // Act.
        let answered = call(
            "deleteEvent",
            r#"{"calendarId":"primary","eventId":"e1"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: the API's 204 becomes a deletion confirmation.
        assert_eq!(answered.structured.as_deref(), Some(r#"{"deleted":true}"#));
        assert_eq!(stub.seen[0].url, format!("{API}/me/calendar/events/e1"));
        assert_eq!(stub.seen[0].method, Method::Delete);
    }

    #[test]
    fn deletes_from_a_named_calendar() {
        // Arrange: Graph answers 200 with a body here instead of 204.
        let mut stub = Stub::of(&[ok_json(r#"{"id":"e1"}"#)]);

        // Act.
        let answered = call(
            "deleteEvent",
            r#"{"calendarId":"work","eventId":"e1"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: still a deletion confirmation.
        assert_eq!(answered.structured.as_deref(), Some(r#"{"deleted":true}"#));
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/me/calendars/work/events/e1")
        );
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
        let ok = answer_shaped(
            &response(200, r#"{"id":"e1","subject":"Focus"}"#),
            shape_event_single,
        )
        .unwrap();
        assert_eq!(
            ok.structured.as_deref(),
            Some(r#"{"id":"e1","subject":"Focus"}"#)
        );
        let gone = answer_shaped(&response(204, ""), shape_event_single).unwrap();
        assert_eq!(gone.structured.as_deref(), Some(r#"{"deleted":true}"#));
        let refused =
            answer_shaped(&response(403, "insufficient scope"), shape_event_single).unwrap();
        assert_eq!(
            refused,
            ToolResponse::error("The API answered 403: insufficient scope")
        );
        let garbled = answer_shaped(&response(200, "<html>"), shape_event_single);
        assert!(
            matches!(garbled, Err(ConnectorError::Upstream(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{garbled:?}"
        );
    }

    #[test]
    fn shape_event_list_requires_a_value_collection() {
        // Arrange: a 200 without the "value" collection.
        let mut stub = Stub::of(&[ok_json(r#"{"nope":[]}"#)]);

        // Act.
        let failed = call("listEvents", r#"{}"#, &mut stub);

        // Assert: upstream garbage stays an upstream error.
        assert_eq!(
            failed.unwrap_err(),
            ConnectorError::Upstream("the API answered an event list without \"value\"".into())
        );
    }

    #[test]
    fn shapes_a_full_event() {
        // Arrange: every shaped field present.
        let mut stub = Stub::of(&[ok_json(
            r#"{"id":"e1","subject":"Lunch","start":{"dateTime":"2026-10-12T12:00:00.0000000","timeZone":"Pacific Standard Time"},"end":{"dateTime":"2026-10-12T13:00:00.0000000","timeZone":"Pacific Standard Time"},"isAllDay":false,"showAs":"busy","location":{"displayName":"Cafe"},"attendees":[{"type":"required","emailAddress":{"name":"A","address":"a@example.com"}},{"type":"optional"}],"bodyPreview":"See you","body":{"contentType":"html","content":"<p>See you</p>"},"webLink":"https://outlook/x"}"#,
        )]);

        // Act.
        let answered = call("getEvent", r#"{"eventId":"e1"}"#, &mut stub).unwrap();

        // Assert: the flat shape, with the attendee flattened too.
        let shaped: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(shaped["location"], json!("Cafe"));
        assert_eq!(shaped["showAs"], json!("busy"));
        assert_eq!(
            shaped["attendees"],
            json!([
                {"name": "A", "address": "a@example.com", "type": "required"},
                {"type": "optional"},
            ])
        );
        assert_eq!(shaped["description"], json!("<p>See you</p>"));
        assert_eq!(shaped["descriptionType"], json!("html"));
        assert_eq!(shaped["preview"], json!("See you"));
        assert_eq!(shaped["webLink"], json!("https://outlook/x"));
    }

    #[test]
    fn shapes_a_sparse_event() {
        // Arrange: an event missing every optional field.
        let mut stub = Stub::of(&[ok_json(r#"{"id":"e1","subject":"Lunch"}"#)]);

        // Act.
        let answered = call("getEvent", r#"{"eventId":"e1"}"#, &mut stub).unwrap();

        // Assert: absent fields stay absent, never null.
        let shaped: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(shaped, json!({"id": "e1", "subject": "Lunch"}));
    }

    #[test]
    fn shapes_an_event_with_an_unshaped_body() {
        // Arrange: a body whose content is not a string carries no description.
        let mut stub = Stub::of(&[ok_json(
            r#"{"id":"e1","body":{"contentType":"text","content":42}}"#,
        )]);

        // Act.
        let answered = call("getEvent", r#"{"eventId":"e1"}"#, &mut stub).unwrap();

        // Assert.
        let shaped: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(shaped, json!({"id": "e1"}));

        // Arrange: a body with content but no content type.
        let mut stub = Stub::of(&[ok_json(r#"{"id":"e1","body":{"content":"Hi"}}"#)]);

        // Act.
        let answered = call("getEvent", r#"{"eventId":"e1"}"#, &mut stub).unwrap();

        // Assert: the description stays, the type stays out.
        let shaped: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(shaped, json!({"id": "e1", "description": "Hi"}));
    }

    #[test]
    fn shapes_an_event_list() {
        // Arrange: two events, one sparse.
        let mut stub = Stub::of(&[ok_json(
            r#"{"value":[{"id":"e1","subject":"One"},{"id":"e2","subject":"Two","location":{"displayName":"Here"}}]}"#,
        )]);

        // Act.
        let answered = call("listEvents", r#"{}"#, &mut stub).unwrap();

        // Assert.
        let shaped: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(
            shaped,
            json!({"events": [
                {"id": "e1", "subject": "One"},
                {"id": "e2", "subject": "Two", "location": "Here"},
            ]})
        );
    }

    const FREE_VIEW: &str = r#"{"value":[
        {"id":"a","start":{"dateTime":"2026-10-12T10:00:00.0000000"},"end":{"dateTime":"2026-10-12T11:00:00.0000000"},"showAs":"busy"},
        {"id":"b","start":{"dateTime":"2026-10-12T13:00:00.0000000"},"end":{"dateTime":"2026-10-12T14:00:00.0000000"},"showAs":"free"},
        {"id":"c","start":{"dateTime":"2026-10-12T15:00:00.0000000"},"end":{"dateTime":"2026-10-12T16:00:00.0000000"},"showAs":"busy","isCancelled":true},
        {"id":"d","start":{"dateTime":"2026-10-12T16:00:00.0000000"},"end":{"dateTime":"2026-10-12T16:00:00.0000000"},"showAs":"busy"}
    ]}"#;
    const FREE_SEARCH: &str = r#"{"timeMin":"2026-10-12T09:00:00-07:00","timeMax":"2026-10-12T17:00:00-07:00","durationMinutes":60,"timeZone":"Pacific Standard Time"}"#;

    #[test]
    fn finds_free_time() {
        // Arrange: one busy block; free, cancelled and zero-width events block nothing.
        let mut stub = Stub::of(&[ok_json(FREE_VIEW)]);

        // Act.
        let answered = call("findFreeTime", FREE_SEARCH, &mut stub).unwrap();

        // Assert: one calendarView query in the requested zone, then the slots.
        assert_eq!(stub.seen.len(), 1);
        assert_eq!(stub.seen[0].method, Method::Get);
        assert_eq!(
            stub.seen[0].url,
            format!(
                "{API}/me/calendar/calendarView?startDateTime=2026-10-12T09%3A00%3A00-07%3A00&endDateTime=2026-10-12T17%3A00%3A00-07%3A00&$select=id%2Cstart%2Cend%2CshowAs%2CisCancelled&$top=100"
            )
        );
        assert_eq!(
            prefer(&stub.seen[0].headers, "Prefer"),
            vec!["outlook.timezone=\"Pacific Standard Time\"".to_string()]
        );
        assert_eq!(
            serde_json::from_str::<Value>(answered.structured.as_deref().unwrap()).unwrap(),
            json!({"slots":[
                {"start":"2026-10-12T09:00:00-07:00","end":"2026-10-12T10:00:00-07:00"},
                {"start":"2026-10-12T11:00:00-07:00","end":"2026-10-12T17:00:00-07:00"},
            ]})
        );
    }

    #[test]
    fn finds_free_time_across_calendars() {
        // Arrange: overlapping busy blocks on two calendars.
        let mut stub = Stub::of(&[
            ok_json(
                r#"{"value":[
                {"start":{"dateTime":"2026-10-12T10:00:00.0000000"},"end":{"dateTime":"2026-10-12T11:00:00.0000000"},"showAs":"busy"}
            ]}"#,
            ),
            ok_json(
                r#"{"value":[
                {"start":{"dateTime":"2026-10-12T10:30:00.0000000"},"end":{"dateTime":"2026-10-12T12:00:00.0000000"},"showAs":"busy"}
            ]}"#,
            ),
        ]);

        // Act.
        let answered = call(
            "findFreeTime",
            r#"{"timeMin":"2026-10-12T09:00:00-07:00","timeMax":"2026-10-12T17:00:00-07:00","durationMinutes":60,"timeZone":"Pacific Standard Time","calendarIds":["primary","team"]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: one query per calendar, busy blocks merged.
        assert_eq!(stub.seen.len(), 2);
        assert!(
            stub.seen[1]
                .url
                .starts_with(&format!("{API}/me/calendars/team/calendarView?"))
        );
        assert_eq!(
            serde_json::from_str::<Value>(answered.structured.as_deref().unwrap()).unwrap(),
            json!({"slots":[
                {"start":"2026-10-12T09:00:00-07:00","end":"2026-10-12T10:00:00-07:00"},
                {"start":"2026-10-12T12:00:00-07:00","end":"2026-10-12T17:00:00-07:00"},
            ]})
        );
    }

    #[test]
    fn find_free_time_rejects_bad_searches() {
        // Arrange: every invalid shape the validation guards.
        let cases = [
            // timeMin is not RFC3339.
            (
                r#"{"timeMin":"tomorrow","timeMax":"2026-10-12T17:00:00-07:00","durationMinutes":30,"timeZone":"Pacific Standard Time"}"#,
                "timeMin must be RFC3339",
            ),
            // timeMax is not RFC3339.
            (
                r#"{"timeMin":"2026-10-12T09:00:00-07:00","timeMax":"next week","durationMinutes":30,"timeZone":"Pacific Standard Time"}"#,
                "timeMax must be RFC3339",
            ),
            // The window ends when it starts.
            (
                r#"{"timeMin":"2026-10-12T09:00:00-07:00","timeMax":"2026-10-12T09:00:00-07:00","durationMinutes":30,"timeZone":"Pacific Standard Time"}"#,
                "timeMax must be after timeMin",
            ),
            // The window runs backwards.
            (
                r#"{"timeMin":"2026-10-12T17:00:00-07:00","timeMax":"2026-10-12T09:00:00-07:00","durationMinutes":30,"timeZone":"Pacific Standard Time"}"#,
                "timeMax must be after timeMin",
            ),
            // A zero-length slot is not a slot.
            (
                r#"{"timeMin":"2026-10-12T09:00:00-07:00","timeMax":"2026-10-12T17:00:00-07:00","durationMinutes":0,"timeZone":"Pacific Standard Time"}"#,
                "durationMinutes must be positive",
            ),
            // Neither is a negative one.
            (
                r#"{"timeMin":"2026-10-12T09:00:00-07:00","timeMax":"2026-10-12T17:00:00-07:00","durationMinutes":-15,"timeZone":"Pacific Standard Time"}"#,
                "durationMinutes must be positive",
            ),
            // Minutes times sixty must fit.
            (
                r#"{"timeMin":"2026-10-12T09:00:00-07:00","timeMax":"2026-10-12T17:00:00-07:00","durationMinutes":153722867280912931,"timeZone":"Pacific Standard Time"}"#,
                "durationMinutes is too large",
            ),
            // Searching no calendars searches nothing.
            (
                r#"{"timeMin":"2026-10-12T09:00:00-07:00","timeMax":"2026-10-12T17:00:00-07:00","durationMinutes":30,"timeZone":"Pacific Standard Time","calendarIds":[]}"#,
                "calendarIds must name at least one calendar",
            ),
            // A missing timeZone is not valid input either.
            (
                r#"{"timeMin":"2026-10-12T09:00:00-07:00","timeMax":"2026-10-12T17:00:00-07:00","durationMinutes":30}"#,
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
        // Arrange: the calendarView query is refused.
        let mut stub = Stub::of(&[response(403, "insufficient scope")]);

        // Act.
        let answered = call("findFreeTime", FREE_SEARCH, &mut stub).unwrap();

        // Assert: a tool error naming the failed step.
        assert_eq!(
            answered,
            ToolResponse::error(
                "finding free time failed: the API answered 403: insufficient scope"
            )
        );
    }

    #[test]
    fn find_free_time_rejects_answers_without_value() {
        // Arrange: a 200 without the "value" collection.
        let mut stub = Stub::of(&[ok_json(r#"{"nope":[]}"#)]);

        // Act.
        let failed = call("findFreeTime", FREE_SEARCH, &mut stub);

        // Assert: upstream garbage stays an upstream error.
        assert_eq!(
            failed.unwrap_err(),
            ConnectorError::Upstream("the API answered an event list without \"value\"".into())
        );
    }

    #[test]
    fn find_free_time_rejects_unparseable_event_times() {
        // Arrange: events the API sent but the wall clock cannot describe.
        let cases = [
            (
                "start",
                r#"{"value":[{"start":{"dateTime":"not-a-time"},"end":{"dateTime":"2026-10-12T11:00:00.0000000"},"showAs":"busy"}]}"#,
            ),
            (
                "end",
                r#"{"value":[{"start":{"dateTime":"2026-10-12T10:00:00.0000000"},"end":{"dateTime":"not-a-time"},"showAs":"busy"}]}"#,
            ),
        ];
        for (field, view) in cases {
            // Arrange.
            let mut stub = Stub::of(&[ok_json(view)]);

            // Act.
            let failed = call("findFreeTime", FREE_SEARCH, &mut stub);

            // Assert.
            assert!(
                matches!(&failed, Err(ConnectorError::Upstream(message)) if message.contains(&format!("unparseable event {field}"))),
                "{field}: {failed:?}"
            );
        }
    }

    #[test]
    fn find_free_time_rejects_a_missing_event_start() {
        // Arrange: an event with no start at all.
        let mut stub = Stub::of(&[ok_json(
            r#"{"value":[{"end":{"dateTime":"2026-10-12T11:00:00.0000000"},"showAs":"busy"}]}"#,
        )]);

        // Act.
        let failed = call("findFreeTime", FREE_SEARCH, &mut stub);

        // Assert.
        assert!(
            matches!(&failed, Err(ConnectorError::Upstream(message)) if message.contains("unparseable event start: missing")),
            "{failed:?}"
        );
    }

    #[test]
    fn find_free_time_rejects_garbled_answers() {
        // Arrange: valid input, but the API answers HTML instead of JSON.
        let mut stub = Stub::of(&[response(200, "<html>")]);

        // Act.
        let garbled = call("findFreeTime", FREE_SEARCH, &mut stub);

        // Assert: upstream garbage stays an upstream error.
        assert!(
            matches!(garbled, Err(ConnectorError::Upstream(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{garbled:?}"
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
    fn parses_wall_clocks() {
        // A wall clock is its civil time read as UTC.
        let utc = parse_moment("2026-10-12T10:00:00Z").unwrap().epoch;
        assert_eq!(wall_to_zone_epoch("2026-10-12T10:00:00.0000000"), Some(utc));
        assert_eq!(wall_to_zone_epoch("2026-10-12T10:00:00"), Some(utc));
        // The leap day parses.
        assert!(wall_to_zone_epoch("2024-02-29T10:00:00").is_some());

        // Everything that is not a wall clock in the parsed shape.
        let invalid = [
            "",
            "tomorrow",
            "2026-10-12",          // too short
            "2026-10-12 10:00:00", // no T separator
            "20x6-10-12T10:00:00", // non-digit year
            "2026-1x-12T10:00:00", // non-digit month
            "2026-10-1xT10:00:00", // non-digit day
            "2026/10/12T10:00:00", // wrong date separators
            "2026-10-12Txx:00:00", // non-digit hour
            "2026-10-12T10:x0:00", // non-digit minute
            "2026-10-12T10:00:x0", // non-digit second
            "2026-10-12T10-00:00", // wrong time separator
            "2026-10-12T10:00-00", // wrong time separator
            "2026-13-01T10:00:00", // month 13
            "2023-02-29T10:00:00", // no leap day in 2023
            "2026-04-31T10:00:00", // April has 30 days
            "2026-10-00T10:00:00", // day zero
            "2026-10-12T24:00:00", // hour 24
            "2026-10-12T10:60:00", // minute 60
            "2026-10-12T10:00:61", // second 61
        ];
        for text in invalid {
            // Act + assert.
            assert_eq!(wall_to_zone_epoch(text), None, "{text}");
        }
    }

    #[test]
    fn validates_dates() {
        assert!(valid_date("2026-12-25"));
        assert!(valid_date("2024-02-29"));
        for bad in [
            "2026-12-4",     // too short
            "2026/12/25",    // wrong separators
            "202x-12-25",    // non-digit year
            "2026-13-25",    // month 13
            "2026-00-25",    // month zero
            "2026-12-00",    // day zero
            "2026-12-32",    // day past the month
            "2023-02-29",    // no leap day in 2023
            "2026-04-31",    // April has 30 days
            "2026-12-25T00", // trailing input
        ] {
            assert!(!valid_date(bad), "{bad}");
        }
    }

    #[test]
    fn builds_query_strings() {
        assert_eq!(with_query("https://x/y", &[]), "https://x/y");
        assert_eq!(
            with_query("https://x/y", &[("a", Some("b c".into())), ("d", None)]),
            "https://x/y?a=b%20c"
        );
        assert_eq!(encode("a b/c?d=e&f"), "a%20b%2Fc%3Fd%3De%26f");
    }

    #[test]
    fn propagates_sender_errors() {
        // Arrange: the sender itself fails.
        let failed = OutlookCalendar::call_with(
            request("getEvent", r#"{"calendarId":"primary","eventId":"e1"}"#),
            |_| Err(ConnectorError::Upstream("network down".into())),
        );

        // Assert.
        assert_eq!(
            failed.unwrap_err(),
            ConnectorError::Upstream("network down".into())
        );

        // Every tool's first send surfaces the sender's failure the same way.
        let tools = [
            ("listEvents", r#"{"calendarId":"primary"}"#),
            ("getEvent", r#"{"calendarId":"primary","eventId":"e1"}"#),
            (
                "createEvent",
                r#"{"subject":"x","start":"2026-10-12T14:00:00-07:00","end":"2026-10-12T15:00:00-07:00","timeZone":"Pacific Standard Time"}"#,
            ),
            ("updateEvent", r#"{"eventId":"e1","subject":"New"}"#),
            ("deleteEvent", r#"{"calendarId":"primary","eventId":"e1"}"#),
            (
                "findFreeTime",
                r#"{"timeMin":"2026-10-12T09:00:00-07:00","timeMax":"2026-10-12T17:00:00-07:00","durationMinutes":30,"timeZone":"Pacific Standard Time"}"#,
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
        let sent = OutlookCalendar::call_tool(request("listEvents", r#"{"calendarId":"primary"}"#));
        assert!(matches!(&sent, Err(ConnectorError::Upstream(m)) if m.contains("wasm32-wasip2")));
    }

    #[test]
    fn manifest_is_embedded() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["auth"]["schemes"][0]["type"], "oauth2");
        assert_eq!(
            manifest["auth"]["schemes"][0]["scopes"],
            json!([
                "https://graph.microsoft.com/Calendars.ReadWrite",
                "offline_access",
            ])
        );
        assert_eq!(
            manifest["permissions"]["http"]["allow"][0],
            DEFAULT_API_BASE
        );
        assert!(!manifest["icons"].as_array().unwrap().is_empty());
        assert!(!manifest["instructions"].as_str().unwrap().is_empty());
        let tools = manifest["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 6);
        let free_time = tools
            .iter()
            .find(|tool| tool["name"] == "findFreeTime")
            .unwrap();
        assert_eq!(
            free_time["inputSchema"]["required"],
            json!(["timeMin", "timeMax", "durationMinutes", "timeZone"])
        );
    }
}
