//! Google Forms connector: outcome-oriented tools that finish in one call.
//!
//! This is deliberately not a 1:1 mapping of the Forms API. Each tool
//! completes an outcome in a single call: creating a form with its questions,
//! adding questions, or reading responses with answers keyed by question
//! title. Questions are given as simple `type` strings ("text",
//! "paragraph", "multipleChoice", "checkboxes", "dropdown", "scale", "date",
//! "time") and the connector builds the API's question payloads internally.
//! A question is found by item ID *or* by title, resolved inside the
//! connector instead of pushing a lookup onto the agent. One tool may
//! therefore make several HTTP calls; see [`Api`].
//!
//! [`Transport`] is what makes those multi-call tools unit-testable: the
//! guest sends requests over HTTP, while tests answer from canned
//! responses.
//!
//! The host runs the OAuth 2.0 sign-in, refreshes the token and adds
//! `Authorization: Bearer …` after each request leaves the guest: this code
//! never sees it.
//!
//! Every `createItem`/`updateItem`/`deleteItem` request carries an explicit
//! `location.index`: the API requires it and never appends implicitly.

use connectors_sdk::http::{Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://forms.googleapis.com/v1";

/// The connector.
#[derive(Debug)]
pub struct Gforms;

// ---------------------------------------------------------------------------
// Tool inputs. Field names are the API's camelCase parameter names.
// ---------------------------------------------------------------------------

/// One question in a form, as an agent describes it. `type` is one of
/// `text`, `paragraph`, `multipleChoice`, `checkboxes`, `dropdown`,
/// `scale`, `date`, `time`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuestionSpec {
    #[serde(rename = "type")]
    question_type: String,
    title: String,
    description: Option<String>,
    required: Option<bool>,
    options: Option<Vec<String>>,
    low: Option<i32>,
    high: Option<i32>,
    low_label: Option<String>,
    high_label: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateForm {
    title: String,
    description: Option<String>,
    questions: Vec<QuestionSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddQuestions {
    form_id: String,
    questions: Vec<QuestionSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateFormInfo {
    form_id: String,
    title: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateQuestion {
    form_id: String,
    question: String,
    title: Option<String>,
    description: Option<String>,
    required: Option<bool>,
    options: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuestionRef {
    form_id: String,
    question: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishForm {
    form_id: String,
    published: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FormRef {
    form_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetResponses {
    form_id: String,
    page_size: Option<u32>,
    page_token: Option<String>,
}

// ---------------------------------------------------------------------------
// Failures
// ---------------------------------------------------------------------------

/// Why a tool call failed. Rendered as the tool's error message so the
/// agent can react (a 403 usually means a missing scope).
#[derive(Debug)]
struct ToolError {
    /// The API's HTTP status, when the failure is an API answer. Only read
    /// by tests (to assert on failure kinds); agents see the message alone.
    #[allow(dead_code)]
    status: Option<u16>,
    message: String,
}

impl ToolError {
    fn api(status: u16, body: &str) -> Self {
        Self {
            status: Some(status),
            message: format!("The API answered {status}: {body}"),
        }
    }

    fn input(message: impl Into<String>) -> Self {
        Self {
            status: None,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<ConnectorError> for ToolError {
    fn from(error: ConnectorError) -> Self {
        Self::input(error.to_string())
    }
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// Sends planned requests. The guest sends them over HTTP; tests answer
/// from canned responses.
trait Transport {
    fn send(&self, request: Request) -> Result<Response, ConnectorError>;
}

/// The real transport: HTTP inside the wasm32-wasip2 guest.
struct Guest;

impl Transport for Guest {
    fn send(&self, request: Request) -> Result<Response, ConnectorError> {
        request.send()
    }
}

// ---------------------------------------------------------------------------
// Question kinds
// ---------------------------------------------------------------------------

/// The question kinds agents can ask for, as `QuestionSpec.type` values.
const QUESTION_TYPES: &[&str] = &[
    "text",
    "paragraph",
    "multipleChoice",
    "checkboxes",
    "dropdown",
    "scale",
    "date",
    "time",
];

/// The API's question object for one agent-described question spec:
/// `{"required": …, "<kind>Question": {…}}`.
fn question_payload(spec: &QuestionSpec) -> Result<Value, ToolError> {
    let (kind, body) = match spec.question_type.as_str() {
        "text" | "paragraph" => (
            "textQuestion",
            json!({"paragraph": spec.question_type == "paragraph"}),
        ),
        "multipleChoice" | "checkboxes" | "dropdown" => ("choiceQuestion", choice_body(spec)?),
        "scale" => ("scaleQuestion", scale_body(spec)?),
        "date" => (
            "dateQuestion",
            json!({"includeTime": false, "includeYear": true}),
        ),
        "time" => ("timeQuestion", json!({"duration": false})),
        other => {
            return Err(ToolError::input(format!(
                "Unknown question type {other:?}; use one of {}",
                QUESTION_TYPES.join(", ")
            )));
        }
    };
    Ok(object(&[
        ("required", field(spec.required)),
        (kind, Some(body)),
    ]))
}

/// The body of a `choiceQuestion` from the spec's options.
fn choice_body(spec: &QuestionSpec) -> Result<Value, ToolError> {
    let options = spec.options.as_deref().unwrap_or_default();
    if options.is_empty() {
        return Err(ToolError::input(format!(
            "{} questions need at least one option",
            spec.question_type
        )));
    }
    let api_type = match spec.question_type.as_str() {
        "multipleChoice" => "RADIO",
        "checkboxes" => "CHECKBOX",
        _ => "DROP_DOWN",
    };
    Ok(json!({
        "type": api_type,
        "options": options.iter().map(|value| json!({"value": value})).collect::<Vec<_>>(),
    }))
}

/// The body of a `scaleQuestion` from the spec's bounds (default 1–5).
fn scale_body(spec: &QuestionSpec) -> Result<Value, ToolError> {
    let low = spec.low.unwrap_or(1);
    let high = spec.high.unwrap_or(5);
    if low >= high {
        return Err(ToolError::input(format!(
            "A scale needs low below high, got low={low} high={high}"
        )));
    }
    Ok(json!({
        "low": low,
        "high": high,
        "lowLabel": spec.low_label,
        "highLabel": spec.high_label,
    }))
}

/// The API's item object for one agent-described question.
fn item_payload(spec: &QuestionSpec) -> Result<Value, ToolError> {
    Ok(object(&[
        ("title", Some(json!(spec.title))),
        ("description", field(spec.description.clone())),
        (
            "questionItem",
            Some(json!({"question": question_payload(spec)?})),
        ),
    ]))
}

/// The simple type name of a question on a form the API returned.
fn question_type_of(question: &Value) -> &'static str {
    if let Some(text) = question.get("textQuestion") {
        return if text
            .get("paragraph")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "paragraph"
        } else {
            "text"
        };
    }
    if let Some(choice) = question.get("choiceQuestion") {
        return match choice.get("type").and_then(Value::as_str) {
            Some("CHECKBOX") => "checkboxes",
            Some("DROP_DOWN") => "dropdown",
            _ => "multipleChoice",
        };
    }
    if question.get("scaleQuestion").is_some() {
        return "scale";
    }
    if question.get("dateQuestion").is_some() {
        return "date";
    }
    if question.get("timeQuestion").is_some() {
        return "time";
    }
    "unknown"
}

/// A question on a form the API returned, in the shape agents read.
fn readable_question(item: &Value) -> Value {
    let question = item
        .get("questionItem")
        .and_then(|item| item.get("question"));
    let mut fields: Vec<(&str, Option<Value>)> = vec![
        ("id", item.get("itemId").cloned()),
        ("title", item.get("title").cloned()),
        ("description", item.get("description").cloned()),
    ];
    if let Some(question) = question {
        fields.push(("type", Some(json!(question_type_of(question)))));
        fields.push((
            "required",
            Some(json!(
                question
                    .get("required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            )),
        ));
        if let Some(options) = question
            .get("choiceQuestion")
            .and_then(|choice| choice.get("options"))
            .and_then(Value::as_array)
        {
            fields.push((
                "options",
                Some(Value::Array(
                    options
                        .iter()
                        .filter_map(|option| {
                            option
                                .get("value")
                                .and_then(Value::as_str)
                                .map(|text| json!(text))
                        })
                        .collect(),
                )),
            ));
        }
    }
    object(&fields)
}

/// A form the API returned, in the shape agents read.
fn readable_form(form: &Value) -> Value {
    let info = form.get("info");
    object(&[
        ("formId", form.get("formId").cloned()),
        ("title", info.and_then(|info| info.get("title")).cloned()),
        (
            "description",
            info.and_then(|info| info.get("description")).cloned(),
        ),
        ("responderUri", form.get("responderUri").cloned()),
        (
            "questions",
            Some(Value::Array(
                form.get("items")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter(|item| item.get("questionItem").is_some())
                            .map(readable_question)
                            .collect()
                    })
                    .unwrap_or_default(),
            )),
        ),
    ])
}

// ---------------------------------------------------------------------------
// The API client
// ---------------------------------------------------------------------------

/// The Forms API at one base URL. A tool method plans whatever requests its
/// outcome needs and sends them through the transport, resolving question
/// titles to item IDs along the way.
struct Api<T: Transport = Guest> {
    /// `{base}/forms`, the root of every endpoint.
    forms: String,
    transport: T,
}

impl<T: Transport> Api<T> {
    fn new(base: &str, transport: T) -> Self {
        Self {
            forms: format!("{}/forms", base.trim_end_matches('/')),
            transport,
        }
    }

    /// Sends one request and answers the parsed JSON body. An empty body
    /// (the API's 204 answers) becomes `Value::Null`.
    fn send(&self, request: Request) -> Result<Value, ToolError> {
        let response = self.transport.send(request)?;
        if !(200..300).contains(&response.status) {
            let body = String::from_utf8_lossy(&response.body);
            return Err(ToolError::api(response.status, &body));
        }
        if response.body.is_empty() {
            return Ok(Value::Null);
        }
        Ok(response.json()?)
    }

    /// GETs a URL, asking for JSON back.
    fn get(&self, url: &str) -> Result<Value, ToolError> {
        self.send(Request::get(url).header("accept", "application/json"))
    }

    /// POSTs a JSON body.
    fn post_json(&self, url: &str, body: &Value) -> Result<Value, ToolError> {
        self.send(
            Request::post(url)
                .header("accept", "application/json")
                .json(body)?, // coverage: exclude — Request::json serializes a serde_json::Value, which is infallible
        )
    }

    /// One form by ID.
    fn form(&self, form_id: &str) -> Result<Value, ToolError> {
        self.get(&format!("{}/{}", self.forms, encode(form_id)))
    }

    /// Applies batch update requests, answering the updated form.
    fn batch_update(&self, form_id: &str, requests: &[Value]) -> Result<Value, ToolError> {
        let body = self.post_json(
            &format!("{}:batchUpdate", self.form_url(form_id)),
            &json!({"includeFormInResponse": true, "requests": requests}),
        )?;
        Ok(body.get("form").cloned().unwrap_or(Value::Null))
    }

    fn form_url(&self, form_id: &str) -> String {
        format!("{}/{}", self.forms, encode(form_id))
    }

    /// Finds one question on a form given as an item ID or a question
    /// title (case-insensitive), answering its index and item. A title
    /// that matches several questions fails with their IDs so the agent
    /// can be specific.
    fn resolve_question(&self, form_id: &str, question: &str) -> Result<(usize, Value), ToolError> {
        let form = self.form(form_id)?;
        let items: Vec<&Value> = form
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter(|item| item.get("questionItem").is_some())
                    .collect()
            })
            .unwrap_or_default();
        let mut matches = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let id = item
                .get("itemId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if id == question || title.eq_ignore_ascii_case(question) {
                matches.push((index, (*item).clone()));
            }
        }
        match matches.as_slice() {
            [(index, item)] => Ok((*index, item.clone())),
            [] => {
                let titles: Vec<&str> = items
                    .iter()
                    .filter_map(|item| item.get("title").and_then(Value::as_str))
                    .collect();
                Err(ToolError::input(format!(
                    "No question {question:?} on this form; its questions are: {}",
                    titles.join(", ")
                )))
            }
            many => {
                let ids: Vec<&str> = many
                    .iter()
                    .filter_map(|(_, item)| item.get("itemId").and_then(Value::as_str))
                    .collect();
                Err(ToolError::input(format!(
                    "Several questions are titled {question:?}; use an ID: {}",
                    ids.join(", ")
                )))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

impl Connector for Gforms {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        let base = host::config("api_base").unwrap_or_else(|| DEFAULT_API_BASE.into());
        let api = Api::new(&base, Guest);
        // A failed tool call is still a tool answer: the agent sees what
        // happened and can react, instead of the call itself erroring.
        run(&api, &request).or_else(|error| Ok(ToolResponse::error(error.to_string())))
    }
}

connectors_sdk::export_connector!(Gforms);

/// Runs one tool call: plans whatever requests the outcome needs, sends
/// them, and maps the answers to a tool result.
fn run<T: Transport>(api: &Api<T>, request: &ToolRequest) -> Result<ToolResponse, ToolError> {
    Ok(match request.tool.as_str() {
        "createForm" => run_create_form(api, request)?,
        "getForm" => run_get_form(api, request)?,
        "addQuestions" => run_add_questions(api, request)?,
        "updateFormInfo" => run_update_form_info(api, request)?,
        "updateQuestion" => run_update_question(api, request)?,
        "deleteQuestion" => run_delete_question(api, request)?,
        "publishForm" => run_publish_form(api, request)?,
        "getResponses" => run_get_responses(api, request)?,
        other => return Err(ToolError::input(format!("no tool {other}"))),
    })
}

/// `createItem` requests for `questions`, appended after `start`.
fn create_item_requests(questions: &[QuestionSpec], start: usize) -> Result<Vec<Value>, ToolError> {
    questions
        .iter()
        .enumerate()
        .map(|(offset, spec)| {
            Ok(json!({
                "createItem": {
                    "location": {"index": start + offset},
                    "item": item_payload(spec)?,
                }
            }))
        })
        .collect()
}

fn run_create_form<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: CreateForm = request.json()?;
    // Validate the questions before creating anything, so a bad spec
    // fails without leaving a half-built form behind.
    let items = create_item_requests(&input.questions, 0)?;
    let created: Value = api.post_json(
        &api.forms,
        &json!({"info": {"title": &input.title, "documentTitle": &input.title}}),
    )?;
    let form_id = created
        .get("formId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut updates = Vec::new();
    if input.description.is_some() {
        updates.push(json!({
            "updateFormInfo": {
                "info": {"description": input.description},
                "updateMask": "description",
            }
        }));
    }
    updates.extend(items);
    let form = if updates.is_empty() {
        created
    } else {
        api.batch_update(form_id, &updates)?
    };
    Ok(ToolResponse::structured(&readable_form(&form))?) // coverage: exclude — ToolResponse::structured serializes a serde_json::Value, which is infallible
}

fn run_get_form<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: FormRef = request.json()?;
    let form = api.form(&input.form_id)?;
    Ok(ToolResponse::structured(&readable_form(&form))?) // coverage: exclude — ToolResponse::structured serializes a serde_json::Value, which is infallible
}

fn run_add_questions<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: AddQuestions = request.json()?;
    if input.questions.is_empty() {
        return Err(ToolError::input(
            "Nothing to add: give at least one question",
        ));
    }
    // `createItem` needs an explicit index; new items go after the last.
    let existing = api.form(&input.form_id)?;
    let count = existing
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let updates = create_item_requests(&input.questions, count)?;
    let form = api.batch_update(&input.form_id, &updates)?;
    let questions: Vec<Value> = form
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .skip(count)
                .filter(|item| item.get("questionItem").is_some())
                .map(readable_question)
                .collect()
        })
        .unwrap_or_default();
    Ok(ToolResponse::structured(&object(&[
        ("formId", Some(json!(input.form_id))),
        ("added", Some(Value::Array(questions))),
    ]))?) // coverage: exclude — ToolResponse::structured serializes a serde_json::Value, which is infallible
}

fn run_update_form_info<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: UpdateFormInfo = request.json()?;
    if input.title.is_none() && input.description.is_none() {
        return Err(ToolError::input(
            "Nothing to update: give at least one of title, description",
        ));
    }
    let mut masked = Vec::new();
    if input.title.is_some() {
        masked.push("title");
    }
    if input.description.is_some() {
        masked.push("description");
    }
    let updates = vec![json!({
        "updateFormInfo": {
            "info": {
                "title": input.title,
                "description": input.description,
            },
            "updateMask": masked.join(","),
        }
    })];
    let form = api.batch_update(&input.form_id, &updates)?;
    Ok(ToolResponse::structured(&readable_form(&form))?) // coverage: exclude — ToolResponse::structured serializes a serde_json::Value, which is infallible
}

fn run_update_question<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: UpdateQuestion = request.json()?;
    if input.title.is_none()
        && input.description.is_none()
        && input.required.is_none()
        && input.options.is_none()
    {
        return Err(ToolError::input(
            "Nothing to update: give at least one of title, description, required, options",
        ));
    }
    let (index, item) = api.resolve_question(&input.form_id, &input.question)?;
    let question = item
        .get("questionItem")
        .and_then(|item| item.get("question"));
    if input.options.is_some() && question.is_some_and(|q| q.get("choiceQuestion").is_none()) {
        return Err(ToolError::input(
            "options only apply to multipleChoice, checkboxes, and dropdown questions",
        ));
    }
    // Replacing the whole choice question clears its type unless it is
    // carried along, so the existing type is preserved.
    let choice_type = question
        .and_then(|q| q.get("choiceQuestion"))
        .and_then(|choice| choice.get("type"))
        .and_then(Value::as_str);
    let mut item_map = serde_json::Map::with_capacity(4);
    if let Some(item_id) = item.get("itemId") {
        item_map.insert("itemId".to_string(), item_id.clone());
    } // coverage: exclude — defensive: the Forms API always returns itemId for items
    if let Some(title) = input.title {
        item_map.insert("title".to_string(), json!(title));
    }
    if let Some(description) = input.description {
        item_map.insert("description".to_string(), json!(description));
    }
    if input.required.is_some() || input.options.is_some() {
        item_map.insert(
            "questionItem".to_string(),
            json!({
                "question": {
                    "required": input.required,
                    "choiceQuestion": input.options.as_ref().map(|options| json!({
                        "type": choice_type,
                        "options": options.iter().map(|value| json!({"value": value})).collect::<Vec<_>>(),
                    })),
                }
            }),
        );
    }
    let mut mask = Vec::new();
    if item_map.contains_key("title") {
        mask.push("title");
    }
    if item_map.contains_key("description") {
        mask.push("description");
    }
    if input.required.is_some() {
        mask.push("questionItem.question.required");
    }
    if input.options.is_some() {
        mask.push("questionItem.question.choiceQuestion");
    }
    let updates = vec![json!({
        "updateItem": {
            "location": {"index": index},
            "item": Value::Object(item_map),
            "updateMask": mask.join(","),
        }
    })];
    let form = api.batch_update(&input.form_id, &updates)?;
    let updated = form
        .get("items")
        .and_then(Value::as_array)
        .and_then(|items| items.get(index))
        .cloned()
        .unwrap_or(Value::Null);
    Ok(ToolResponse::structured(&readable_question(&updated))?) // coverage: exclude — ToolResponse::structured serializes a serde_json::Value, which is infallible
}

fn run_delete_question<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: QuestionRef = request.json()?;
    let (index, _) = api.resolve_question(&input.form_id, &input.question)?;
    api.batch_update(
        &input.form_id,
        &[json!({"deleteItem": {"location": {"index": index}}})],
    )?;
    Ok(ToolResponse::structured(&json!({"deleted": true}))?) // coverage: exclude — ToolResponse::structured serializes a serde_json::Value, which is infallible
}

fn run_publish_form<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: PublishForm = request.json()?;
    let body = api.post_json(
        &format!("{}:setPublishSettings", api.form_url(&input.form_id)),
        &json!({
            "publishSettings": {
                "publishState": {
                    "isPublished": input.published,
                    "isAcceptingResponses": input.published,
                }
            }
        }),
    )?;
    let settings = body.get("publishSettings").cloned().unwrap_or(Value::Null);
    Ok(ToolResponse::structured(&settings)?) // coverage: exclude — ToolResponse::structured serializes a serde_json::Value, which is infallible
}

fn run_get_responses<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: GetResponses = request.json()?;
    // Question titles make the answers readable in one call.
    let form = api.form(&input.form_id)?;
    let titles: HashMap<&str, &str> = form
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some((item.get("itemId")?.as_str()?, item.get("title")?.as_str()?))
                })
                .collect()
        })
        .unwrap_or_default();
    let url = with_query(
        &format!("{}/responses", api.form_url(&input.form_id)),
        &[
            ("pageSize", query(input.page_size)),
            ("pageToken", input.page_token),
        ],
    );
    let body = api.get(&url)?;
    let responses: Vec<Value> = body
        .get("responses")
        .and_then(Value::as_array)
        .map(|responses| {
            responses
                .iter()
                .map(|response| readable_response(response, &titles))
                .collect()
        })
        .unwrap_or_default();
    Ok(ToolResponse::structured(&object(&[
        ("responses", Some(Value::Array(responses))),
        ("nextPageToken", body.get("nextPageToken").cloned()),
    ]))?) // coverage: exclude — ToolResponse::structured serializes a serde_json::Value, which is infallible
}

