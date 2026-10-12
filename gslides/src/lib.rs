//! Google Slides connector: read, create and edit presentations through the
//! Google Slides API v1. The host runs the OAuth 2.0 sign-in, refreshes the
//! token and adds `Authorization: Bearer …` after each request leaves the
//! guest: this code never sees it.
//!
//! The tools are shaped around what an agent wants to accomplish — adding a
//! slide with its title and body filled in, rewriting a slide's text,
//! reordering the deck — rather than the API's endpoints. A tool may make
//! several HTTP calls to finish its job (`addSlide` creates the slide, reads
//! its placeholders back, then writes the text); tests inject a stub sender
//! so the whole orchestration is covered without a network.

use connectors_sdk::http::{Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::Deserialize;
use serde_json::{Value, json};

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://slides.googleapis.com";

/// EMUs in one inch: the Slides API measures sizes in English Metric Units.
const EMU_PER_INCH: f64 = 914_400.0;

/// The layout `addSlide` uses unless asked otherwise.
const DEFAULT_LAYOUT: &str = "TITLE_AND_BODY";

/// The predefined layouts `addSlide` accepts.
const LAYOUTS: &[&str] = &[
    "BLANK",
    "CAPTION_ONLY",
    "TITLE",
    "TITLE_AND_BODY",
    "TITLE_AND_TWO_COLUMNS",
    "TITLE_ONLY",
    "SECTION_HEADER",
    "SECTION_TITLE_AND_DESCRIPTION",
    "ONE_COLUMN_TEXT",
    "MAIN_POINT",
    "BIG_NUMBER",
];

/// The thumbnail sizes `getSlideThumbnail` accepts.
const THUMBNAIL_SIZES: &[&str] = &["SMALL", "MEDIUM", "LARGE"];

/// The thumbnail size used unless asked otherwise.
const DEFAULT_THUMBNAIL_SIZE: &str = "LARGE";

/// Placeholder types that carry a slide's title, in preference order.
const TITLE_PLACEHOLDERS: &[&str] = &["TITLE", "CENTER_TITLE", "SUBTITLE"];

/// Placeholder types that carry a slide's body, in preference order.
const BODY_PLACEHOLDERS: &[&str] = &["BODY", "TEXT"];

/// How much body text `getPresentation` keeps per slide.
const BODY_PREVIEW_LEN: usize = 200;

/// How much element text `getSlide` keeps per element.
const ELEMENT_PREVIEW_LEN: usize = 80;

/// The connector.
#[derive(Debug)]
pub struct Gslides;

#[derive(Debug, Deserialize)]
struct CreatePresentation {
    title: String,
}

#[derive(Debug, Deserialize)]
struct GetPresentation {
    presentation_id: String,
}

#[derive(Debug, Deserialize)]
struct GetSlide {
    presentation_id: String,
    slide_id: String,
}

#[derive(Debug, Deserialize)]
struct AddSlide {
    presentation_id: String,
    layout: Option<String>,
    title: Option<String>,
    body: Option<String>,
    index: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct SetSlideText {
    presentation_id: String,
    slide_id: String,
    title: Option<String>,
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AddImage {
    presentation_id: String,
    slide_id: String,
    image_url: String,
    width_inches: Option<f64>,
    height_inches: Option<f64>,
    x_inches: Option<f64>,
    y_inches: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ReplaceText {
    presentation_id: String,
    text: String,
    replacement: String,
    page_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct DeleteSlide {
    presentation_id: String,
    slide_id: String,
}

#[derive(Debug, Deserialize)]
struct ReorderSlides {
    presentation_id: String,
    slide_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GetSlideThumbnail {
    presentation_id: String,
    slide_id: String,
    size: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BatchUpdate {
    presentation_id: String,
    requests: Vec<Value>,
    write_control: Option<Value>,
}

/// A text placeholder on a slide: its kind, object ID and current text.
struct TextPlaceholder {
    kind: String,
    object_id: String,
    text: String,
}

impl Connector for Gslides {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        Self::call_with(request, |request| Ok(request.clone().send()?))
    }
}

impl Gslides {
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
            "createPresentation" => create_presentation(&api, &request, &mut send),
            "getPresentation" => get_presentation(&api, &request, &mut send),
            "getSlide" => get_slide(&api, &request, &mut send),
            "addSlide" => add_slide(&api, &request, &mut send),
            "setSlideText" => set_slide_text(&api, &request, &mut send),
            "addImage" => one_call(&api, &request, &mut send, plan_add_image),
            "replaceText" => replace_text(&api, &request, &mut send),
            "deleteSlide" => delete_slide(&api, &request, &mut send),
            "reorderSlides" => reorder_slides(&api, &request, &mut send),
            "getSlideThumbnail" => get_slide_thumbnail(&api, &request, &mut send),
            "batchUpdate" => one_call(&api, &request, &mut send, plan_batch_update),
            other => Err(ConnectorError::NotFound(format!("unknown tool: {other}"))),
        }
    }
}

connectors_sdk::export_connector!(Gslides);

/// The Slides API endpoints derived from the configured base URL.
struct Api {
    presentations: String,
}

impl Api {
    fn new(base: &str) -> Self {
        let root = base.trim_end_matches('/');
        let presentations = format!("{root}/v1/presentations");
        Self { presentations }
    }

    /// The URL of one presentation's resource.
    fn presentation(&self, presentation_id: &str) -> String {
        format!("{}/{}", self.presentations, encode(presentation_id))
    }

    /// The URL a tool's batchUpdate posts to.
    fn batch_update(&self, presentation_id: &str) -> String {
        format!("{}:batchUpdate", self.presentation(presentation_id))
    }

    /// The URL of one page (slide, layout or master).
    fn page(&self, presentation_id: &str, page_id: &str) -> String {
        format!(
            "{}/pages/{}",
            self.presentation(presentation_id),
            encode(page_id)
        )
    }

    /// The URL that renders one page's thumbnail.
    fn thumbnail(&self, presentation_id: &str, page_id: &str) -> String {
        format!("{}/thumbnail", self.page(presentation_id, page_id))
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

/// Runs the calls behind `createPresentation`: the API's answer plus the
/// presentation's edit URL, which the API itself does not return.
fn create_presentation(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: CreatePresentation = request.json()?;
    let created = send(&Request::post(api.presentations.clone()).json(&json!({
        "title": input.title
    }))?)?; // coverage: exclude — Request::json serializes a serde_json::Value, which is infallible
    if let Err(error) = step_ok(&created, "creating the presentation") {
        return Ok(error);
    }
    let body: Value = created.json()?;
    let presentation_id = body
        .get("presentationId")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ConnectorError::Upstream("the API did not return the new presentation's ID".into())
        })?;
    ToolResponse::structured(&object(&[
        ("presentationId", Some(json!(presentation_id))),
        ("title", body.get("title").cloned()),
        (
            "url",
            Some(json!(format!(
                "https://docs.google.com/presentation/d/{presentation_id}/edit"
            ))),
        ),
    ]))
}

/// Runs the calls behind `getPresentation`: one fetch with a tight field mask,
/// answered as a compact per-slide summary instead of the full resource.
fn get_presentation(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: GetPresentation = request.json()?;
    let url = with_query(
        &api.presentation(&input.presentation_id),
        &[(
            "fields",
            Some(
                "title,pageSize,slides(objectId,slideProperties(layoutObjectId),\
                 pageElements(objectId,shape(placeholder(type),text)))"
                    .into(),
            ),
        )],
    );
    let fetched = send(&json_get(url))?;
    if let Err(error) = step_ok(&fetched, "reading the presentation") {
        return Ok(error);
    }
    let body: Value = fetched.json()?;
    ToolResponse::structured(&object(&[
        ("presentationId", Some(json!(input.presentation_id))),
        ("title", body.get("title").cloned()),
        ("slideCount", Some(json!(slide_ids(&body).len()))),
        ("pageSize", page_size(&body)),
        ("slides", Some(json!(summarize_slides(&body)))),
    ]))
}

/// Runs the calls behind `getSlide`: one page fetch, answered as the slide's
/// elements with their types and text previews.
fn get_slide(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: GetSlide = request.json()?;
    let url = with_query(
        &api.page(&input.presentation_id, &input.slide_id),
        &[(
            "fields",
            Some(
                "objectId,pageElements(objectId,shape(shapeType,placeholder(type),text),\
                 image,table,video,line,wordArt,sheetsChart,group)"
                    .into(),
            ),
        )],
    );
    let fetched = send(&json_get(url))?;
    if let Err(error) = step_ok(&fetched, "reading the slide") {
        return Ok(error);
    }
    let body: Value = fetched.json()?;
    ToolResponse::structured(&object(&[
        ("objectId", body.get("objectId").cloned()),
        ("elements", Some(json!(summarize_elements(&body)))),
    ]))
}

/// Runs the calls behind `addSlide`: create the slide, then — when title or
/// body text was given — read the new slide's placeholders back and write
/// the text into them.
fn add_slide(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: AddSlide = request.json()?;
    let layout = input.layout.as_deref().unwrap_or(DEFAULT_LAYOUT);
    if !LAYOUTS.contains(&layout) {
        return Err(ConnectorError::InvalidInput(format!(
            "unknown layout \"{layout}\"; expected one of {}",
            LAYOUTS.join(", ")
        )));
    }
    let mut create = json!({
        "createSlide": {
            "slideLayoutReference": { "predefinedLayout": layout },
        }
    });
    if let Some(index) = input.index {
        create["createSlide"]["insertionIndex"] = json!(index);
    }
    let planned = batch_update_request(api, &input.presentation_id, &[create], None)?; // coverage: exclude — batch_update_request only fails serializing a serde_json::Value, which is infallible
    let created = send(&planned)?;
    if let Err(error) = step_ok(&created, "creating the slide") {
        return Ok(error);
    }
    let created_body: Value = created.json()?;
    let slide_id = created_body
        .pointer("/replies/0/createSlide/objectId")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ConnectorError::Upstream("the API did not return the new slide's ID".into())
        })?;

    if input.title.is_none() && input.body.is_none() {
        return ToolResponse::structured(&json!({ "slideId": slide_id }));
    }

    let page = send(&json_get(with_query(
        &api.page(&input.presentation_id, slide_id),
        &[(
            "fields",
            Some("pageElements(objectId,shape(placeholder(type)))".into()),
        )],
    )))?;
    if let Err(error) = step_ok(&page, "reading the new slide") {
        return Ok(error);
    }
    let page_body: Value = page.json()?;
    let placeholders = text_placeholders(&page_body);

    let mut requests = Vec::new();
    if let Some(title) = &input.title {
        let object_id = placeholder_id(&placeholders, TITLE_PLACEHOLDERS).ok_or_else(|| {
            ConnectorError::InvalidInput(format!("layout \"{layout}\" has no title placeholder"))
        })?;
        requests.push(insert_text_request(&object_id, title));
    }
    if let Some(body) = &input.body {
        let object_id = placeholder_id(&placeholders, BODY_PLACEHOLDERS).ok_or_else(|| {
            ConnectorError::InvalidInput(format!("layout \"{layout}\" has no body placeholder"))
        })?;
        requests.push(insert_text_request(&object_id, body));
    }
    let planned = batch_update_request(api, &input.presentation_id, &requests, None)?; // coverage: exclude — batch_update_request only fails serializing a serde_json::Value, which is infallible
    let written = send(&planned)?;
    if let Err(error) = step_ok(&written, "writing the slide text") {
        return Ok(error);
    }
    ToolResponse::structured(&json!({ "slideId": slide_id }))
}

/// Runs the calls behind `setSlideText`: read the slide's placeholders, then
/// replace the title and/or body placeholder's text.
fn set_slide_text(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: SetSlideText = request.json()?;
    if input.title.is_none() && input.body.is_none() {
        return Err(ConnectorError::InvalidInput(
            "set the title and/or the body".into(),
        ));
    }
    let page = send(&json_get(with_query(
        &api.page(&input.presentation_id, &input.slide_id),
        &[(
            "fields",
            Some("pageElements(objectId,shape(placeholder(type)))".into()),
        )],
    )))?;
    if let Err(error) = step_ok(&page, "reading the slide") {
        return Ok(error);
    }
    let page_body: Value = page.json()?;
    let placeholders = text_placeholders(&page_body);

    let mut requests = Vec::new();
    for (kinds, what, text) in [
        (TITLE_PLACEHOLDERS, "title", &input.title),
        (BODY_PLACEHOLDERS, "body", &input.body),
    ] {
        if let Some(text) = text {
            let object_id = placeholder_id(&placeholders, kinds).ok_or_else(|| {
                ConnectorError::InvalidInput(format!("the slide has no {what} placeholder"))
            })?;
            requests.push(delete_text_request(&object_id));
            requests.push(insert_text_request(&object_id, text));
        }
    }
    let planned = batch_update_request(api, &input.presentation_id, &requests, None)?; // coverage: exclude — batch_update_request only fails serializing a serde_json::Value, which is infallible
    let written = send(&planned)?;
    if let Err(error) = step_ok(&written, "writing the slide text") {
        return Ok(error);
    }
    ToolResponse::structured(&json!({ "slideId": input.slide_id }))
}

/// Plans `addImage`: a `createImage` request with the size in EMU and the
/// position in the transform; both are left to the API's defaults unless the
/// caller says otherwise. The API keeps the image's aspect ratio inside the
/// given size.
fn plan_add_image(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: AddImage = request.json()?;
    let mut properties = serde_json::Map::new();
    properties.insert("pageObjectId".to_string(), json!(input.slide_id));
    let size = match (input.width_inches, input.height_inches) {
        (None, None) => None,
        (Some(width), None) | (None, Some(width)) => Some((width, width)),
        (Some(width), Some(height)) => Some((width, height)),
    };
    if let Some((width, height)) = size {
        properties.insert(
            "size".to_string(),
            json!({
                "width": dimension(width),
                "height": dimension(height),
            }),
        );
    }
    if input.x_inches.is_some() || input.y_inches.is_some() {
        properties.insert(
            "transform".to_string(),
            json!({
                "scaleX": 1,
                "scaleY": 1,
                "translateX": dimension(input.x_inches.unwrap_or(0.0)),
                "translateY": dimension(input.y_inches.unwrap_or(0.0)),
                "unit": "EMU",
            }),
        );
    }
    let create = json!({
        "createImage": {
            "elementProperties": properties,
            "sourceUrl": input.image_url,
        }
    });
    batch_update_request(api, &input.presentation_id, &[create], None)
}

/// Runs the calls behind `replaceText`: one `replaceAllText`, answered with
/// the number of changed occurrences.
fn replace_text(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: ReplaceText = request.json()?;
    let mut replace = json!({
        "replaceAllText": {
            "containsText": { "text": input.text },
            "replaceText": input.replacement,
        }
    });
    if let Some(page_ids) = input.page_ids {
        replace["replaceAllText"]["pageObjectIds"] = json!(page_ids);
    }
    let planned = batch_update_request(api, &input.presentation_id, &[replace], None)?; // coverage: exclude — batch_update_request only fails serializing a serde_json::Value, which is infallible
    let updated = send(&planned)?;
    if let Err(error) = step_ok(&updated, "replacing the text") {
        return Ok(error);
    }
    let body: Value = updated.json()?;
    let changed = body
        .pointer("/replies/0/replaceAllText/occurrencesChanged")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    ToolResponse::structured(&json!({ "occurrencesChanged": changed }))
}

/// Runs the calls behind `deleteSlide`: a `deleteObject` on the slide.
/// Slides are deleted like any other page object; the API has no separate
/// delete-slide request.
fn delete_slide(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: DeleteSlide = request.json()?;
    let delete = json!({ "deleteObject": { "objectId": input.slide_id } });
    let planned = batch_update_request(api, &input.presentation_id, &[delete], None)?; // coverage: exclude — batch_update_request only fails serializing a serde_json::Value, which is infallible
    let updated = send(&planned)?;
    if let Err(error) = step_ok(&updated, "deleting the slide") {
        return Ok(error);
    }
    ToolResponse::structured(&json!({ "deleted": true, "slideId": input.slide_id }))
}

/// Runs the calls behind `reorderSlides`: reads the current order, checks the
/// requested IDs are the same slides, then moves each slide to the front in
/// reverse order so they land in the desired arrangement. A single
/// `updateSlidesPosition` can only move slides that are already in relative
/// order, so a general reorder needs one move per slide.
fn reorder_slides(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: ReorderSlides = request.json()?;
    let fetched = send(&json_get(with_query(
        &api.presentation(&input.presentation_id),
        &[("fields", Some("slides.objectId".into()))],
    )))?;
    if let Err(error) = step_ok(&fetched, "reading the slide order") {
        return Ok(error);
    }
    let body: Value = fetched.json()?;
    let current = slide_ids(&body);
    let mut wanted = input.slide_ids.clone();
    wanted.sort_unstable();
    let mut have = current.clone();
    have.sort_unstable();
    if wanted != have {
        return Err(ConnectorError::InvalidInput(
            "slide_ids must list every slide ID exactly once".into(),
        ));
    }
    if input.slide_ids == current {
        return ToolResponse::structured(&json!({ "slideIds": input.slide_ids }));
    }
    let requests: Vec<Value> = input
        .slide_ids
        .iter()
        .rev()
        .map(|slide_id| {
            json!({
                "updateSlidesPosition": {
                    "slideObjectIds": [slide_id],
                    "insertionIndex": 0,
                }
            })
        })
        .collect();
    let planned = batch_update_request(api, &input.presentation_id, &requests, None)?; // coverage: exclude — batch_update_request only fails serializing a serde_json::Value, which is infallible
    let moved = send(&planned)?;
    if let Err(error) = step_ok(&moved, "reordering the slides") {
        return Ok(error);
    }
    ToolResponse::structured(&json!({ "slideIds": input.slide_ids }))
}

/// Runs the calls behind `getSlideThumbnail`: renders the slide and answers
/// the thumbnail URL. The URL stays valid for about 30 minutes.
fn get_slide_thumbnail(
    api: &Api,
    request: &ToolRequest,
    send: &mut impl FnMut(&Request) -> Result<Response, ConnectorError>,
) -> Result<ToolResponse, ConnectorError> {
    let input: GetSlideThumbnail = request.json()?;
    let size = input.size.as_deref().unwrap_or(DEFAULT_THUMBNAIL_SIZE);
    if !THUMBNAIL_SIZES.contains(&size) {
        return Err(ConnectorError::InvalidInput(format!(
            "unknown size \"{size}\"; expected one of {}",
            THUMBNAIL_SIZES.join(", ")
        )));
    }
    let url = with_query(
        &api.thumbnail(&input.presentation_id, &input.slide_id),
        &[("thumbnailProperties.thumbnailSize", Some(size.to_string()))],
    );
    let fetched = send(&json_get(url))?;
    if let Err(error) = step_ok(&fetched, "rendering the thumbnail") {
        return Ok(error);
    }
    let body: Value = fetched.json()?;
    let content_url = body
        .get("contentUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| ConnectorError::Upstream("the API did not return a thumbnail URL".into()))?;
    ToolResponse::structured(&object(&[
        ("thumbnailUrl", Some(json!(content_url))),
        ("width", body.get("width").cloned()),
        ("height", body.get("height").cloned()),
    ]))
}

/// Plans `batchUpdate`: the low-level escape hatch for raw Slides API request
/// objects.
fn plan_batch_update(api: &Api, request: &ToolRequest) -> Result<Request, ConnectorError> {
    let input: BatchUpdate = request.json()?;
    batch_update_request(
        api,
        &input.presentation_id,
        &input.requests,
        input.write_control,
    )
}

/// A `:batchUpdate` POST carrying `requests`, plus an optional write control.
fn batch_update_request(
    api: &Api,
    presentation_id: &str,
    requests: &[Value],
    write_control: Option<Value>,
) -> Result<Request, ConnectorError> {
    let mut body = json!({ "requests": requests });
    if let Some(write_control) = write_control {
        body["writeControl"] = write_control;
    }
    Ok(Request::post(api.batch_update(presentation_id)).json(&body)?) // coverage: exclude — Request::json serializes a serde_json::Value here, which is infallible
}

/// An `insertText` request writing `text` at the start of a placeholder.
fn insert_text_request(object_id: &str, text: &str) -> Value {
    json!({
        "insertText": {
            "objectId": object_id,
            "insertionIndex": 0,
            "text": text,
        }
    })
}

/// A `deleteText` request clearing a whole shape's text.
fn delete_text_request(object_id: &str) -> Value {
    json!({
        "deleteText": {
            "objectId": object_id,
            "textRange": { "type": "ALL" },
        }
    })
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

/// A GET request that expects a JSON answer.
fn json_get(url: String) -> Request {
    Request::get(url).header("accept", "application/json")
}

/// The slide IDs of a presentation resource, in order.
fn slide_ids(presentation: &Value) -> Vec<String> {
    presentation
        .get("slides")
        .and_then(Value::as_array)
        .map(|slides| {
            slides
                .iter()
                .filter_map(|slide| {
                    slide
                        .get("objectId")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The presentation's page size in inches, if the API reported one.
fn page_size(presentation: &Value) -> Option<Value> {
    let size = presentation.get("pageSize")?;
    let width = size.pointer("/width/magnitude")?.as_f64()?;
    let height = size.pointer("/height/magnitude")?.as_f64()?;
    Some(json!({
        "widthInches": round3(width / EMU_PER_INCH),
        "heightInches": round3(height / EMU_PER_INCH),
    }))
}

/// Rounds to three decimals: enough for a size summary, no float noise.
fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// One slide of `getPresentation`'s summary: its ID, layout, title and a body
/// preview.
fn summarize_slides(presentation: &Value) -> Vec<Value> {
    presentation
        .get("slides")
        .and_then(Value::as_array)
        .map(|slides| slides.iter().map(summarize_slide).collect())
        .unwrap_or_default()
}

fn summarize_slide(slide: &Value) -> Value {
    let placeholders = text_placeholders(slide);
    object(&[
        ("objectId", slide.get("objectId").cloned()),
        (
            "layout",
            slide.pointer("/slideProperties/layoutObjectId").cloned(),
        ),
        (
            "title",
            find_placeholder(&placeholders, TITLE_PLACEHOLDERS).map(|found| json!(found.text)),
        ),
        (
            "bodyPreview",
            find_placeholder(&placeholders, BODY_PLACEHOLDERS)
                .map(|found| json!(preview(&found.text, BODY_PREVIEW_LEN))),
        ),
    ])
}

/// One element of `getSlide`'s answer: its ID, what kind of element it is,
/// and a text preview for shapes that have text.
fn summarize_elements(page: &Value) -> Vec<Value> {
    page.get("pageElements")
        .and_then(Value::as_array)
        .map(|elements| elements.iter().map(summarize_element).collect())
        .unwrap_or_default()
}

fn summarize_element(element: &Value) -> Value {
    let text = element.get("shape").map(shape_text).unwrap_or_default();
    object(&[
        ("objectId", element.get("objectId").cloned()),
        ("type", Some(json!(element_type(element)))),
        (
            "textPreview",
            (!text.is_empty()).then(|| json!(preview(&text, ELEMENT_PREVIEW_LEN))),
        ),
    ])
}

/// What kind of page element this is, by which union field is present.
fn element_type(element: &Value) -> &'static str {
    for (field, name) in [
        ("shape", "shape"),
        ("image", "image"),
        ("video", "video"),
        ("table", "table"),
        ("line", "line"),
        ("wordArt", "wordArt"),
        ("sheetsChart", "sheetsChart"),
        ("group", "group"),
    ] {
        if element.get(field).is_some() {
            return name;
        }
    }
    "unknown"
}

/// The text placeholders on a page: shapes whose placeholder has a type.
fn text_placeholders(page: &Value) -> Vec<TextPlaceholder> {
    let mut placeholders = Vec::new();
    let elements = page.get("pageElements").and_then(Value::as_array);
    let Some(elements) = elements else {
        return placeholders;
    };
    for element in elements {
        let shape = element.get("shape");
        let kind = shape
            .and_then(|shape| shape.pointer("/placeholder/type"))
            .and_then(Value::as_str);
        let object_id = element.get("objectId").and_then(Value::as_str);
        let (Some(kind), Some(object_id)) = (kind, object_id) else {
            continue;
        };
        placeholders.push(TextPlaceholder {
            kind: kind.to_string(),
            object_id: object_id.to_string(),
            text: shape.map(shape_text).unwrap_or_default(),
        });
    }
    placeholders
}

/// The first placeholder of one of `kinds`, in the caller's preference order.
fn find_placeholder<'a>(
    placeholders: &'a [TextPlaceholder],
    kinds: &[&str],
) -> Option<&'a TextPlaceholder> {
    placeholders
        .iter()
        .find(|placeholder| kinds.contains(&placeholder.kind.as_str()))
}

/// The object ID of the first placeholder of one of `kinds`.
fn placeholder_id(placeholders: &[TextPlaceholder], kinds: &[&str]) -> Option<String> {
    find_placeholder(placeholders, kinds).map(|found| found.object_id.clone())
}

/// All the text in a shape: the content of every text run, in order.
fn shape_text(shape: &Value) -> String {
    let elements = shape
        .pointer("/text/textElements")
        .and_then(Value::as_array);
    let Some(elements) = elements else {
        return String::new();
    };
    let mut text = String::new();
    for element in elements {
        if let Some(content) = element.pointer("/textRun/content").and_then(Value::as_str) {
            text.push_str(content);
        }
    }
    text
}

/// The first `len` characters of `text`.
fn preview(text: &str, len: usize) -> String {
    text.chars().take(len).collect()
}

/// A Slides API dimension of `inches` inches, in EMU.
fn dimension(inches: f64) -> Value {
    json!({
        "magnitude": (inches * EMU_PER_INCH).round() as i64,
        "unit": "EMU",
    })
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

    const API: &str = "https://slides.googleapis.com/v1/presentations";

    fn api() -> Api {
        Api::new("https://slides.test/")
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
        Gslides::call_with(request(tool, body), |request| stub.send(request))
    }

    const PRESENTATION: &str = r#"{
        "presentationId": "deck1",
        "title": "Q3 Review",
        "pageSize": {
            "width": { "magnitude": 9144000, "unit": "EMU" },
            "height": { "magnitude": 5143500, "unit": "EMU" }
        },
        "slides": [
            {
                "objectId": "slide1",
                "slideProperties": { "layoutObjectId": "layout_title_body" },
                "pageElements": [
                    {
                        "objectId": "title1",
                        "shape": {
                            "placeholder": { "type": "TITLE" },
                            "text": { "textElements": [
                                { "textRun": { "content": "Q3 Review" } },
                                { "paragraphMarker": {} }
                            ]}
                        }
                    },
                    {
                        "objectId": "body1",
                        "shape": {
                            "placeholder": { "type": "BODY" },
                            "text": { "textElements": [
                                { "textRun": { "content": "Revenue grew 12% this quarter." } }
                            ]}
                        }
                    },
                    { "objectId": "img1", "image": { "sourceUrl": "https://example.com/a.png" } }
                ]
            },
            {
                "objectId": "slide2",
                "slideProperties": { "layoutObjectId": "layout_blank" }
            }
        ]
    }"#;

    #[test]
    fn builds_api_urls() {
        let api = api();
        assert_eq!(
            api.presentation("a/b"),
            "https://slides.test/v1/presentations/a%2Fb"
        );
        assert_eq!(
            api.batch_update("deck1"),
            "https://slides.test/v1/presentations/deck1:batchUpdate"
        );
        assert_eq!(
            api.page("deck1", "slide 1"),
            "https://slides.test/v1/presentations/deck1/pages/slide%201"
        );
        assert_eq!(
            api.thumbnail("deck1", "slide1"),
            "https://slides.test/v1/presentations/deck1/pages/slide1/thumbnail"
        );
    }

    #[test]
    fn builds_query_strings() {
        assert_eq!(
            with_query("https://x.test/a", &[("q", None)]),
            "https://x.test/a"
        );
        assert_eq!(
            with_query(
                "https://x.test/a",
                &[("q", Some("a b".to_string())), ("n", None)]
            ),
            "https://x.test/a?q=a%20b"
        );
        assert_eq!(encode("a/b c~d"), "a%2Fb%20c~d");
    }

    #[test]
    fn creates_presentation() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"presentationId":"deck1","title":"Q3 Review"}"#)]);

        // Act.
        let answered = call("createPresentation", r#"{"title":"Q3 Review"}"#, &mut stub).unwrap();

        // Assert.
        let payload: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(
            payload,
            json!({
                "presentationId": "deck1",
                "title": "Q3 Review",
                "url": "https://docs.google.com/presentation/d/deck1/edit",
            })
        );
        assert_eq!(stub.seen[0].url, API.to_string());
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(body, json!({ "title": "Q3 Review" }));
    }

    #[test]
    fn create_presentation_reports_problems() {
        // Arrange: the API refuses.
        let mut stub = Stub::of(&[response(403, "insufficient scope")]);

        // Act.
        let refused = call("createPresentation", r#"{"title":"Q3"}"#, &mut stub).unwrap();

        // Assert: a tool error, not a Rust error.
        assert!(refused.structured.is_none());

        // Arrange: the API answers without an ID.
        let mut stub = Stub::of(&[ok_json(r#"{"title":"Q3"}"#)]);

        // Act.
        let missing = call("createPresentation", r#"{"title":"Q3"}"#, &mut stub);

        // Assert.
        assert!(
            matches!(missing, Err(ConnectorError::Upstream(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{missing:?}"
        );

        // Arrange: bad input.
        let mut stub = Stub::of(&[]);

        // Act.
        let bad = call("createPresentation", "{}", &mut stub);

        // Assert: nothing was sent.
        assert!(
            matches!(bad, Err(ConnectorError::InvalidInput(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{bad:?}"
        );
        assert!(stub.seen.is_empty());
    }

    #[test]
    fn gets_presentation() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(PRESENTATION)]);

        // Act.
        let answered = call(
            "getPresentation",
            r#"{"presentation_id":"deck1"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        let payload: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(
            payload,
            json!({
                "presentationId": "deck1",
                "title": "Q3 Review",
                "slideCount": 2,
                "pageSize": { "widthInches": 10.0, "heightInches": 5.625 },
                "slides": [
                    {
                        "objectId": "slide1",
                        "layout": "layout_title_body",
                        "title": "Q3 Review",
                        "bodyPreview": "Revenue grew 12% this quarter.",
                    },
                    { "objectId": "slide2", "layout": "layout_blank" },
                ],
            })
        );
        assert!(
            stub.seen[0]
                .url
                .starts_with(&format!("{API}/deck1?fields="))
        );
        assert!(stub.seen[0].url.contains("pageSize"));
    }

    #[test]
    fn gets_presentation_without_page_size() {
        // Arrange: the API answers without a pageSize (e.g. a narrow field
        // mask); the summary simply omits it.
        let mut stub = Stub::of(&[ok_json(
            r#"{"presentationId":"deck1","title":"Q3 Review","slides":[]}"#,
        )]);

        // Act.
        let answered = call(
            "getPresentation",
            r#"{"presentation_id":"deck1"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        let payload: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert!(payload.get("pageSize").is_none());
        assert_eq!(payload["slideCount"], json!(0));
    }

    #[test]
    fn gets_presentation_with_partial_page_size() {
        // A pageSize without a width, with a non-numeric width, and with a
        // non-numeric height each omit the size summary instead of failing.
        for page_size in [
            r#"{"height":{"magnitude":5143500}}"#,
            r#"{"width":{"magnitude":"wide"},"height":{"magnitude":5143500}}"#,
            r#"{"width":{"magnitude":9144000}}"#,
            r#"{"width":{"magnitude":9144000},"height":{"magnitude":"tall"}}"#,
        ] {
            let mut stub = Stub::of(&[ok_json(&format!(
                r#"{{"presentationId":"deck1","title":"Q3","pageSize":{page_size}}}"#
            ))]);

            let answered = call(
                "getPresentation",
                r#"{"presentation_id":"deck1"}"#,
                &mut stub,
            )
            .unwrap();

            let payload: Value =
                serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
            assert!(payload.get("pageSize").is_none(), "{page_size}");
        }
    }

    #[test]
    fn get_presentation_reports_problems() {
        // Arrange: not found.
        let mut stub = Stub::of(&[response(404, "not found")]);

        // Act.
        let missing = call(
            "getPresentation",
            r#"{"presentation_id":"nope"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: a tool error.
        assert!(missing.structured.is_none());

        // Arrange: garbled JSON.
        let mut stub = Stub::of(&[ok_json("<html>")]);

        // Act.
        let garbled = call(
            "getPresentation",
            r#"{"presentation_id":"deck1"}"#,
            &mut stub,
        );

        // Assert.
        assert!(
            matches!(garbled, Err(ConnectorError::Upstream(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{garbled:?}"
        );
    }

    #[test]
    fn gets_slide() {
        // Arrange: every element kind, with and without text.
        let mut stub = Stub::of(&[ok_json(
            r#"{
                "objectId": "slide1",
                "pageElements": [
                    {
                        "objectId": "title1",
                        "shape": {
                            "shapeType": "TEXT_BOX",
                            "placeholder": { "type": "TITLE" },
                            "text": { "textElements": [
                                { "textRun": { "content": "Hello world" } },
                                { "paragraphMarker": {} }
                            ]}
                        }
                    },
                    { "objectId": "img1", "image": { "sourceUrl": "https://example.com/a.png" } },
                    { "objectId": "tbl1", "table": {} },
                    { "objectId": "vid1", "video": {} },
                    { "objectId": "ln1", "line": {} },
                    { "objectId": "wa1", "wordArt": {} },
                    { "objectId": "ch1", "sheetsChart": {} },
                    { "objectId": "gr1", "group": { "children": [] } },
                    { "objectId": "xx1" }
                ]
            }"#,
        )]);

        // Act.
        let answered = call(
            "getSlide",
            r#"{"presentation_id":"deck1","slide_id":"slide1"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        let payload: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(
            payload,
            json!({
                "objectId": "slide1",
                "elements": [
                    { "objectId": "title1", "type": "shape", "textPreview": "Hello world" },
                    { "objectId": "img1", "type": "image" },
                    { "objectId": "tbl1", "type": "table" },
                    { "objectId": "vid1", "type": "video" },
                    { "objectId": "ln1", "type": "line" },
                    { "objectId": "wa1", "type": "wordArt" },
                    { "objectId": "ch1", "type": "sheetsChart" },
                    { "objectId": "gr1", "type": "group" },
                    { "objectId": "xx1", "type": "unknown" },
                ],
            })
        );
        assert!(
            stub.seen[0]
                .url
                .starts_with(&format!("{API}/deck1/pages/slide1?fields="))
        );
    }

    #[test]
    fn get_slide_reports_missing_slide() {
        // Arrange.
        let mut stub = Stub::of(&[response(404, "not found")]);

        // Act.
        let missing = call(
            "getSlide",
            r#"{"presentation_id":"deck1","slide_id":"nope"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert!(missing.structured.is_none());
    }

    #[test]
    fn adds_slide_without_text() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(
            r#"{"replies":[{"createSlide":{"objectId":"newslide"}}]}"#,
        )]);

        // Act.
        let answered = call(
            "addSlide",
            r#"{"presentation_id":"deck1","layout":"BLANK","index":2}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: one call, the new slide's ID.
        assert_eq!(stub.seen.len(), 1);
        assert_eq!(stub.seen[0].url, format!("{API}/deck1:batchUpdate"));
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            body,
            json!({
                "requests": [{
                    "createSlide": {
                        "slideLayoutReference": { "predefinedLayout": "BLANK" },
                        "insertionIndex": 2,
                    }
                }]
            })
        );
        let payload: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(payload, json!({ "slideId": "newslide" }));
    }

    #[test]
    fn adds_slide_with_text() {
        // Arrange: create, read the new slide's placeholders, write the text.
        let mut stub = Stub::of(&[
            ok_json(r#"{"replies":[{"createSlide":{"objectId":"newslide"}}]}"#),
            ok_json(
                r#"{"pageElements":[
                    {"objectId":"t1","shape":{"placeholder":{"type":"TITLE"}}},
                    {"objectId":"b1","shape":{"placeholder":{"type":"BODY"}}},
                    {"objectId":"img1","image":{}}
                ]}"#,
            ),
            ok_json(r#"{"replies":[{},{},{}]}"#),
        ]);

        // Act.
        let answered = call(
            "addSlide",
            r#"{"presentation_id":"deck1","title":"Kickoff","body":"Agenda:\n- Intros"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: three calls in order.
        assert_eq!(stub.seen.len(), 3);
        let created: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            created["requests"][0]["createSlide"]["slideLayoutReference"],
            json!({ "predefinedLayout": "TITLE_AND_BODY" })
        );
        assert!(
            created["requests"][0]["createSlide"]
                .get("insertionIndex")
                .is_none()
        );
        assert_eq!(
            stub.seen[1].url,
            format!(
                "{API}/deck1/pages/newslide?fields=pageElements%28objectId%2Cshape%28placeholder%28type%29%29%29"
            )
        );
        let written: Value = serde_json::from_slice(&stub.seen[2].body).unwrap();
        assert_eq!(
            written,
            json!({
                "requests": [
                    { "insertText": { "objectId": "t1", "insertionIndex": 0, "text": "Kickoff" } },
                    { "insertText": { "objectId": "b1", "insertionIndex": 0, "text": "Agenda:\n- Intros" } },
                ]
            })
        );
        let payload: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(payload, json!({ "slideId": "newslide" }));
    }

    #[test]
    fn add_slide_accepts_all_predefined_layouts() {
        // Arrange: the two layouts the API defines but the connector used to
        // reject.
        for layout in ["CAPTION_ONLY", "SECTION_TITLE_AND_DESCRIPTION"] {
            let mut stub = Stub::of(&[ok_json(
                r#"{"replies":[{"createSlide":{"objectId":"newslide"}}]}"#,
            )]);

            // Act.
            let answered = call(
                "addSlide",
                &format!(r#"{{"presentation_id":"deck1","layout":"{layout}"}}"#),
                &mut stub,
            )
            .unwrap();

            // Assert: the layout reached the API, no placeholder pass needed.
            assert_eq!(stub.seen.len(), 1);
            let created: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
            assert_eq!(
                created["requests"][0]["createSlide"]["slideLayoutReference"],
                json!({ "predefinedLayout": layout })
            );
            let payload: Value =
                serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
            assert_eq!(payload, json!({ "slideId": "newslide" }));
        }
    }

    #[test]
    fn add_slide_validates_layout() {
        // Arrange.
        let mut stub = Stub::of(&[]);

        // Act.
        let bad = call(
            "addSlide",
            r#"{"presentation_id":"deck1","layout":"FANCY"}"#,
            &mut stub,
        );

        // Assert.
        assert!(
            matches!(bad, Err(ConnectorError::InvalidInput(ref m)) if m.contains("unknown layout")),
            "{bad:?}"
        );
        assert!(stub.seen.is_empty());
    }

    #[test]
    fn add_slide_reports_problems() {
        // Arrange: creating fails.
        let mut stub = Stub::of(&[response(403, "insufficient scope")]);

        // Act.
        let failed = call("addSlide", r#"{"presentation_id":"deck1"}"#, &mut stub).unwrap();

        // Assert: a tool error naming the step.
        assert!(failed.structured.is_none());

        // Arrange: the reply has no slide ID.
        let mut stub = Stub::of(&[ok_json(r#"{"replies":[]}"#)]);

        // Act.
        let missing = call("addSlide", r#"{"presentation_id":"deck1"}"#, &mut stub);

        // Assert.
        assert!(
            matches!(missing, Err(ConnectorError::Upstream(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{missing:?}"
        );

        // Arrange: title requested on a layout without placeholders.
        let mut stub = Stub::of(&[
            ok_json(r#"{"replies":[{"createSlide":{"objectId":"newslide"}}]}"#),
            ok_json(r#"{"pageElements":[]}"#),
        ]);

        // Act.
        let bad = call(
            "addSlide",
            r#"{"presentation_id":"deck1","layout":"BLANK","title":"Hi"}"#,
            &mut stub,
        );

        // Assert.
        assert!(
            matches!(bad, Err(ConnectorError::InvalidInput(ref m)) if m.contains("no title placeholder")),
            "{bad:?}"
        );

        // Arrange: body requested, title placeholder only.
        let mut stub = Stub::of(&[
            ok_json(r#"{"replies":[{"createSlide":{"objectId":"newslide"}}]}"#),
            ok_json(
                r#"{"pageElements":[{"objectId":"t1","shape":{"placeholder":{"type":"TITLE"}}}]}"#,
            ),
        ]);

        // Act.
        let bad = call(
            "addSlide",
            r#"{"presentation_id":"deck1","body":"Hi"}"#,
            &mut stub,
        );

        // Assert.
        assert!(
            matches!(bad, Err(ConnectorError::InvalidInput(ref m)) if m.contains("no body placeholder")),
            "{bad:?}"
        );

        // Arrange: reading the new slide fails.
        let mut stub = Stub::of(&[
            ok_json(r#"{"replies":[{"createSlide":{"objectId":"newslide"}}]}"#),
            response(404, "gone"),
        ]);

        // Act.
        let failed = call(
            "addSlide",
            r#"{"presentation_id":"deck1","title":"Hi"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert!(failed.structured.is_none());

        // Arrange: writing the slide text fails.
        let mut stub = Stub::of(&[
            ok_json(r#"{"replies":[{"createSlide":{"objectId":"newslide"}}]}"#),
            ok_json(
                r#"{"pageElements":[{"objectId":"t1","shape":{"placeholder":{"type":"TITLE"}}}]}"#,
            ),
            response(400, "bad request"),
        ]);

        // Act.
        let failed = call(
            "addSlide",
            r#"{"presentation_id":"deck1","title":"Hi"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert!(failed.structured.is_none());
    }

    #[test]
    fn sets_slide_text() {
        // Arrange: read placeholders, then delete + insert for title and body.
        let mut stub = Stub::of(&[
            ok_json(
                r#"{"pageElements":[
                    {"objectId":"t1","shape":{"placeholder":{"type":"CENTER_TITLE"}}},
                    {"objectId":"b1","shape":{"placeholder":{"type":"TEXT"}}}
                ]}"#,
            ),
            ok_json(r#"{"replies":[{},{},{},{}]}"#),
        ]);

        // Act.
        let answered = call(
            "setSlideText",
            r#"{"presentation_id":"deck1","slide_id":"slide1","title":"New title","body":"New body"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(stub.seen.len(), 2);
        assert!(
            stub.seen[0]
                .url
                .starts_with(&format!("{API}/deck1/pages/slide1?fields="))
        );
        let written: Value = serde_json::from_slice(&stub.seen[1].body).unwrap();
        assert_eq!(
            written,
            json!({
                "requests": [
                    { "deleteText": { "objectId": "t1", "textRange": { "type": "ALL" } } },
                    { "insertText": { "objectId": "t1", "insertionIndex": 0, "text": "New title" } },
                    { "deleteText": { "objectId": "b1", "textRange": { "type": "ALL" } } },
                    { "insertText": { "objectId": "b1", "insertionIndex": 0, "text": "New body" } },
                ]
            })
        );
        let payload: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(payload, json!({ "slideId": "slide1" }));
    }

    #[test]
    fn set_slide_text_reports_problems() {
        // Arrange: neither title nor body.
        let mut stub = Stub::of(&[]);

        // Act.
        let bad = call(
            "setSlideText",
            r#"{"presentation_id":"deck1","slide_id":"slide1"}"#,
            &mut stub,
        );

        // Assert.
        assert!(
            matches!(bad, Err(ConnectorError::InvalidInput(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{bad:?}"
        );
        assert!(stub.seen.is_empty());

        // Arrange: the slide has no title placeholder.
        let mut stub = Stub::of(&[ok_json(
            r#"{"pageElements":[{"objectId":"b1","shape":{"placeholder":{"type":"BODY"}}}]}"#,
        )]);

        // Act.
        let bad = call(
            "setSlideText",
            r#"{"presentation_id":"deck1","slide_id":"slide1","title":"Hi"}"#,
            &mut stub,
        );

        // Assert.
        assert!(
            matches!(bad, Err(ConnectorError::InvalidInput(ref m)) if m.contains("no title placeholder")),
            "{bad:?}"
        );

        // Arrange: the slide has no body placeholder.
        let mut stub = Stub::of(&[ok_json(
            r#"{"pageElements":[{"objectId":"t1","shape":{"placeholder":{"type":"TITLE"}}}]}"#,
        )]);

        // Act.
        let bad = call(
            "setSlideText",
            r#"{"presentation_id":"deck1","slide_id":"slide1","body":"Hi"}"#,
            &mut stub,
        );

        // Assert.
        assert!(
            matches!(bad, Err(ConnectorError::InvalidInput(ref m)) if m.contains("no body placeholder")),
            "{bad:?}"
        );

        // Arrange: the slide cannot be read.
        let mut stub = Stub::of(&[response(404, "not found")]);

        // Act.
        let failed = call(
            "setSlideText",
            r#"{"presentation_id":"deck1","slide_id":"nope","title":"Hi"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert!(failed.structured.is_none());

        // Arrange: writing fails.
        let mut stub = Stub::of(&[
            ok_json(
                r#"{"pageElements":[{"objectId":"b1","shape":{"placeholder":{"type":"BODY"}}}]}"#,
            ),
            response(400, "bad request"),
        ]);

        // Act.
        let failed = call(
            "setSlideText",
            r#"{"presentation_id":"deck1","slide_id":"slide1","body":"Hi"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert!(failed.structured.is_none());
    }

    #[test]
    fn adds_image_with_size_and_position() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(
            r#"{"replies":[{"createImage":{"objectId":"img9"}}]}"#,
        )]);

        // Act.
        let answered = call(
            "addImage",
            r#"{"presentation_id":"deck1","slide_id":"slide1","image_url":"https://example.com/a.png","width_inches":5,"height_inches":3,"x_inches":1.5,"y_inches":2}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: EMU math (914400 EMU per inch) and the raw reply.
        assert_eq!(stub.seen[0].url, format!("{API}/deck1:batchUpdate"));
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            body,
            json!({
                "requests": [{
                    "createImage": {
                        "elementProperties": {
                            "pageObjectId": "slide1",
                            "size": {
                                "width": { "magnitude": 4572000, "unit": "EMU" },
                                "height": { "magnitude": 2743200, "unit": "EMU" },
                            },
                            "transform": {
                                "scaleX": 1,
                                "scaleY": 1,
                                "translateX": { "magnitude": 1371600, "unit": "EMU" },
                                "translateY": { "magnitude": 1828800, "unit": "EMU" },
                                "unit": "EMU",
                            },
                        },
                        "sourceUrl": "https://example.com/a.png",
                    }
                }]
            })
        );
        assert_eq!(
            answered.structured.as_deref(),
            Some(r#"{"replies":[{"createImage":{"objectId":"img9"}}]}"#)
        );
    }

    #[test]
    fn adds_image_with_defaults() {
        // Arrange: only a width; height follows the width so the API fits the
        // image proportionally. Then no size or position at all.
        let mut stub = Stub::of(&[ok_json("{}"), ok_json("{}")]);

        // Act.
        call(
            "addImage",
            r#"{"presentation_id":"deck1","slide_id":"slide1","image_url":"https://example.com/a.png","width_inches":4}"#,
            &mut stub,
        )
        .unwrap();
        call(
            "addImage",
            r#"{"presentation_id":"deck1","slide_id":"slide1","image_url":"https://example.com/a.png"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        let size = &body["requests"][0]["createImage"]["elementProperties"]["size"];
        assert_eq!(size["width"]["magnitude"], json!(3657600));
        assert_eq!(size["height"]["magnitude"], json!(3657600));
        assert!(
            body["requests"][0]["createImage"]["elementProperties"]
                .get("transform")
                .is_none()
        );
        let body: Value = serde_json::from_slice(&stub.seen[1].body).unwrap();
        assert!(
            body["requests"][0]["createImage"]["elementProperties"]
                .get("size")
                .is_none()
        );

        // Act: bad input never reaches the sender.
        let bad = call("addImage", r#"{"presentation_id":"deck1"}"#, &mut stub);
        assert!(
            matches!(bad, Err(ConnectorError::InvalidInput(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{bad:?}"
        );
    }

    #[test]
    fn adds_image_with_height_only() {
        // Arrange: only a height; width follows the height so the API fits
        // the image proportionally.
        let mut stub = Stub::of(&[ok_json("{}")]);

        // Act.
        call(
            "addImage",
            r#"{"presentation_id":"deck1","slide_id":"slide1","image_url":"https://example.com/a.png","height_inches":3}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: both dimensions carry the height.
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        let size = &body["requests"][0]["createImage"]["elementProperties"]["size"];
        assert_eq!(size["width"]["magnitude"], json!(2743200));
        assert_eq!(size["height"]["magnitude"], json!(2743200));
    }

    #[test]
    fn replaces_text() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(
            r#"{"replies":[{"replaceAllText":{"occurrencesChanged":7}}]}"#,
        )]);

        // Act.
        let answered = call(
            "replaceText",
            r#"{"presentation_id":"deck1","text":"Acme","replacement":"Globex","page_ids":["slide1","slide2"]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            body,
            json!({
                "requests": [{
                    "replaceAllText": {
                        "containsText": { "text": "Acme" },
                        "replaceText": "Globex",
                        "pageObjectIds": ["slide1", "slide2"],
                    }
                }]
            })
        );
        let payload: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(payload, json!({ "occurrencesChanged": 7 }));
    }

    #[test]
    fn replace_text_reports_problems() {
        // Arrange: no occurrencesChanged in the reply, and no page limit.
        let mut stub = Stub::of(&[ok_json(r#"{"replies":[{}]}"#)]);

        // Act.
        let answered = call(
            "replaceText",
            r#"{"presentation_id":"deck1","text":"Acme","replacement":"Globex"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: zero changes, no pageObjectIds sent.
        let payload: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(payload, json!({ "occurrencesChanged": 0 }));
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert!(
            body["requests"][0]["replaceAllText"]
                .get("pageObjectIds")
                .is_none()
        );

        // Arrange: the API refuses.
        let mut stub = Stub::of(&[response(400, "bad request")]);

        // Act.
        let failed = call(
            "replaceText",
            r#"{"presentation_id":"deck1","text":"Acme","replacement":"Globex"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert!(failed.structured.is_none());
    }

    #[test]
    fn deletes_slide() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"replies":[{}]}"#)]);

        // Act.
        let answered = call(
            "deleteSlide",
            r#"{"presentation_id":"deck1","slide_id":"slide2"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: a deleteObject on the slide.
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            body,
            json!({ "requests": [{ "deleteObject": { "objectId": "slide2" } }] })
        );
        let payload: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(payload, json!({ "deleted": true, "slideId": "slide2" }));
    }

    #[test]
    fn delete_slide_reports_failure() {
        // Arrange.
        let mut stub = Stub::of(&[response(404, "not found")]);

        // Act.
        let failed = call(
            "deleteSlide",
            r#"{"presentation_id":"deck1","slide_id":"nope"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert!(failed.structured.is_none());
    }

    #[test]
    fn reorders_slides() {
        // Arrange: three slides, asked to reverse them.
        let mut stub = Stub::of(&[
            ok_json(r#"{"slides":[{"objectId":"a"},{"objectId":"b"},{"objectId":"c"}]}"#),
            ok_json(r#"{"replies":[{},{},{}]}"#),
        ]);

        // Act.
        let answered = call(
            "reorderSlides",
            r#"{"presentation_id":"deck1","slide_ids":["c","b","a"]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: each slide moved to the front in reverse order.
        assert_eq!(stub.seen.len(), 2);
        assert!(stub.seen[0].url.ends_with("/deck1?fields=slides.objectId"));
        let body: Value = serde_json::from_slice(&stub.seen[1].body).unwrap();
        assert_eq!(
            body,
            json!({
                "requests": [
                    { "updateSlidesPosition": { "slideObjectIds": ["a"], "insertionIndex": 0 } },
                    { "updateSlidesPosition": { "slideObjectIds": ["b"], "insertionIndex": 0 } },
                    { "updateSlidesPosition": { "slideObjectIds": ["c"], "insertionIndex": 0 } },
                ]
            })
        );
        let payload: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(payload, json!({ "slideIds": ["c", "b", "a"] }));
    }

    #[test]
    fn reorder_slides_skips_already_ordered() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"slides":[{"objectId":"a"},{"objectId":"b"}]}"#)]);

        // Act.
        let answered = call(
            "reorderSlides",
            r#"{"presentation_id":"deck1","slide_ids":["a","b"]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert: no mutation was sent.
        assert_eq!(stub.seen.len(), 1);
        let payload: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(payload, json!({ "slideIds": ["a", "b"] }));
    }

    #[test]
    fn reorder_slides_validates_ids() {
        // Arrange: a missing ID, then a duplicate.
        let mut stub = Stub::of(&[
            ok_json(r#"{"slides":[{"objectId":"a"},{"objectId":"b"}]}"#),
            ok_json(r#"{"slides":[{"objectId":"a"},{"objectId":"b"}]}"#),
        ]);

        // Act.
        let missing = call(
            "reorderSlides",
            r#"{"presentation_id":"deck1","slide_ids":["a","zzz"]}"#,
            &mut stub,
        );
        let duplicate = call(
            "reorderSlides",
            r#"{"presentation_id":"deck1","slide_ids":["a","a"]}"#,
            &mut stub,
        );

        // Assert: both rejected, neither mutated.
        assert!(
            matches!(missing, Err(ConnectorError::InvalidInput(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{missing:?}"
        );
        assert!(
            matches!(duplicate, Err(ConnectorError::InvalidInput(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{duplicate:?}"
        );
        assert_eq!(
            stub.seen
                .iter()
                .filter(|seen| seen.url.contains(":batchUpdate"))
                .count(),
            0
        );

        // Arrange: the order read fails.
        let mut stub = Stub::of(&[response(404, "not found")]);

        // Act.
        let failed = call(
            "reorderSlides",
            r#"{"presentation_id":"deck1","slide_ids":["a"]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert!(failed.structured.is_none());

        // Arrange: the move itself fails.
        let mut stub = Stub::of(&[
            ok_json(r#"{"slides":[{"objectId":"a"},{"objectId":"b"}]}"#),
            response(400, "bad request"),
        ]);

        // Act.
        let failed = call(
            "reorderSlides",
            r#"{"presentation_id":"deck1","slide_ids":["b","a"]}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert!(failed.structured.is_none());
    }

    #[test]
    fn gets_slide_thumbnail() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(
            r#"{"width":1600,"height":900,"contentUrl":"https://example.com/thumb.png"}"#,
        )]);

        // Act: default size is LARGE.
        let answered = call(
            "getSlideThumbnail",
            r#"{"presentation_id":"deck1","slide_id":"slide1"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(
            stub.seen[0].url,
            format!("{API}/deck1/pages/slide1/thumbnail?thumbnailProperties.thumbnailSize=LARGE")
        );
        let payload: Value = serde_json::from_str(answered.structured.as_deref().unwrap()).unwrap();
        assert_eq!(
            payload,
            json!({
                "thumbnailUrl": "https://example.com/thumb.png",
                "width": 1600,
                "height": 900,
            })
        );

        // Arrange/Act: an explicit size.
        let mut stub = Stub::of(&[ok_json(r#"{"contentUrl":"https://example.com/s.png"}"#)]);
        call(
            "getSlideThumbnail",
            r#"{"presentation_id":"deck1","slide_id":"slide1","size":"SMALL"}"#,
            &mut stub,
        )
        .unwrap();
        assert!(stub.seen[0].url.ends_with("thumbnailSize=SMALL"));

        // Arrange: an unknown size.
        let mut stub = Stub::of(&[]);

        // Act.
        let bad = call(
            "getSlideThumbnail",
            r#"{"presentation_id":"deck1","slide_id":"slide1","size":"HUGE"}"#,
            &mut stub,
        );

        // Assert.
        assert!(
            matches!(bad, Err(ConnectorError::InvalidInput(ref m)) if m.contains("unknown size")),
            "{bad:?}"
        );
        assert!(stub.seen.is_empty());

        // Arrange: no thumbnail URL in the answer.
        let mut stub = Stub::of(&[ok_json(r#"{"width":1}"#)]);

        // Act.
        let missing = call(
            "getSlideThumbnail",
            r#"{"presentation_id":"deck1","slide_id":"slide1"}"#,
            &mut stub,
        );

        // Assert.
        assert!(
            matches!(missing, Err(ConnectorError::Upstream(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{missing:?}"
        );

        // Arrange: rendering fails.
        let mut stub = Stub::of(&[response(500, "boom")]);

        // Act.
        let failed = call(
            "getSlideThumbnail",
            r#"{"presentation_id":"deck1","slide_id":"slide1"}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert!(failed.structured.is_none());
    }

    #[test]
    fn applies_raw_batch_update() {
        // Arrange.
        let mut stub = Stub::of(&[ok_json(r#"{"replies":[{}]}"#)]);

        // Act: with a write control.
        let answered = call(
            "batchUpdate",
            r#"{"presentation_id":"deck1","requests":[{"createShape":{"shapeType":"RECTANGLE"}}],"write_control":{"requiredRevisionId":"r1"}}"#,
            &mut stub,
        )
        .unwrap();

        // Assert.
        assert_eq!(stub.seen[0].url, format!("{API}/deck1:batchUpdate"));
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert_eq!(
            body,
            json!({
                "requests": [{ "createShape": { "shapeType": "RECTANGLE" } }],
                "writeControl": { "requiredRevisionId": "r1" },
            })
        );
        assert_eq!(answered.structured.as_deref(), Some(r#"{"replies":[{}]}"#));

        // Arrange/Act: without one.
        let mut stub = Stub::of(&[ok_json("{}")]);
        call(
            "batchUpdate",
            r#"{"presentation_id":"deck1","requests":[]}"#,
            &mut stub,
        )
        .unwrap();
        let body: Value = serde_json::from_slice(&stub.seen[0].body).unwrap();
        assert!(body.get("writeControl").is_none());
    }

    #[test]
    fn refuses_bad_input() {
        // Arrange.
        let mut stub = Stub::of(&[]);

        // Act: a missing presentation_id never reaches the sender.
        let missing = call("getPresentation", "{}", &mut stub);

        // Assert.
        assert!(
            matches!(missing, Err(ConnectorError::InvalidInput(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{missing:?}"
        );
        assert!(stub.seen.is_empty());

        // Act: an unknown tool.
        let unknown = call("nope", "{}", &mut stub);

        // Assert.
        assert_eq!(
            unknown.unwrap_err(),
            ConnectorError::NotFound("unknown tool: nope".into())
        );

        // Every tool parses its input first, so malformed JSON fails before
        // anything is sent.
        for tool in [
            "createPresentation",
            "getPresentation",
            "getSlide",
            "addSlide",
            "setSlideText",
            "addImage",
            "replaceText",
            "deleteSlide",
            "reorderSlides",
            "getSlideThumbnail",
            "batchUpdate",
        ] {
            let mut stub = Stub::of(&[]);
            let bad = call(tool, "{oops", &mut stub);
            assert!(
                matches!(bad, Err(ConnectorError::InvalidInput(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
                "{tool}: {bad:?}"
            );
            assert!(
                stub.seen.is_empty(),
                "{tool} sent a request for malformed input"
            );
        }
    }

    #[test]
    fn maps_responses() {
        let ok = answer(&response(200, r#"{"title":"Q3"}"#)).unwrap();
        assert_eq!(ok.structured.as_deref(), Some(r#"{"title":"Q3"}"#));
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
    fn checks_intermediate_steps() {
        assert!(step_ok(&ok_json("{}"), "reading the slide").is_ok());
        let failed = step_ok(&response(404, "gone"), "reading the slide");
        assert!(failed.is_err());
    }

    #[test]
    fn propagates_sender_errors() {
        // Arrange: the sender itself fails.
        let failed = Gslides::call_with(
            request("getPresentation", r#"{"presentation_id":"deck1"}"#),
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
            "getPresentation",
            r#"{"presentation_id":"deck1"}"#,
            &mut stub,
        );

        // Assert.
        assert_eq!(
            exhausted.unwrap_err(),
            ConnectorError::Upstream("stub sender ran out of responses".into())
        );

        // Every tool's first send surfaces a sender failure.
        for (tool, body) in [
            ("createPresentation", r#"{"title":"Q3"}"#),
            ("getPresentation", r#"{"presentation_id":"deck1"}"#),
            (
                "getSlide",
                r#"{"presentation_id":"deck1","slide_id":"slide1"}"#,
            ),
            ("addSlide", r#"{"presentation_id":"deck1"}"#),
            (
                "setSlideText",
                r#"{"presentation_id":"deck1","slide_id":"slide1","title":"T"}"#,
            ),
            (
                "addImage",
                r#"{"presentation_id":"deck1","slide_id":"slide1","image_url":"https://example.com/a.png"}"#,
            ),
            (
                "replaceText",
                r#"{"presentation_id":"deck1","text":"a","replacement":"b"}"#,
            ),
            (
                "deleteSlide",
                r#"{"presentation_id":"deck1","slide_id":"slide1"}"#,
            ),
            (
                "reorderSlides",
                r#"{"presentation_id":"deck1","slide_ids":["slide1"]}"#,
            ),
            (
                "getSlideThumbnail",
                r#"{"presentation_id":"deck1","slide_id":"slide1"}"#,
            ),
            (
                "batchUpdate",
                r#"{"presentation_id":"deck1","requests":[]}"#,
            ),
        ] {
            let mut stub = Stub::of(&[]);
            let failed = call(tool, body, &mut stub);
            assert_eq!(
                failed.unwrap_err(),
                ConnectorError::Upstream("stub sender ran out of responses".into()),
                "{tool}"
            );
        }
    }

    #[test]
    fn surfaces_garbled_responses() {
        // A 200 with a non-JSON body fails the response parse.
        for (tool, body) in [
            ("createPresentation", r#"{"title":"Q3"}"#),
            (
                "getSlide",
                r#"{"presentation_id":"deck1","slide_id":"slide1"}"#,
            ),
            ("addSlide", r#"{"presentation_id":"deck1"}"#),
            (
                "setSlideText",
                r#"{"presentation_id":"deck1","slide_id":"slide1","title":"T"}"#,
            ),
            (
                "replaceText",
                r#"{"presentation_id":"deck1","text":"a","replacement":"b"}"#,
            ),
            (
                "reorderSlides",
                r#"{"presentation_id":"deck1","slide_ids":["slide1"]}"#,
            ),
            (
                "getSlideThumbnail",
                r#"{"presentation_id":"deck1","slide_id":"slide1"}"#,
            ),
        ] {
            let mut stub = Stub::of(&[response(200, "<html>")]);
            let garbled = call(tool, body, &mut stub);
            assert!(
                matches!(garbled, Err(ConnectorError::Upstream(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
                "{tool}: {garbled:?}"
            );
        }

        // addSlide's second call (reading the new slide) garbled.
        let mut stub = Stub::of(&[
            ok_json(r#"{"replies":[{"createSlide":{"objectId":"newslide"}}]}"#),
            response(200, "<html>"),
        ]);
        let garbled = call(
            "addSlide",
            r#"{"presentation_id":"deck1","title":"Kickoff"}"#,
            &mut stub,
        );
        assert!(
            matches!(garbled, Err(ConnectorError::Upstream(_))), // coverage: exclude — matches! false arm is unreachable in a passing test
            "{garbled:?}"
        );
    }

    #[test]
    fn surfaces_later_send_failures() {
        // addSlide's second call (reading the new slide) failing.
        let mut stub = Stub::of(&[ok_json(
            r#"{"replies":[{"createSlide":{"objectId":"newslide"}}]}"#,
        )]);
        let failed = call(
            "addSlide",
            r#"{"presentation_id":"deck1","title":"Kickoff"}"#,
            &mut stub,
        );
        assert_eq!(
            failed.unwrap_err(),
            ConnectorError::Upstream("stub sender ran out of responses".into())
        );

        // addSlide's third call (writing the text) failing.
        let mut stub = Stub::of(&[
            ok_json(r#"{"replies":[{"createSlide":{"objectId":"newslide"}}]}"#),
            ok_json(
                r#"{"pageElements":[
                    {"objectId":"t1","shape":{"placeholder":{"type":"TITLE"}}},
                    {"objectId":"b1","shape":{"placeholder":{"type":"BODY"}}}
                ]}"#,
            ),
        ]);
        let failed = call(
            "addSlide",
            r#"{"presentation_id":"deck1","title":"Kickoff","body":"Agenda"}"#,
            &mut stub,
        );
        assert_eq!(
            failed.unwrap_err(),
            ConnectorError::Upstream("stub sender ran out of responses".into())
        );

        // setSlideText's second call (writing the text) failing.
        let mut stub = Stub::of(&[ok_json(
            r#"{"pageElements":[
                {"objectId":"t1","shape":{"placeholder":{"type":"TITLE"}}}
            ]}"#,
        )]);
        let failed = call(
            "setSlideText",
            r#"{"presentation_id":"deck1","slide_id":"slide1","title":"New title"}"#,
            &mut stub,
        );
        assert_eq!(
            failed.unwrap_err(),
            ConnectorError::Upstream("stub sender ran out of responses".into())
        );

        // reorderSlides' second call (moving the slides) failing.
        let mut stub = Stub::of(&[ok_json(r#"{"slides":[{"objectId":"a"},{"objectId":"b"}]}"#)]);
        let failed = call(
            "reorderSlides",
            r#"{"presentation_id":"deck1","slide_ids":["b","a"]}"#,
            &mut stub,
        );
        assert_eq!(
            failed.unwrap_err(),
            ConnectorError::Upstream("stub sender ran out of responses".into())
        );
    }

    #[test]
    fn off_wasm_nothing_is_sent() {
        let sent = Gslides::call_tool(request("getPresentation", r#"{"presentation_id":"a"}"#));
        assert!(matches!(&sent, Err(ConnectorError::Upstream(m)) if m.contains("wasm32-wasip2")));
    }

    #[test]
    fn manifest_is_embedded() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["name"], "gslides");
        assert_eq!(manifest["auth"]["schemes"][0]["type"], "oauth2");
        assert_eq!(
            manifest["auth"]["schemes"][0]["scopes"][0],
            "https://www.googleapis.com/auth/presentations"
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
                "createPresentation",
                "getPresentation",
                "getSlide",
                "addSlide",
                "setSlideText",
                "addImage",
                "replaceText",
                "deleteSlide",
                "reorderSlides",
                "getSlideThumbnail",
                "batchUpdate",
            ]
        );
        let add = &manifest["tools"][3];
        assert_eq!(
            add["inputSchema"]["properties"]["layout"]["enum"]
                .as_array()
                .unwrap()
                .len(),
            9
        );
    }
}