/// One form response with answers keyed by question title.
fn readable_response(response: &Value, titles: &HashMap<&str, &str>) -> Value {
    let mut answers = serde_json::Map::new();
    if let Some(map) = response.get("answers").and_then(Value::as_object) {
        for (question_id, answer) in map {
            let title = titles
                .get(question_id.as_str())
                .copied()
                .unwrap_or(question_id.as_str());
            let values: Vec<&str> = answer
                .get("textAnswers")
                .and_then(|text| text.get("answers"))
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|entry| entry.get("value").and_then(Value::as_str))
                        .collect()
                })
                .unwrap_or_default();
            answers.insert(title.to_string(), json!(values.join(", ")));
        }
    }
    object(&[
        ("responseId", response.get("responseId").cloned()),
        ("submittedAt", response.get("lastSubmittedTime").cloned()),
        ("answers", Some(Value::Object(answers))),
    ])
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// A JSON body field that is present only when the input was.
fn field<T: Into<Value>>(value: Option<T>) -> Option<Value> {
    value.map(Into::into)
}

/// A query parameter that is present only when the input was.
fn query<T: ToString>(value: Option<T>) -> Option<String> {
    value.map(|value| value.to_string())
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
    let mut map = serde_json::Map::with_capacity(fields.len());
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
    use connectors_sdk::http::Method;
    use std::cell::RefCell;

    const BASE: &str = "https://forms.test/v1";
    const FORMS: &str = "https://forms.test/v1/forms";
    const FORM_URL: &str = "https://forms.test/v1/forms/f1";
    const BATCH_URL: &str = "https://forms.test/v1/forms/f1:batchUpdate";

    const FORM_BODY: &str = r#"{
        "formId": "f1",
        "info": {"title": "Survey", "description": "A survey"},
        "responderUri": "https://forms.test/r/f1",
        "items": [
            {"itemId": "q1", "title": "Your name",
             "questionItem": {"question": {"required": true, "textQuestion": {"paragraph": false}}}},
            {"itemId": "q2", "title": "Favorite color", "description": "Pick one",
             "questionItem": {"question": {"choiceQuestion": {"type": "RADIO",
                 "options": [{"value": "Red"}, {"value": "Blue"}]}}}},
            {"itemId": "t1", "title": "Welcome", "textItem": {}}
        ]
    }"#;

    /// A canned API answer: the request's method and URL, then the status
    /// and body to answer with.
    type CannedAnswer = ((Method, String), (u16, &'static str));

    /// Canned API answers, keyed by method and URL, plus every request
    /// seen, so tests can assert on planned bodies.
    struct Fake {
        answers: Vec<CannedAnswer>,
        fail: bool,
        seen: RefCell<Vec<Request>>,
    }

    impl Fake {
        fn answering(answers: &[((Method, &str), u16, &'static str)]) -> Self {
            Self {
                answers: answers
                    .iter()
                    .map(|((method, url), status, body)| {
                        ((*method, String::from(*url)), (*status, *body))
                    })
                    .collect(),
                fail: false,
                seen: RefCell::new(Vec::new()),
            }
        }

        fn failing() -> Self {
            Self {
                answers: Vec::new(),
                fail: true,
                seen: RefCell::new(Vec::new()),
            }
        }

        fn answer_for(&self, method: Method, url: &str) -> Option<(u16, &'static str)> {
            self.answers
                .iter()
                .find(|((m, u), _)| *m == method && u == url)
                .map(|(_, answer)| *answer)
        }
    }

    impl Transport for Fake {
        fn send(&self, request: Request) -> Result<Response, ConnectorError> {
            if self.fail {
                return Err(ConnectorError::Upstream("transport boom".into()));
            }
            let (status, body) =
                self.answer_for(request.method, &request.url)
                    .ok_or_else(|| {
                        ConnectorError::Upstream(format!(
                            "no canned answer for {:?} {}",
                            request.method, request.url
                        ))
                    })?;
            self.seen.borrow_mut().push(request);
            Ok(Response {
                status,
                headers: Vec::new(),
                body: body.as_bytes().to_vec(),
            })
        }
    }

    fn client(answers: &[((Method, &str), u16, &'static str)]) -> Api<Fake> {
        Api::new(BASE, Fake::answering(answers))
    }

    fn request(tool: &str, body: &str) -> ToolRequest {
        ToolRequest {
            tool: tool.into(),
            body: body.into(),
        }
    }

    fn run_tool(api: &Api<Fake>, tool: &str, body: &str) -> Result<ToolResponse, ToolError> {
        run(api, &request(tool, body))
    }

    fn structured(response: &ToolResponse) -> &str {
        response.structured.as_deref().unwrap()
    }

    fn seen_bodies(api: &Api<Fake>) -> Vec<Value> {
        api.transport
            .seen
            .borrow()
            .iter()
            .filter(|request| !request.body.is_empty())
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect()
    }

    #[test]
    fn creates_a_form_with_questions_in_one_call() {
        let api = client(&[
            (
                (Method::Post, FORMS),
                200,
                r#"{"formId":"f1","info":{"title":"Team lunch"}}"#,
            ),
            (
                (Method::Post, BATCH_URL),
                200,
                r#"{"form": {"formId":"f1","info":{"title":"Team lunch","description":"Pick a day"},"responderUri":"https://forms.test/r/f1","items":[
                    {"itemId":"q1","title":"Your name","questionItem":{"question":{"required":true,"textQuestion":{"paragraph":false}}}},
                    {"itemId":"q2","title":"Day","questionItem":{"question":{"choiceQuestion":{"type":"RADIO","options":[{"value":"Mon"},{"value":"Tue"}]}}}}
                ]}}"#,
            ),
        ]);

        let response = run_tool(
            &api,
            "createForm",
            r#"{"title":"Team lunch","description":"Pick a day","questions":[
                {"type":"text","title":"Your name","required":true},
                {"type":"multipleChoice","title":"Day","options":["Mon","Tue"]}
            ]}"#,
        )
        .unwrap();
        let answered: Value = serde_json::from_str(structured(&response)).unwrap();
        assert_eq!(answered["formId"], "f1");
        assert_eq!(answered["title"], "Team lunch");
        assert_eq!(answered["description"], "Pick a day");
        assert_eq!(answered["responderUri"], "https://forms.test/r/f1");

        let bodies = seen_bodies(&api);
        assert_eq!(bodies.len(), 2);
        // The description update goes first, then the questions at
        // indices 0 and 1.
        let creates = bodies[1]["requests"].as_array().unwrap();
        assert_eq!(creates.len(), 3);
        assert_eq!(creates[0]["updateFormInfo"]["updateMask"], "description");
        assert_eq!(creates[1]["createItem"]["location"]["index"], 0);
        assert_eq!(
            creates[1]["createItem"]["item"]["questionItem"]["question"]["textQuestion"],
            json!({"paragraph": false})
        );
        assert_eq!(
            creates[2]["createItem"]["item"]["questionItem"]["question"]["choiceQuestion"]["type"],
            "RADIO"
        );
        assert_eq!(creates[2]["createItem"]["location"]["index"], 1);
    }

    #[test]
    fn creates_every_question_kind() {
        let api = client(&[
            (
                (Method::Post, FORMS),
                200,
                r#"{"formId":"f1","info":{"title":"Kinds"}}"#,
            ),
            ((Method::Post, BATCH_URL), 200, r#"{"form":{}}"#),
        ]);

        let response = run_tool(
            &api,
            "createForm",
            r#"{"title":"Kinds","questions":[
                {"type":"paragraph","title":"Bio"},
                {"type":"checkboxes","title":"Toppings","options":["Cheese","Ham"]},
                {"type":"dropdown","title":"Size","options":["S","M"]},
                {"type":"scale","title":"Spice"},
                {"type":"date","title":"When"},
                {"type":"time","title":"At"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(structured(&response), r#"{"questions":[]}"#);

        let bodies = seen_bodies(&api);
        let questions: Vec<&Value> = bodies[1]["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| &r["createItem"]["item"]["questionItem"]["question"])
            .collect();
        assert_eq!(questions.len(), 6);
        assert_eq!(questions[0]["textQuestion"], json!({"paragraph": true}));
        assert_eq!(questions[1]["choiceQuestion"]["type"], "CHECKBOX");
        assert_eq!(questions[2]["choiceQuestion"]["type"], "DROP_DOWN");
        // Default scale bounds are 1–5.
        assert_eq!(
            questions[3]["scaleQuestion"],
            json!({"low": 1, "high": 5, "lowLabel": null, "highLabel": null})
        );
        assert_eq!(
            questions[4]["dateQuestion"],
            json!({"includeTime": false, "includeYear": true})
        );
        assert_eq!(questions[5]["timeQuestion"], json!({"duration": false}));
    }

    #[test]
    fn creates_a_bare_form_without_a_batch() {
        let api = client(&[(
            (Method::Post, FORMS),
            200,
            r#"{"formId":"f9","info":{"title":"Bare"},"responderUri":"https://forms.test/r/f9"}"#,
        )]);
        let response = run_tool(&api, "createForm", r#"{"title":"Bare","questions":[]}"#).unwrap();
        let answered: Value = serde_json::from_str(structured(&response)).unwrap();
        assert_eq!(answered["formId"], "f9");
        assert_eq!(answered["questions"], json!([]));
        assert_eq!(api.transport.seen.borrow().len(), 1);
    }

    #[test]
    fn rejects_bad_question_specs() {
        let api = client(&[]);

        let unknown = run_tool(
            &api,
            "createForm",
            r#"{"title":"x","questions":[{"type":"essay","title":"?"}]}"#,
        );
        assert!(
            matches!(&unknown, Err(error) if error.message.contains("Unknown question type")
                && error.message.contains("multipleChoice")),
            "{unknown:?}"
        );

        let no_options = run_tool(
            &api,
            "createForm",
            r#"{"title":"x","questions":[{"type":"dropdown","title":"?"}]}"#,
        );
        assert!(
            matches!(&no_options, Err(error) if error.message.contains("need at least one option")),
            "{no_options:?}"
        );

        let bad_scale = run_tool(
            &api,
            "createForm",
            r#"{"title":"x","questions":[{"type":"scale","title":"?","low":5,"high":5}]}"#,
        );
        assert!(
            matches!(&bad_scale, Err(error) if error.message.contains("low below high")),
            "{bad_scale:?}"
        );
    }

    #[test]
    fn gets_a_form_in_readable_shape() {
        let api = client(&[((Method::Get, FORM_URL), 200, FORM_BODY)]);
        let response = run_tool(&api, "getForm", r#"{"formId":"f1"}"#).unwrap();
        let answered: Value = serde_json::from_str(structured(&response)).unwrap();
        assert_eq!(answered["formId"], "f1");
        assert_eq!(answered["title"], "Survey");
        assert_eq!(answered["description"], "A survey");
        assert_eq!(answered["responderUri"], "https://forms.test/r/f1");
        // Only questions show up, not presentation items.
        assert_eq!(answered["questions"].as_array().unwrap().len(), 2);
        assert_eq!(
            answered["questions"][0],
            json!({"id":"q1","title":"Your name","type":"text","required":true})
        );
        assert_eq!(
            answered["questions"][1],
            json!({"id":"q2","title":"Favorite color","description":"Pick one","type":"multipleChoice","required":false,"options":["Red","Blue"]})
        );
    }

    #[test]
    fn reads_every_question_kind() {
        let body = r#"{"formId":"f1","info":{"title":"Kinds"},"items":[
            {"itemId":"a","title":"A","questionItem":{"question":{"textQuestion":{"paragraph":true}}}},
            {"itemId":"b","title":"B","questionItem":{"question":{"choiceQuestion":{"type":"CHECKBOX","options":[{"value":"x"},{}]}}}},
            {"itemId":"c","title":"C","questionItem":{"question":{"choiceQuestion":{"type":"DROP_DOWN"}}}},
            {"itemId":"d","title":"D","questionItem":{"question":{"scaleQuestion":{}}}},
            {"itemId":"e","title":"E","questionItem":{"question":{"dateQuestion":{}}}},
            {"itemId":"f","title":"F","questionItem":{"question":{"timeQuestion":{}}}},
            {"itemId":"g","title":"G","questionItem":{"question":{}}},
            {"itemId":"h","title":"H","questionItem":{}}
        ]}"#;
        let api = client(&[((Method::Get, FORM_URL), 200, body)]);
        let response = run_tool(&api, "getForm", r#"{"formId":"f1"}"#).unwrap();
        let answered: Value = serde_json::from_str(structured(&response)).unwrap();
        let types: Vec<&str> = answered["questions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|q| q["type"].as_str().unwrap_or("-"))
            .collect();
        assert_eq!(
            types,
            [
                "paragraph",
                "checkboxes",
                "dropdown",
                "scale",
                "date",
                "time",
                "unknown",
                "-"
            ]
        );
        // Options with no value are skipped.
        assert_eq!(answered["questions"][1]["options"], json!(["x"]));
        assert!(answered["questions"][2].get("options").is_none());
    }

    #[test]
    fn adds_questions_after_the_last_item() {
        let api = client(&[
            ((Method::Get, FORM_URL), 200, FORM_BODY),
            (
                (Method::Post, BATCH_URL),
                200,
                r#"{"form":{"formId":"f1","info":{"title":"Survey"},"items":[
                    {"itemId":"q1","title":"Your name","questionItem":{"question":{"textQuestion":{"paragraph":false}}}},
                    {"itemId":"q2","title":"Favorite color","questionItem":{"question":{"choiceQuestion":{"type":"RADIO"}}}},
                    {"itemId":"t1","title":"Welcome","textItem":{}},
                    {"itemId":"q3","title":"How hungry?","questionItem":{"question":{"scaleQuestion":{"low":1,"high":10}}}}
                ]}}"#,
            ),
        ]);

        let response = run_tool(
            &api,
            "addQuestions",
            r#"{"formId":"f1","questions":[{"type":"scale","title":"How hungry?","low":1,"high":10,"lowLabel":"Not at all","highLabel":"Starving"}]}"#,
        )
        .unwrap();
        let answered: Value = serde_json::from_str(structured(&response)).unwrap();
        assert_eq!(answered["formId"], "f1");
        assert_eq!(answered["added"].as_array().unwrap().len(), 1);
        assert_eq!(answered["added"][0]["id"], "q3");
        assert_eq!(answered["added"][0]["type"], "scale");

        let bodies = seen_bodies(&api);
        assert_eq!(bodies.len(), 1);
        // The form already has 3 items, so the new one lands at index 3.
        assert_eq!(
            bodies[0]["requests"][0]["createItem"]["location"]["index"],
            3
        );
        assert_eq!(
            bodies[0]["requests"][0]["createItem"]["item"]["questionItem"]["question"]["scaleQuestion"],
            json!({"low": 1, "high": 10, "lowLabel": "Not at all", "highLabel": "Starving"})
        );
    }

    #[test]
    fn refuses_to_add_no_questions() {
        let api = client(&[]);
        let missing = run_tool(&api, "addQuestions", r#"{"formId":"f1","questions":[]}"#);
        assert!(
            matches!(&missing, Err(error) if error.message.contains("Nothing to add")),
            "{missing:?}"
        );
    }

    #[test]
    fn updates_form_info() {
        let api = client(&[(
            (Method::Post, BATCH_URL),
            200,
            r#"{"form":{"formId":"f1","info":{"title":"New title","description":"New desc"}}}"#,
        )]);

        let response = run_tool(
            &api,
            "updateFormInfo",
            r#"{"formId":"f1","title":"New title","description":"New desc"}"#,
        )
        .unwrap();
        let answered: Value = serde_json::from_str(structured(&response)).unwrap();
        assert_eq!(answered["title"], "New title");
        assert_eq!(answered["description"], "New desc");

        let bodies = seen_bodies(&api);
        assert_eq!(
            bodies[0]["requests"][0]["updateFormInfo"]["updateMask"],
            "title,description"
        );

        let nothing = run_tool(&api, "updateFormInfo", r#"{"formId":"f1"}"#);
        assert!(
            matches!(&nothing, Err(error) if error.message.contains("Nothing to update")),
            "{nothing:?}"
        );
    }

    #[test]
    fn updates_form_info_with_title_only() {
        let api = client(&[(
            (Method::Post, BATCH_URL),
            200,
            r#"{"form":{"formId":"f1","info":{"title":"Only"}}}"#,
        )]);
        run_tool(&api, "updateFormInfo", r#"{"formId":"f1","title":"Only"}"#).unwrap();
        let bodies = seen_bodies(&api);
        assert_eq!(
            bodies[0]["requests"][0]["updateFormInfo"]["updateMask"],
            "title"
        );
    }

    #[test]
    fn updates_a_question_found_by_title() {
        let updated_form = r#"{"form":{"formId":"f1","info":{"title":"Survey"},"items":[
            {"itemId":"q1","title":"Your name","questionItem":{"question":{"textQuestion":{"paragraph":false}}}},
            {"itemId":"q2","title":"Favorite colour?","questionItem":{"question":{"required":true,"choiceQuestion":{"type":"RADIO","options":[{"value":"Red"},{"value":"Blue"},{"value":"Green"}]}}}}
        ]}}"#;
        let api = client(&[
            ((Method::Get, FORM_URL), 200, FORM_BODY),
            ((Method::Post, BATCH_URL), 200, updated_form),
        ]);

        let response = run_tool(
            &api,
            "updateQuestion",
            r#"{"formId":"f1","question":"favorite COLOR","title":"Favorite colour?","required":true,"options":["Red","Blue","Green"]}"#,
        )
        .unwrap();
        let answered: Value = serde_json::from_str(structured(&response)).unwrap();
        assert_eq!(answered["id"], "q2");
        assert_eq!(answered["title"], "Favorite colour?");
        assert_eq!(answered["required"], true);
        assert_eq!(answered["options"], json!(["Red", "Blue", "Green"]));

        let bodies = seen_bodies(&api);
        assert_eq!(bodies.len(), 1);
        let update = &bodies[0]["requests"][0]["updateItem"];
        // The matched question is the second item.
        assert_eq!(update["location"]["index"], 1);
        assert_eq!(update["item"]["itemId"], "q2");
        // The choice type is preserved when options are replaced.
        assert_eq!(
            update["item"]["questionItem"]["question"]["choiceQuestion"]["type"],
            "RADIO"
        );
        assert_eq!(
            update["updateMask"],
            "title,questionItem.question.required,questionItem.question.choiceQuestion"
        );
    }

    #[test]
    fn updates_a_question_by_id_with_description_only() {
        let updated_form = r#"{"form":{"formId":"f1","info":{"title":"Survey"},"items":[
            {"itemId":"q1","title":"Your name","description":"First and last","questionItem":{"question":{"required":true,"textQuestion":{"paragraph":false}}}}
        ]}}"#;
        let api = client(&[
            ((Method::Get, FORM_URL), 200, FORM_BODY),
            ((Method::Post, BATCH_URL), 200, updated_form),
        ]);

        let response = run_tool(
            &api,
            "updateQuestion",
            r#"{"formId":"f1","question":"q1","description":"First and last"}"#,
        )
        .unwrap();
        let answered: Value = serde_json::from_str(structured(&response)).unwrap();
        assert_eq!(answered["description"], "First and last");

        let bodies = seen_bodies(&api);
        assert_eq!(bodies.len(), 1);
        let update = &bodies[0]["requests"][0]["updateItem"];
        assert_eq!(update["location"]["index"], 0);
        assert_eq!(update["updateMask"], "description");
        assert!(update["item"].get("questionItem").is_none());
    }

    #[test]
    fn updates_only_a_questions_required_flag() {
        let updated_form = r#"{"form":{"formId":"f1","info":{"title":"Survey"},"items":[
            {"itemId":"q1","title":"Your name","questionItem":{"question":{"required":false,"textQuestion":{"paragraph":false}}}}
        ]}}"#;
        let api = client(&[
            ((Method::Get, FORM_URL), 200, FORM_BODY),
            ((Method::Post, BATCH_URL), 200, updated_form),
        ]);

        let response = run_tool(
            &api,
            "updateQuestion",
            r#"{"formId":"f1","question":"q1","required":false}"#,
        )
        .unwrap();
        let answered: Value = serde_json::from_str(structured(&response)).unwrap();
        assert_eq!(answered["required"], false);

        let bodies = seen_bodies(&api);
        assert_eq!(bodies.len(), 1);
        let update = &bodies[0]["requests"][0]["updateItem"];
        assert_eq!(update["updateMask"], "questionItem.question.required");
        assert!(update["item"].get("title").is_none());
    }

    #[test]
    fn updates_options_on_a_kindless_question() {
        let body = r#"{"formId":"f1","info":{"title":"Survey"},"items":[
            {"itemId":"q9","title":"Odd","questionItem":{}}
        ]}"#;
        let api = client(&[
            ((Method::Get, FORM_URL), 200, body),
            ((Method::Post, BATCH_URL), 200, r#"{"form":{}}"#),
        ]);

        let response = run_tool(
            &api,
            "updateQuestion",
            r#"{"formId":"f1","question":"q9","options":["a"]}"#,
        )
        .unwrap();
        // The updated form has no items, so the answer is empty.
        assert_eq!(structured(&response), "{}");

        let bodies = seen_bodies(&api);
        assert_eq!(bodies.len(), 1);
        let update = &bodies[0]["requests"][0]["updateItem"];
        assert_eq!(update["updateMask"], "questionItem.question.choiceQuestion");
    }

    #[test]
    fn refuses_option_updates_on_non_choice_questions() {
        let api = client(&[((Method::Get, FORM_URL), 200, FORM_BODY)]);
        let failed = run_tool(
            &api,
            "updateQuestion",
            r#"{"formId":"f1","question":"q1","options":["a"]}"#,
        );
        assert!(
            matches!(&failed, Err(error) if error.message.contains("options only apply")),
            "{failed:?}"
        );

        let nothing = run_tool(&api, "updateQuestion", r#"{"formId":"f1","question":"q1"}"#);
        assert!(
            matches!(&nothing, Err(error) if error.message.contains("Nothing to update")),
            "{nothing:?}"
        );
    }

    #[test]
    fn reports_unknown_or_ambiguous_questions() {
        let api = client(&[((Method::Get, FORM_URL), 200, FORM_BODY)]);

        let missing = run_tool(
            &api,
            "deleteQuestion",
            r#"{"formId":"f1","question":"Nope"}"#,
        );
        assert!(
            matches!(&missing, Err(error) if error.message.contains(r#"No question "Nope""#)
                && error.message.contains("Your name, Favorite color")),
            "{missing:?}"
        );

        let dupe_body = r#"{"formId":"f1","items":[
            {"itemId":"d1","title":"Dupe","questionItem":{"question":{}}},
            {"itemId":"d2","title":"Dupe","questionItem":{"question":{}}}
        ]}"#;
        let dupe_api = client(&[((Method::Get, FORM_URL), 200, dupe_body)]);
        let dupe = run_tool(
            &dupe_api,
            "deleteQuestion",
            r#"{"formId":"f1","question":"dupe"}"#,
        );
        assert!(
            matches!(&dupe, Err(error) if error.message.contains("Several questions are titled")
                && error.message.contains("d1, d2")),
            "{dupe:?}"
        );

        let empty_api = client(&[((Method::Get, FORM_URL), 200, r#"{"formId":"f1"}"#)]);
        let empty = run_tool(
            &empty_api,
            "deleteQuestion",
            r#"{"formId":"f1","question":"Nope"}"#,
        );
        assert!(
            matches!(&empty, Err(error) if error.message == r#"No question "Nope" on this form; its questions are: "#),
            "{empty:?}"
        );
    }

    #[test]
    fn deletes_a_question() {
        let api = client(&[
            ((Method::Get, FORM_URL), 200, FORM_BODY),
            ((Method::Post, BATCH_URL), 204, ""),
        ]);
        let deleted =
            run_tool(&api, "deleteQuestion", r#"{"formId":"f1","question":"q1"}"#).unwrap();
        assert_eq!(structured(&deleted), r#"{"deleted":true}"#);

        let bodies = seen_bodies(&api);
        assert_eq!(bodies.len(), 1);
        assert_eq!(
            bodies[0]["requests"][0]["deleteItem"]["location"]["index"],
            0
        );
    }

    #[test]
    fn publishes_a_form() {
        let api = client(&[(
            (
                Method::Post,
                "https://forms.test/v1/forms/f1:setPublishSettings",
            ),
            200,
            r#"{"formId":"f1","publishSettings":{"publishState":{"isPublished":true,"isAcceptingResponses":true}}}"#,
        )]);

        let published =
            run_tool(&api, "publishForm", r#"{"formId":"f1","published":true}"#).unwrap();
        let answered: Value = serde_json::from_str(structured(&published)).unwrap();
        assert_eq!(answered["publishState"]["isPublished"], true);
        assert_eq!(answered["publishState"]["isAcceptingResponses"], true);

        let bodies = seen_bodies(&api);
        assert_eq!(
            bodies[0]["publishSettings"]["publishState"],
            json!({"isPublished": true, "isAcceptingResponses": true})
        );
    }

    #[test]
    fn unpublishes_a_form() {
        let api = client(&[(
            (
                Method::Post,
                "https://forms.test/v1/forms/f1:setPublishSettings",
            ),
            200,
            r#"{"formId":"f1","publishSettings":{"publishState":{}}}"#,
        )]);

        let unpublished =
            run_tool(&api, "publishForm", r#"{"formId":"f1","published":false}"#).unwrap();
        assert_eq!(structured(&unpublished), r#"{"publishState":{}}"#);

        let bodies = seen_bodies(&api);
        assert_eq!(
            bodies[0]["publishSettings"]["publishState"],
            json!({"isPublished": false, "isAcceptingResponses": false})
        );
    }

    #[test]
    fn gets_responses_keyed_by_question_title() {
        let responses = r#"{
            "responses": [
                {"responseId":"r1","lastSubmittedTime":"2026-10-11T10:00:00Z",
                 "answers":{
                     "q1":{"questionId":"q1","textAnswers":{"answers":[{"value":"Ada"}]}},
                     "q2":{"questionId":"q2","textAnswers":{"answers":[{"value":"Blue"},{"value":"Green"}]}},
                     "q9":{"questionId":"q9","textAnswers":{"answers":[{"value":"?"}]}},
                     "qx":{"questionId":"qx"}
                 }},
                {"responseId":"r2","lastSubmittedTime":"2026-10-11T11:00:00Z"}
            ],
            "nextPageToken": "tok"
        }"#;
        let api = client(&[
            ((Method::Get, FORM_URL), 200, FORM_BODY),
            (
                (
                    Method::Get,
                    "https://forms.test/v1/forms/f1/responses?pageSize=10&pageToken=abc",
                ),
                200,
                responses,
            ),
        ]);

        let response = run_tool(
            &api,
            "getResponses",
            r#"{"formId":"f1","pageSize":10,"pageToken":"abc"}"#,
        )
        .unwrap();
        let answered: Value = serde_json::from_str(structured(&response)).unwrap();
        assert_eq!(answered["nextPageToken"], "tok");
        assert_eq!(
            answered["responses"][0],
            json!({
                "responseId": "r1",
                "submittedAt": "2026-10-11T10:00:00Z",
                "answers": {"Your name": "Ada", "Favorite color": "Blue, Green", "q9": "?", "qx": ""}
            })
        );
        // A response without answers still shows up.
        assert_eq!(
            answered["responses"][1],
            json!({"responseId": "r2", "submittedAt": "2026-10-11T11:00:00Z", "answers": {}})
        );
    }

    #[test]
    fn gets_responses_without_pagination() {
        let api = client(&[
            ((Method::Get, FORM_URL), 200, FORM_BODY),
            (
                (Method::Get, "https://forms.test/v1/forms/f1/responses"),
                200,
                r#"{"responses":[]}"#,
            ),
        ]);
        let response = run_tool(&api, "getResponses", r#"{"formId":"f1"}"#).unwrap();
        let answered: Value = serde_json::from_str(structured(&response)).unwrap();
        assert_eq!(answered["responses"], json!([]));
        assert!(answered.get("nextPageToken").is_none());
    }

    #[test]
    fn encodes_special_characters_in_form_ids() {
        let api = client(&[(
            (Method::Get, "https://forms.test/v1/forms/a%20b"),
            200,
            FORM_BODY,
        )]);
        let response = run_tool(&api, "getForm", r#"{"formId":"a b"}"#).unwrap();
        assert!(structured(&response).contains("Survey"));
    }

    #[test]
    fn surfaces_delete_and_publish_failures() {
        let delete_api = client(&[
            ((Method::Get, FORM_URL), 200, FORM_BODY),
            ((Method::Post, BATCH_URL), 403, "forbidden"),
        ]);
        let failed = run_tool(
            &delete_api,
            "deleteQuestion",
            r#"{"formId":"f1","question":"q1"}"#,
        );
        assert!(
            matches!(&failed, Err(error) if error.message == "The API answered 403: forbidden"),
            "{failed:?}"
        );

        let publish_api = client(&[(
            (
                Method::Post,
                "https://forms.test/v1/forms/f1:setPublishSettings",
            ),
            403,
            "forbidden",
        )]);
        let failed = run_tool(
            &publish_api,
            "publishForm",
            r#"{"formId":"f1","published":true}"#,
        );
        assert!(
            matches!(&failed, Err(error) if error.message == "The API answered 403: forbidden"),
            "{failed:?}"
        );
    }

    #[test]
    fn surfaces_write_failures() {
        let api = client(&[
            ((Method::Post, FORMS), 403, "forbidden"),
            ((Method::Get, FORM_URL), 200, FORM_BODY),
            ((Method::Post, BATCH_URL), 403, "forbidden"),
        ]);

        let failed = run_tool(&api, "createForm", r#"{"title":"x","questions":[]}"#);
        assert!(
            matches!(&failed, Err(error) if error.message == "The API answered 403: forbidden" && error.status == Some(403)),
            "{failed:?}"
        );

        let failed = run_tool(
            &api,
            "addQuestions",
            r#"{"formId":"f1","questions":[{"type":"text","title":"?"}]}"#,
        );
        assert!(
            matches!(&failed, Err(error) if error.message == "The API answered 403: forbidden"),
            "{failed:?}"
        );

        // createForm's second call (the description batchUpdate) failing.
        let created = client(&[
            ((Method::Post, FORMS), 200, r#"{"formId":"f1"}"#),
            ((Method::Post, BATCH_URL), 403, "forbidden"),
        ]);
        let failed = run_tool(
            &created,
            "createForm",
            r#"{"title":"x","questions":[],"description":"D"}"#,
        );
        assert!(
            matches!(&failed, Err(error) if error.message == "The API answered 403: forbidden"),
            "{failed:?}"
        );

        // updateQuestion's batchUpdate failing after the question resolved.
        let failed = run_tool(
            &api,
            "updateQuestion",
            r#"{"formId":"f1","question":"q1","description":"D"}"#,
        );
        assert!(
            matches!(&failed, Err(error) if error.message == "The API answered 403: forbidden"),
            "{failed:?}"
        );
    }

    #[test]
    fn rejects_bad_specs_when_adding_questions() {
        let api = client(&[((Method::Get, FORM_URL), 200, FORM_BODY)]);

        let bad = run_tool(
            &api,
            "addQuestions",
            r#"{"formId":"f1","questions":[{"type":"essay","title":"?"}]}"#,
        );
        assert!(
            matches!(&bad, Err(error) if error.message.contains("Unknown question type")),
            "{bad:?}"
        );
    }

    #[test]
    fn updates_form_info_with_description_only() {
        let api = client(&[(
            (Method::Post, BATCH_URL),
            200,
            r#"{"form":{"formId":"f1","info":{"description":"New desc"}}}"#,
        )]);

        run_tool(
            &api,
            "updateFormInfo",
            r#"{"formId":"f1","description":"New desc"}"#,
        )
        .unwrap();
        let bodies = seen_bodies(&api);
        assert_eq!(
            bodies[0]["requests"][0]["updateFormInfo"]["updateMask"],
            "description"
        );
    }

    #[test]
    fn skips_items_without_ids_in_responses() {
        let form = r#"{
            "formId": "f1",
            "items": [
                {"title": "Nameless", "questionItem": {"question": {"textQuestion": {}}}},
                {"itemId": "q1", "questionItem": {"question": {"textQuestion": {}}}},
                {"itemId": 42, "title": "Numeric id", "questionItem": {"question": {"textQuestion": {}}}},
                {"itemId": "q2", "title": 7, "questionItem": {"question": {"textQuestion": {}}}}
            ]
        }"#;
        let responses = r#"{"responses": []}"#;
        let api = client(&[
            ((Method::Get, FORM_URL), 200, form),
            (
                (Method::Get, "https://forms.test/v1/forms/f1/responses"),
                200,
                responses,
            ),
        ]);

        // Items without an itemId or title — or with a non-string id or
        // title — cannot key answers, so they are skipped instead of
        // failing the call.
        let answered = run_tool(&api, "getResponses", r#"{"formId":"f1"}"#).unwrap();
        let structured: Value =
            serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(structured["responses"], json!([]));
    }

    #[test]
    fn surfaces_responses_fetch_failure() {
        // The form loads but the responses call fails.
        let api = client(&[((Method::Get, FORM_URL), 200, FORM_BODY)]);
        let failed = run_tool(&api, "getResponses", r#"{"formId":"f1"}"#);
        assert!(
            matches!(&failed, Err(error) if error.message.contains("no canned answer")),
            "{failed:?}"
        );
    }

    #[test]
    fn surfaces_transport_and_api_failures() {
        let failing = Api::new(BASE, Fake::failing());
        let failed = run(&failing, &request("getForm", r#"{"formId":"f1"}"#));
        assert!(
            matches!(&failed, Err(error) if error.message == "transport boom" && error.status.is_none()),
            "{failed:?}"
        );

        // Every tool's first send surfaces a transport failure.
        for (tool, body) in [
            ("createForm", r#"{"title":"x","questions":[]}"#),
            ("getForm", r#"{"formId":"f1"}"#),
            (
                "addQuestions",
                r#"{"formId":"f1","questions":[{"type":"text","title":"?"}]}"#,
            ),
            ("updateFormInfo", r#"{"formId":"f1","title":"T"}"#),
            (
                "updateQuestion",
                r#"{"formId":"f1","question":"q1","description":"D"}"#,
            ),
            ("deleteQuestion", r#"{"formId":"f1","question":"q1"}"#),
            ("publishForm", r#"{"formId":"f1","published":true}"#),
            ("getResponses", r#"{"formId":"f1"}"#),
        ] {
            let failed = run(&failing, &request(tool, body));
            assert!(
                matches!(&failed, Err(error) if error.message == "transport boom"),
                "{tool}: {failed:?}"
            );
        }

        let garbled = client(&[((Method::Get, FORM_URL), 200, "<html>")]);
        let parsed = run(&garbled, &request("getForm", r#"{"formId":"f1"}"#));
        assert!(
            matches!(&parsed, Err(error) if error.status.is_none()),
            "{parsed:?}"
        );

        // A garbled form answer fails getResponses too.
        let garbled = run(&garbled, &request("getResponses", r#"{"formId":"f1"}"#));
        assert!(
            matches!(&garbled, Err(error) if error.status.is_none()),
            "{garbled:?}"
        );

        let uncanned = client(&[]);
        let missing = uncanned.get("https://forms.test/v1/nope");
        assert!(
            matches!(&missing, Err(error) if error.message.contains("no canned answer")),
            "{missing:?}"
        );
    }

    #[test]
    fn refuses_bad_input() {
        let api = client(&[]);
        let malformed = run(&api, &request("createForm", "{oops"));
        assert!(
            matches!(&malformed, Err(error) if error.status.is_none()),
            "{malformed:?}"
        );

        let missing = run(&api, &request("createForm", "{}"));
        assert!(missing.is_err(), "{missing:?}");

        let unknown = run(&api, &request("nope", "{}"));
        assert!(
            matches!(&unknown, Err(error) if error.message == "no tool nope"),
            "{unknown:?}"
        );

        // Every tool parses its input first, so malformed JSON fails before
        // anything is sent.
        for (tool, body) in [
            ("createForm", r#"{"title":"x","questions":[]}"#),
            ("getForm", r#"{"formId":"f1"}"#),
            ("addQuestions", r#"{"formId":"f1","questions":[]}"#),
            ("updateFormInfo", r#"{"formId":"f1","title":"T"}"#),
            ("updateQuestion", r#"{"formId":"f1","question":"q1"}"#),
            ("deleteQuestion", r#"{"formId":"f1","question":"q1"}"#),
            ("publishForm", r#"{"formId":"f1","published":true}"#),
            ("getResponses", r#"{"formId":"f1"}"#),
        ] {
            let api = client(&[]);
            let malformed = run_tool(&api, tool, "{oops");
            assert!(
                matches!(&malformed, Err(error) if error.status.is_none() && !error.message.is_empty()),
                "{tool} with {body}: {malformed:?}"
            );
            assert!(
                api.transport.seen.borrow().is_empty(),
                "{tool} sent a request for malformed input"
            );
        }
    }

    #[test]
    fn maps_tool_failures_through_call_tool() {
        let unknown = Gforms::call_tool(request("nope", "{}")).unwrap();
        assert_eq!(unknown.structured, None);
    }

    #[test]
    fn off_wasm_nothing_is_sent() {
        // Failures surface as tool errors, not call errors.
        let sent = Gforms::call_tool(request("getForm", r#"{"formId":"f1"}"#)).unwrap();
        assert_eq!(sent.structured, None);

        let guest = Guest.send(Request::get("https://forms.test/"));
        assert!(matches!(&guest, Err(ConnectorError::Upstream(m)) if m.contains("wasm32-wasip2")));
    }

    #[test]
    fn trims_a_trailing_slash_off_the_base() {
        let api = Api::new("https://forms.test/v1/", Fake::answering(&[]));
        assert_eq!(api.forms, FORMS);
    }

    #[test]
    fn manifest_is_embedded() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["name"], "gforms");
        assert_eq!(manifest["auth"]["schemes"][0]["type"], "oauth2");
        assert_eq!(
            manifest["auth"]["schemes"][0]["scopes"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            manifest["permissions"]["http"]["allow"][0],
            "https://forms.googleapis.com"
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
                "createForm",
                "getForm",
                "addQuestions",
                "updateFormInfo",
                "updateQuestion",
                "deleteQuestion",
                "publishForm",
                "getResponses",
            ]
        );
    }
}
