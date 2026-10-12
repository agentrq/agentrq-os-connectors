//! Google Contacts connector: contact-shaped tools that finish in one call.
//!
//! This is deliberately not a 1:1 mapping of the People API. Reads are
//! shaped into flat, agent-friendly objects (see [`person_to_contact`]) —
//! the raw `Person` JSON with its metadata noise never leaves the
//! connector. Groups resolve by name or resource name, and write tools do
//! their own etag round-trips instead of pushing choreography onto the
//! agent. One tool may therefore make several HTTP calls; see [`Api`].
//!
//! [`Transport`] is what makes those multi-call tools unit-testable: the
//! guest sends requests over HTTP, while tests answer from canned
//! responses.
//!
//! The host runs the OAuth 2.0 sign-in, refreshes the token and adds
//! `Authorization: Bearer …` after each request leaves the guest: this code
//! never sees it.
//!
//! Not covered: the Workspace directory (`people.listDirectoryPeople`),
//! which needs the extra `directory.readonly` scope.

use connectors_sdk::http::{Method, Request, Response};
use connectors_sdk::{Connector, ConnectorError, ToolRequest, ToolResponse, host};
use serde::Deserialize;
use serde_json::{Value, json};

connectors_sdk::manifest!("manifest.json");

/// Where the API is unless the `api_base` input says otherwise.
pub const DEFAULT_API_BASE: &str = "https://people.googleapis.com/v1";

/// The person fields every full-contact read asks for.
const PERSON_FIELDS: &str = "names,emailAddresses,phoneNumbers,organizations,addresses,notes";
/// The only mask the "other contacts" bucket accepts server-side.
const OTHER_CONTACTS_READ_MASK: &str = "names,emailAddresses,phoneNumbers";

/// The connector.
#[derive(Debug)]
pub struct Gcontacts;

// ---------------------------------------------------------------------------
// Tool inputs. Field names are camelCase on the wire (`rename_all`); contact
// data is plain values — string lists for emails/phones/addresses — and the
// connector maps it onto the Person resource internally.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchContacts {
    query: String,
    page_size: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetContactByEmail {
    email: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContactRef {
    resource_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListContacts {
    page_size: Option<u32>,
    page_token: Option<String>,
    sort_order: Option<String>,
}

/// The writable contact fields, shared by create and update.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContactFields {
    given_name: Option<String>,
    family_name: Option<String>,
    display_name: Option<String>,
    emails: Option<Vec<String>>,
    phones: Option<Vec<String>>,
    organization_name: Option<String>,
    organization_title: Option<String>,
    addresses: Option<Vec<String>>,
    notes: Option<String>,
}

impl ContactFields {
    /// True when no contact data was provided at all.
    fn is_empty(&self) -> bool {
        update_fields(self).is_empty()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewContact {
    #[serde(flatten)]
    fields: ContactFields,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditContact {
    resource_name: String,
    #[serde(flatten)]
    fields: ContactFields,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupRef {
    group: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateGroup {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameGroup {
    group: String,
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteGroup {
    group: String,
    delete_contacts: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModifyMembers {
    group: String,
    add_resource_names: Option<Vec<String>>,
    remove_resource_names: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListOtherContacts {
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
    /// The API's HTTP status, when the failure is an API answer.
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
        Ok(request.send()?)
    }
}

// ---------------------------------------------------------------------------
// The API client
// ---------------------------------------------------------------------------

/// The People API at one base URL. A tool method plans whatever requests
/// its outcome needs and sends them through the transport.
struct Api<T: Transport = Guest> {
    /// `{base}`, the root of every endpoint.
    base: String,
    transport: T,
}

impl<T: Transport> Api<T> {
    fn new(base: &str, transport: T) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            transport,
        }
    }

    /// Sends one request and answers the parsed JSON body. An empty body
    /// becomes `Value::Null`.
    fn send(&self, request: Request) -> Result<Value, ToolError> {
        let request = request.header(
            "user-agent",
            concat!(
                "agentrq-gcontacts-connector/",
                env!("CARGO_PKG_VERSION")
            ),
        );
        let response = self.transport.send(request)?;
        if !(200..300).contains(&response.status) {
            let body = String::from_utf8_lossy(&response.body);
            return Err(ToolError::api(response.status, &body));
        }
        if response.body.is_empty() {
            return Ok(Value::Null);
        }
        Ok(response.json().map_err(ConnectorError::from)?)
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
                .json(body)
                .map_err(ConnectorError::from)?, // coverage: exclude — post_json serializes a serde_json::Value, which cannot fail
        )
    }

    /// `PATCH`es a JSON body.
    fn patch_json(&self, url: &str, body: &Value) -> Result<Value, ToolError> {
        self.send(
            Request::new(Method::Patch, url)
                .header("accept", "application/json")
                .json(body)
                .map_err(ConnectorError::from)?, // coverage: exclude — patch_json serializes a serde_json::Value, which cannot fail
        )
    }

    /// `PUT`s a JSON body.
    fn put_json(&self, url: &str, body: &Value) -> Result<Value, ToolError> {
        self.send(
            Request::new(Method::Put, url)
                .header("accept", "application/json")
                .json(body)
                .map_err(ConnectorError::from)?, // coverage: exclude — put_json serializes a serde_json::Value, which cannot fail
        )
    }

    /// DELETEs a URL.
    fn delete(&self, url: &str) -> Result<Value, ToolError> {
        self.send(Request::new(Method::Delete, url).header("accept", "application/json"))
    }

    /// Fetches one person by resource name.
    fn person(&self, resource_name: &str, person_fields: &str) -> Result<Value, ToolError> {
        self.get(&with_query(
            &format!("{}/{resource_name}", self.base),
            &[("personFields", Some(person_fields.to_string()))],
        ))
    }

    /// The etag a contact update must carry, resolved with an internal read
    /// so the caller never sees it.
    fn contact_etag(&self, resource_name: &str) -> Result<String, ToolError> {
        let person = self.person(resource_name, "names")?;
        person
            .get("metadata")
            .and_then(|metadata| metadata.get("sources"))
            .and_then(Value::as_array)
            .and_then(|sources| sources.first())
            .and_then(|source| source.get("etag"))
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| {
                ToolError::input(format!(
                    "The contact {resource_name:?} did not return an etag; cannot update it safely"
                ))
            })
    }

    /// All contact groups, for resolving a name to a resource name.
    fn list_groups(&self) -> Result<Value, ToolError> {
        self.get(&with_query(
            &format!("{}/contactGroups", self.base),
            &[("pageSize", Some(String::from("1000")))],
        ))
    }

    /// Resolves a contact group given as a resource name or a label name to
    /// its resource name. A name that matches nothing — or several groups —
    /// fails with what the user does have, so the agent can pick one.
    fn resolve_group(&self, group: &str) -> Result<String, ToolError> {
        if group.starts_with("contactGroups/") {
            return Ok(String::from(group));
        }
        let body = self.list_groups()?;
        let mut matching = Vec::new();
        let mut names = Vec::new();
        if let Some(groups) = body.get("contactGroups").and_then(Value::as_array) {
            for item in groups {
                if let Some(name) = item.get("name").and_then(Value::as_str) {
                    names.push(name);
                    if name.eq_ignore_ascii_case(group) {
                        if let Some(resource_name) =
                            item.get("resourceName").and_then(Value::as_str)
                        {
                            matching.push(resource_name);
                        }
                    }
                }
            }
        }
        match matching.as_slice() {
            [resource_name] => Ok(String::from(*resource_name)),
            [] => Err(ToolError::input(format!(
                "No contact group named {group:?}; your groups are: {}",
                names.join(", ")
            ))),
            many => Err(ToolError::input(format!(
                "Several contact groups are named {group:?}; use a resource name: {}",
                many.join(", ")
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Shaping: raw Person/ContactGroup JSON becomes the flat objects agents
// work with. The API's metadata noise never leaves the connector.
// ---------------------------------------------------------------------------

/// The `value` strings of a repeated person field (emails, phones):
/// primary entries first, entries without a value skipped.
fn values_of(entries: Option<&Vec<Value>>) -> Value {
    let mut primary = Vec::new();
    let mut rest = Vec::new();
    if let Some(entries) = entries {
        for entry in entries {
            if let Some(value) = entry
                .get("value")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                if entry
                    .get("metadata")
                    .and_then(|metadata| metadata.get("primary"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    primary.push(value);
                } else {
                    rest.push(value);
                }
            }
        }
    }
    Value::Array(
        primary
            .into_iter()
            .chain(rest)
            .map(Value::from)
            .collect(),
    )
}

/// A contact's addresses as plain strings: the formatted value when the API
/// has one, otherwise the address components joined together.
fn address_strings(person: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(addresses) = person.get("addresses").and_then(Value::as_array) {
        for address in addresses {
            if let Some(formatted) = address
                .get("formattedValue")
                .and_then(Value::as_str)
                .filter(|formatted| !formatted.is_empty())
            {
                out.push(formatted.to_string());
                continue;
            }
            let parts: Vec<&str> = ["streetAddress", "city", "region", "postalCode", "country"]
                .iter()
                .filter_map(|key| address.get(key).and_then(Value::as_str))
                .filter(|part| !part.is_empty())
                .collect();
            if !parts.is_empty() {
                out.push(parts.join(", "));
            }
        }
    }
    out
}

/// A contact's first organization as `{name, title}`, or null when there is
/// nothing to show.
fn organization_of(person: &Value) -> Value {
    let first = person
        .get("organizations")
        .and_then(Value::as_array)
        .and_then(|entries| entries.first());
    let name = first.and_then(|org| org.get("name")).and_then(Value::as_str);
    let title = first
        .and_then(|org| org.get("title"))
        .and_then(Value::as_str);
    match (name, title) {
        (None, None) => Value::Null,
        _ => object(&[
            ("name", name.map(Value::from)),
            ("title", title.map(Value::from)),
        ]),
    }
}

/// Shapes a raw `Person` into the flat contact object agents work with.
fn person_to_contact(person: &Value) -> Value {
    let names = person
        .get("names")
        .and_then(Value::as_array)
        .and_then(|entries| entries.first());
    let name_field = |key: &str| {
        names
            .and_then(|name| name.get(key))
            .and_then(Value::as_str)
    };
    json!({
        "resourceName": person.get("resourceName").and_then(Value::as_str).unwrap_or_default(),
        "displayName": name_field("displayName").unwrap_or_default(),
        "givenName": name_field("givenName").unwrap_or_default(),
        "familyName": name_field("familyName").unwrap_or_default(),
        "emails": values_of(person.get("emailAddresses").and_then(Value::as_array)),
        "phones": values_of(person.get("phoneNumbers").and_then(Value::as_array)),
        "organization": organization_of(person),
        "addresses": address_strings(person),
        "notes": person.get("notes").and_then(Value::as_str).unwrap_or_default(),
    })
}

/// Shapes a raw `ContactGroup` into a flat summary. `members` (resource
/// names) appears only when the API returned it.
fn group_shape(group: &Value) -> Value {
    object(&[
        (
            "resourceName",
            group
                .get("resourceName")
                .and_then(Value::as_str)
                .map(Value::from),
        ),
        (
            "name",
            group.get("name").and_then(Value::as_str).map(Value::from),
        ),
        (
            "groupType",
            group
                .get("groupType")
                .and_then(Value::as_str)
                .map(Value::from),
        ),
        (
            "memberCount",
            group
                .get("memberCount")
                .and_then(Value::as_u64)
                .map(Value::from),
        ),
        ("members", group.get("memberResourceNames").cloned()),
    ])
}

/// The shaped contacts in a connections list answer.
fn listed_contacts(body: &Value) -> Vec<Value> {
    body.get("connections")
        .and_then(Value::as_array)
        .map(|connections| connections.iter().map(person_to_contact).collect())
        .unwrap_or_default()
}

/// The shaped contacts in a search answer.
fn searched_contacts(body: &Value) -> Vec<Value> {
    body.get("results")
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .filter_map(|result| result.get("person"))
                .map(person_to_contact)
                .collect()
        })
        .unwrap_or_default()
}

/// The shaped contacts in an "other contacts" list answer.
fn other_contacts(body: &Value) -> Vec<Value> {
    body.get("otherContacts")
        .and_then(Value::as_array)
        .map(|contacts| contacts.iter().map(person_to_contact).collect())
        .unwrap_or_default()
}

/// The shaped groups in a contactGroups list answer.
fn listed_groups(body: &Value) -> Vec<Value> {
    body.get("contactGroups")
        .and_then(Value::as_array)
        .map(|groups| groups.iter().map(group_shape).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Person request bodies, from the plain contact fields.
// ---------------------------------------------------------------------------

/// One `{"value": …}` entry per string, for emails and phones.
fn value_entries(values: Option<&[String]>) -> Option<Value> {
    values.map(|values| {
        Value::Array(
            values
                .iter()
                .map(|value| json!({"value": value}))
                .collect(),
        )
    })
}

/// The single `names` entry, when any name field was provided. `displayName`
/// is output-only on the API; `unstructuredName` is its writable form.
fn name_entry(fields: &ContactFields) -> Option<Value> {
    if fields.given_name.is_none()
        && fields.family_name.is_none()
        && fields.display_name.is_none()
    {
        return None;
    }
    Some(json!([object(&[
        ("givenName", fields.given_name.as_deref().map(Value::from)),
        (
            "familyName",
            fields.family_name.as_deref().map(Value::from)
        ),
        (
            "unstructuredName",
            fields.display_name.as_deref().map(Value::from)
        ),
    ])]))
}

/// The single `organizations` entry, when any organization field was given.
fn organization_entry(fields: &ContactFields) -> Option<Value> {
    if fields.organization_name.is_none() && fields.organization_title.is_none() {
        return None;
    }
    Some(json!([object(&[
        ("name", fields.organization_name.as_deref().map(Value::from)),
        (
            "title",
            fields.organization_title.as_deref().map(Value::from)
        ),
    ])]))
}

/// One address entry per string, as a formatted value.
fn address_entries(addresses: Option<&[String]>) -> Option<Value> {
    addresses.map(|addresses| {
        Value::Array(
            addresses
                .iter()
                .map(|address| json!({"formattedValue": address}))
                .collect(),
        )
    })
}

/// The `Person` body for a create or an update. Updates also carry the
/// resource name and the internally resolved etag.
fn person_body(fields: &ContactFields, etag: Option<&str>, resource_name: Option<&str>) -> Value {
    object(&[
        ("resourceName", resource_name.map(Value::from)),
        ("etag", etag.map(Value::from)),
        ("names", name_entry(fields)),
        ("emailAddresses", value_entries(fields.emails.as_deref())),
        ("phoneNumbers", value_entries(fields.phones.as_deref())),
        ("organizations", organization_entry(fields)),
        (
            "addresses",
            address_entries(fields.addresses.as_deref()),
        ),
        ("notes", fields.notes.as_deref().map(Value::from)),
    ])
}

/// The `updatePersonFields` mask: the API replaces exactly the fields it
/// lists, so this follows the fields the caller provided.
fn update_fields(fields: &ContactFields) -> Vec<&'static str> {
    let mut out = Vec::new();
    if fields.given_name.is_some()
        || fields.family_name.is_some()
        || fields.display_name.is_some()
    {
        out.push("names");
    }
    if fields.emails.is_some() {
        out.push("emailAddresses");
    }
    if fields.phones.is_some() {
        out.push("phoneNumbers");
    }
    if fields.organization_name.is_some() || fields.organization_title.is_some() {
        out.push("organizations");
    }
    if fields.addresses.is_some() {
        out.push("addresses");
    }
    if fields.notes.is_some() {
        out.push("notes");
    }
    out
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

impl Connector for Gcontacts {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError> {
        let base = host::config("api_base").unwrap_or_else(|| DEFAULT_API_BASE.into());
        let api = Api::new(&base, Guest);
        // A failed tool call is still a tool answer: the agent sees what
        // happened and can react, instead of the call itself erroring.
        run(&api, &request).or_else(|error| Ok(ToolResponse::error(error.to_string())))
    }
}

connectors_sdk::export_connector!(Gcontacts);

/// Runs one tool call: plans whatever requests the outcome needs, sends
/// them, and maps the answers to a tool result.
fn run<T: Transport>(api: &Api<T>, request: &ToolRequest) -> Result<ToolResponse, ToolError> {
    Ok(match request.tool.as_str() {
        "searchContacts" => run_search_contacts(api, request)?,
        "getContactByEmail" => run_get_contact_by_email(api, request)?,
        "getContact" => run_get_contact(api, request)?,
        "listContacts" => run_list_contacts(api, request)?,
        "createContact" => run_create_contact(api, request)?,
        "updateContact" => run_update_contact(api, request)?,
        "deleteContact" => run_delete_contact(api, request)?,
        "listContactGroups" => run_list_contact_groups(api)?,
        "getContactGroup" => run_get_contact_group(api, request)?,
        "createContactGroup" => run_create_contact_group(api, request)?,
        "renameContactGroup" => run_rename_contact_group(api, request)?,
        "deleteContactGroup" => run_delete_contact_group(api, request)?,
        "modifyGroupMembers" => run_modify_group_members(api, request)?,
        "listOtherContacts" => run_list_other_contacts(api, request)?,
        "copyOtherContact" => run_copy_other_contact(api, request)?,
        other => return Err(ToolError::input(format!("no tool {other}"))),
    })
}

fn run_search_contacts<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: SearchContacts = request.json()?;
    let body = api.post_json(
        &format!("{}/people:searchContacts", api.base),
        &object(&[
            ("query", Some(json!(input.query))),
            ("pageSize", input.page_size.map(|size| json!(size))),
            ("readMask", Some(json!(PERSON_FIELDS))),
        ]),
    )?;
    Ok(ToolResponse::structured(&json!({"contacts": searched_contacts(&body)}))?) // coverage: exclude — structured() serializes a serde_json::Value, which cannot fail
}

fn run_get_contact_by_email<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: GetContactByEmail = request.json()?;
    let body = api.post_json(
        &format!("{}/people:searchContacts", api.base),
        &object(&[
            ("query", Some(json!(input.email))),
            ("readMask", Some(json!("names,emailAddresses"))),
        ]),
    )?;
    let mut matching = Vec::new();
    if let Some(results) = body.get("results").and_then(Value::as_array) {
        for result in results {
            if let Some(person) = result.get("person") {
                let uses_email = person
                    .get("emailAddresses")
                    .and_then(Value::as_array)
                    .is_some_and(|emails| {
                        emails
                            .iter()
                            .filter_map(|email| email.get("value"))
                            .filter_map(Value::as_str)
                            .any(|value| value.eq_ignore_ascii_case(&input.email))
                    });
                if uses_email {
                    matching.push(
                        person
                            .get("resourceName")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    );
                }
            }
        }
    }
    match matching.as_slice() {
        [resource_name] => {
            let person = api.person(resource_name, PERSON_FIELDS)?;
            Ok(ToolResponse::structured(&person_to_contact(&person))?) // coverage: exclude — structured() serializes a serde_json::Value, which cannot fail
        }
        [] => Err(ToolError::input(format!(
            "No contact uses the email address {:?}",
            input.email
        ))),
        many => Err(ToolError::input(format!(
            "Several contacts use {:?}; fetch one by resource name: {}",
            input.email,
            many.join(", ")
        ))),
    }
}

fn run_get_contact<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: ContactRef = request.json()?;
    let person = api.person(&input.resource_name, PERSON_FIELDS)?;
    Ok(ToolResponse::structured(&person_to_contact(&person))?) // coverage: exclude — structured() serializes a serde_json::Value, which cannot fail
}

fn run_list_contacts<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: ListContacts = request.json()?;
    if let Some(page_size) = input.page_size {
        if page_size == 0 || page_size > 1000 {
            return Err(ToolError::input("pageSize must be between 1 and 1000"));
        }
    }
    if let Some(sort_order) = input.sort_order.as_deref() {
        match sort_order {
            "LAST_MODIFIED_ASCENDING"
            | "LAST_MODIFIED_DESCENDING"
            | "FIRST_NAME_ASCENDING"
            | "LAST_NAME_ASCENDING" => {}
            _ => {
                return Err(ToolError::input(
                    "sortOrder must be one of LAST_MODIFIED_ASCENDING, LAST_MODIFIED_DESCENDING, FIRST_NAME_ASCENDING, LAST_NAME_ASCENDING",
                ));
            }
        }
    }
    let url = with_query(
        &format!("{}/people/me/connections", api.base),
        &[
            ("personFields", Some(PERSON_FIELDS.to_string())),
            ("pageSize", input.page_size.map(|size| size.to_string())),
            ("pageToken", input.page_token),
            ("sortOrder", input.sort_order),
        ],
    );
    let body = api.get(&url)?;
    Ok(ToolResponse::structured(&object(&[
        ("contacts", Some(json!(listed_contacts(&body)))),
        ("nextPageToken", body.get("nextPageToken").cloned()),
    ]))?) // coverage: exclude — structured() serializes a serde_json::Value, which cannot fail
}

fn run_create_contact<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: NewContact = request.json()?;
    if input.fields.is_empty() {
        return Err(ToolError::input(
            "Nothing to create: give at least one of givenName, familyName, displayName, emails, phones, organizationName, organizationTitle, addresses, notes",
        ));
    }
    let body = api.post_json(
        &with_query(
            &format!("{}/people:createContact", api.base),
            &[("personFields", Some(PERSON_FIELDS.to_string()))],
        ),
        &person_body(&input.fields, None, None),
    )?;
    Ok(ToolResponse::structured(&person_to_contact(&body))?) // coverage: exclude — structured() serializes a serde_json::Value, which cannot fail
}

fn run_update_contact<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: EditContact = request.json()?;
    let fields = update_fields(&input.fields);
    if fields.is_empty() {
        return Err(ToolError::input(
            "Nothing to update: give at least one of givenName, familyName, displayName, emails, phones, organizationName, organizationTitle, addresses, notes",
        ));
    }
    let etag = api.contact_etag(&input.resource_name)?;
    let url = with_query(
        &format!("{}/{}:updateContact", api.base, input.resource_name),
        &[
            ("updatePersonFields", Some(fields.join(","))),
            ("personFields", Some(PERSON_FIELDS.to_string())),
        ],
    );
    let body = api.patch_json(
        &url,
        &person_body(&input.fields, Some(&etag), Some(&input.resource_name)),
    )?;
    Ok(ToolResponse::structured(&person_to_contact(&body))?) // coverage: exclude — structured() serializes a serde_json::Value, which cannot fail
}

fn run_delete_contact<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: ContactRef = request.json()?;
    api.delete(&format!("{}/{}:deleteContact", api.base, input.resource_name))?;
    Ok(ToolResponse::structured(&json!({"deleted": true}))?) // coverage: exclude — structured() serializes a serde_json::Value, which cannot fail
}

fn run_list_contact_groups<T: Transport>(api: &Api<T>) -> Result<ToolResponse, ToolError> {
    let body = api.list_groups()?;
    Ok(ToolResponse::structured(&object(&[(
        "groups",
        Some(json!(listed_groups(&body))),
    )]))?) // coverage: exclude — structured() serializes a serde_json::Value, which cannot fail
}

fn run_get_contact_group<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: GroupRef = request.json()?;
    let resource_name = api.resolve_group(&input.group)?;
    let body = api.get(&with_query(
        &format!("{}/{resource_name}", api.base),
        &[("maxMembers", Some(String::from("1000")))],
    ))?;
    Ok(ToolResponse::structured(&group_shape(&body))?) // coverage: exclude — structured() serializes a serde_json::Value, which cannot fail
}

fn run_create_contact_group<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: CreateGroup = request.json()?;
    let body = api.post_json(
        &format!("{}/contactGroups", api.base),
        &json!({"name": input.name}),
    )?;
    Ok(ToolResponse::structured(&group_shape(&body))?) // coverage: exclude — structured() serializes a serde_json::Value, which cannot fail
}

fn run_rename_contact_group<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: RenameGroup = request.json()?;
    let resource_name = api.resolve_group(&input.group)?;
    let current = api.get(&format!("{}/{resource_name}", api.base))?;
    let etag = current.get("etag").and_then(Value::as_str).unwrap_or_default();
    let body = api.put_json(
        &format!("{}/{resource_name}", api.base),
        &json!({
            "contactGroup": {"resourceName": resource_name, "etag": etag, "name": input.name},
            "updateGroupFields": "name",
        }),
    )?;
    Ok(ToolResponse::structured(&group_shape(&body))?) // coverage: exclude — structured() serializes a serde_json::Value, which cannot fail
}

fn run_delete_contact_group<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: DeleteGroup = request.json()?;
    let resource_name = api.resolve_group(&input.group)?;
    let url = with_query(
        &format!("{}/{resource_name}", api.base),
        &[(
            "deleteContacts",
            input.delete_contacts.map(|delete| delete.to_string()),
        )],
    );
    api.delete(&url)?;
    Ok(ToolResponse::structured(&json!({"deleted": true}))?) // coverage: exclude — structured() serializes a serde_json::Value, which cannot fail
}

fn run_modify_group_members<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: ModifyMembers = request.json()?;
    if input.add_resource_names.is_none() && input.remove_resource_names.is_none() {
        return Err(ToolError::input(
            "Nothing to change: give addResourceNames and/or removeResourceNames",
        ));
    }
    let resource_name = api.resolve_group(&input.group)?;
    api.post_json(
        &format!("{}/{resource_name}/members:modify", api.base),
        &object(&[
            (
                "resourceNamesToAdd",
                input.add_resource_names.map(|names| json!(names)),
            ),
            (
                "resourceNamesToRemove",
                input.remove_resource_names.map(|names| json!(names)),
            ),
        ]),
    )?;
    let group = api.get(&format!("{}/{resource_name}", api.base))?;
    Ok(ToolResponse::structured(&group_shape(&group))?) // coverage: exclude — structured() serializes a serde_json::Value, which cannot fail
}

fn run_list_other_contacts<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: ListOtherContacts = request.json()?;
    let url = with_query(
        &format!("{}/otherContacts", api.base),
        &[
            ("readMask", Some(OTHER_CONTACTS_READ_MASK.to_string())),
            ("pageSize", input.page_size.map(|size| size.to_string())),
            ("pageToken", input.page_token),
        ],
    );
    let body = api.get(&url)?;
    Ok(ToolResponse::structured(&object(&[
        ("contacts", Some(json!(other_contacts(&body)))),
        ("nextPageToken", body.get("nextPageToken").cloned()),
    ]))?) // coverage: exclude — structured() serializes a serde_json::Value, which cannot fail
}

fn run_copy_other_contact<T: Transport>(
    api: &Api<T>,
    request: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let input: ContactRef = request.json()?;
    let body = api.post_json(
        &format!(
            "{}/{}:copyOtherContactToMyContactsGroup",
            api.base, input.resource_name
        ),
        &object(&[("readMask", Some(json!(PERSON_FIELDS)))]),
    )?;
    Ok(ToolResponse::structured(&person_to_contact(&body))?) // coverage: exclude — structured() serializes a serde_json::Value, which cannot fail
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

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

/// Percent-encodes a query value.
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
    use std::cell::RefCell;

    const BASE: &str = "https://people.test/v1";
    const FIELDS: &str = "names,emailAddresses,phoneNumbers,organizations,addresses,notes";
    const ENCODED_FIELDS: &str =
        "names%2CemailAddresses%2CphoneNumbers%2Corganizations%2Caddresses%2Cnotes";

    const GROUPS_URL: &str = "https://people.test/v1/contactGroups?pageSize=1000";
    const GROUPS_BODY: &str = r#"{"contactGroups":[
        {"resourceName":"contactGroups/friends","name":"Friends","groupType":"USER_CONTACT","memberCount":2},
        {"resourceName":"contactGroups/family","name":"Family","groupType":"USER_CONTACT","memberCount":5}
    ]}"#;

    const PERSON: &str = r#"{
        "resourceName": "people/c123",
        "etag": "etag123",
        "names": [{"displayName": "Ada Lovelace", "givenName": "Ada", "familyName": "Lovelace"}],
        "emailAddresses": [
            {"value": "ada@work.example", "metadata": {"primary": false}},
            {"value": "ada@example.com", "metadata": {"primary": true}}
        ],
        "phoneNumbers": [{"value": "+15551234567"}],
        "organizations": [{"name": "Analytical Engines", "title": "Mathematician"}],
        "addresses": [{"formattedValue": "12 St James Square, London"}],
        "notes": "First programmer",
        "metadata": {"sources": [{"etag": "etag123"}]}
    }"#;

    // Keys sort alphabetically: serde_json's Map is a BTreeMap by default.
    const SHAPED: &str = r#"{"addresses":["12 St James Square, London"],"displayName":"Ada Lovelace","emails":["ada@example.com","ada@work.example"],"familyName":"Lovelace","givenName":"Ada","notes":"First programmer","organization":{"name":"Analytical Engines","title":"Mathematician"},"phones":["+15551234567"],"resourceName":"people/c123"}"#;

    /// A canned API answer: the request's method and URL, then the status
    /// and body to answer with.
    type CannedAnswer = ((Method, String), (u16, &'static str));

    /// Canned API answers, keyed by method and URL, plus every request
    /// seen, so tests can assert on planned bodies and headers.
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

    fn groups_answer() -> ((Method, &'static str), u16, &'static str) {
        ((Method::Get, GROUPS_URL), 200, GROUPS_BODY)
    }

    fn search_answer(body: &'static str) -> ((Method, &'static str), u16, &'static str) {
        (
            (Method::Post, "https://people.test/v1/people:searchContacts"),
            200,
            body,
        )
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

    fn last_request(api: &Api<Fake>) -> Request {
        api.transport.seen.borrow().last().cloned().unwrap()
    }

    /// The JSON body of the nth request the fake transport saw.
    fn request_body(api: &Api<Fake>, index: usize) -> Value {
        let seen = api.transport.seen.borrow();
        serde_json::from_slice(&seen[index].body).unwrap()
    }

    fn last_body(api: &Api<Fake>) -> Value {
        let seen = api.transport.seen.borrow();
        serde_json::from_slice(&seen.last().unwrap().body).unwrap()
    }

    fn header(request: &Request, name: &str) -> Option<String> {
        request
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    /// The URL of a `people.get` for tests that can their own URL string.
    fn person_url(resource_name: &str) -> String {
        format!("https://people.test/v1/{resource_name}?personFields={ENCODED_FIELDS}")
    }

    // ------------------------------------------------------------------
    // Shaping
    // ------------------------------------------------------------------

    #[test]
    fn shapes_a_full_person() {
        let person: Value = serde_json::from_str(PERSON).unwrap();
        assert_eq!(person_to_contact(&person).to_string(), SHAPED);
    }

    #[test]
    fn shapes_a_minimal_person() {
        let shaped = person_to_contact(&json!({}));
        assert_eq!(
            shaped.to_string(),
            r#"{"addresses":[],"displayName":"","emails":[],"familyName":"","givenName":"","notes":"","organization":null,"phones":[],"resourceName":""}"#
        );
    }

    #[test]
    fn shapes_person_edge_cases() {
        // Entries without a value are skipped; empty strings are skipped;
        // primary entries come first.
        let shaped = person_to_contact(&json!({
            "resourceName": "people/c1",
            "names": [{"givenName": "Grace"}],
            "emailAddresses": [
                {"metadata": {"primary": true}},
                {"value": ""},
                {"value": "grace@example.com", "metadata": {"primary": true}}
            ],
            "phoneNumbers": [{"value": "+1", "metadata": {}}],
            "organizations": [{"name": "Navy"}],
            "addresses": [
                {"streetAddress": "1 Main St", "city": "", "country": "USA"},
                {"formattedValue": ""},
                {"city": "Nowhere"}
            ],
            "notes": "pioneer"
        }));
        assert_eq!(
            shaped.to_string(),
            r#"{"addresses":["1 Main St, USA","Nowhere"],"displayName":"","emails":["grace@example.com"],"familyName":"","givenName":"Grace","notes":"pioneer","organization":{"name":"Navy"},"phones":["+1"],"resourceName":"people/c1"}"#
        );

        // An organization entry with neither name nor title shows as null.
        let shaped = person_to_contact(&json!({
            "organizations": [{"department": "X"}]
        }));
        assert_eq!(shaped["organization"], Value::Null);

        // An address with no usable parts is dropped.
        let shaped = person_to_contact(&json!({
            "addresses": [{"country": ""}]
        }));
        assert_eq!(shaped["addresses"], json!([]));
    }

    #[test]
    fn shapes_groups() {
        let with_members = group_shape(&json!({
            "resourceName": "contactGroups/friends",
            "name": "Friends",
            "groupType": "USER_CONTACT",
            "memberCount": 2,
            "memberResourceNames": ["people/c1", "people/c2"]
        }));
        assert_eq!(
            with_members.to_string(),
            r#"{"groupType":"USER_CONTACT","memberCount":2,"members":["people/c1","people/c2"],"name":"Friends","resourceName":"contactGroups/friends"}"#
        );

        let bare = group_shape(&json!({"name": "Odd"}));
        assert_eq!(bare.to_string(), r#"{"name":"Odd"}"#);
    }

    // ------------------------------------------------------------------
    // Contacts
    // ------------------------------------------------------------------

    #[test]
    fn searches_contacts() {
        let api = client(&[search_answer(
            r#"{"results": [{"person": {"resourceName": "people/c123", "names": [{"displayName": "Ada"}]}}]}"#,
        )]);
        let response = run_tool(&api, "searchContacts", r#"{"query":"ada","pageSize":5}"#).unwrap();
        assert_eq!(
            structured(&response),
            r#"{"contacts":[{"addresses":[],"displayName":"Ada","emails":[],"familyName":"","givenName":"","notes":"","organization":null,"phones":[],"resourceName":"people/c123"}]}"#
        );
        assert_eq!(
            last_body(&api),
            json!({"query": "ada", "pageSize": 5, "readMask": FIELDS})
        );

        let empty = client(&[search_answer(r#"{"kind":"people#searchResponse"}"#)]);
        let response = run_tool(&empty, "searchContacts", r#"{"query":"nobody"}"#).unwrap();
        assert_eq!(structured(&response), r#"{"contacts":[]}"#);
        assert_eq!(
            last_body(&empty),
            json!({"query": "nobody", "readMask": FIELDS})
        );
    }

    #[test]
    fn gets_contact_by_email() {
        let get_url = person_url("people/c123");
        let api = client(&[
            search_answer(
                r#"{"results": [
                    {"person": {"resourceName": "people/c123", "names": [{"displayName": "Ada"}], "emailAddresses": [{"value": "ADA@example.com"}]}},
                    {"kind": "notAPerson"},
                    {"person": {"resourceName": "people/c9", "names": [{"displayName": "Other"}]}}
                ]}"#,
            ),
            ((Method::Get, get_url.as_str()), 200, PERSON),
        ]);
        let response = run_tool(&api, "getContactByEmail", r#"{"email":"ada@example.com"}"#).unwrap();
        assert_eq!(structured(&response), SHAPED);
        // The search is the first of the two calls; the read mask stays small.
        assert_eq!(
            request_body(&api, 0),
            json!({"query": "ada@example.com", "readMask": "names,emailAddresses"})
        );

        // No contact uses the address — with and without a results key.
        for body in [r#"{"results": []}"#, r#"{"kind":"people#searchResponse"}"#] {
            let lonely = client(&[search_answer(body)]);
            let missing = run_tool(&lonely, "getContactByEmail", r#"{"email":"nobody@example.com"}"#);
            assert!(
                matches!(&missing, Err(error) if error.message.contains(r#"No contact uses the email address "nobody@example.com""#)),
                "{body}: {missing:?}"
            );
        }

        // Several contacts share the address: the agent picks one.
        let crowded = client(&[search_answer(
            r#"{"results": [
                {"person": {"resourceName": "people/c1", "emailAddresses": [{"value": "dupe@example.com"}]}},
                {"person": {"resourceName": "people/c2", "emailAddresses": [{"value": "dupe@example.com"}]}}
            ]}"#,
        )]);
        let dupe = run_tool(&crowded, "getContactByEmail", r#"{"email":"dupe@example.com"}"#);
        assert!(
            matches!(&dupe, Err(error) if error.message.contains("Several contacts use")
                && error.message.contains("people/c1, people/c2")),
            "{dupe:?}"
        );
    }

    #[test]
    fn gets_a_contact() {
        let get_url = person_url("people/c123");
        let api = client(&[((Method::Get, get_url.as_str()), 200, PERSON)]);
        let response = run_tool(&api, "getContact", r#"{"resourceName":"people/c123"}"#).unwrap();
        assert_eq!(structured(&response), SHAPED);
    }

    #[test]
    fn lists_contacts() {
        let api = client(&[
            (
                (
                    Method::Get,
                    "https://people.test/v1/people/me/connections?personFields=names%2CemailAddresses%2CphoneNumbers%2Corganizations%2Caddresses%2Cnotes",
                ),
                200,
                r#"{"connections": [{"resourceName": "people/c123", "names": [{"displayName": "Ada"}]}], "nextPageToken": "tok"}"#,
            ),
            (
                (
                    Method::Get,
                    "https://people.test/v1/people/me/connections?personFields=names%2CemailAddresses%2CphoneNumbers%2Corganizations%2Caddresses%2Cnotes&pageSize=50&pageToken=a%20b&sortOrder=LAST_NAME_ASCENDING",
                ),
                200,
                r#"{"connections": []}"#,
            ),
        ]);

        let first = run_tool(&api, "listContacts", "{}").unwrap();
        assert!(structured(&first).contains(r#""nextPageToken":"tok""#));
        assert!(structured(&first).contains(r#""displayName":"Ada""#));

        let paged = run_tool(
            &api,
            "listContacts",
            r#"{"pageSize":50,"pageToken":"a b","sortOrder":"LAST_NAME_ASCENDING"}"#,
        )
        .unwrap();
        assert_eq!(structured(&paged), r#"{"contacts":[]}"#);
    }

    #[test]
    fn rejects_bad_list_parameters() {
        let api = client(&[]);
        for body in [
            r#"{"pageSize":0}"#,
            r#"{"pageSize":1001}"#,
            r#"{"sortOrder":"WHATEVER"}"#,
        ] {
            let bad = run_tool(&api, "listContacts", body);
            assert!(matches!(&bad, Err(_)), "{body}");
            assert!(api.transport.seen.borrow().is_empty(), "{body}");
        }
    }

    #[test]
    fn creates_contacts() {
        let api = client(&[(
            (
                Method::Post,
                "https://people.test/v1/people:createContact?personFields=names%2CemailAddresses%2CphoneNumbers%2Corganizations%2Caddresses%2Cnotes",
            ),
            200,
            PERSON,
        )]);

        let full = run_tool(
            &api,
            "createContact",
            r#"{
                "givenName": "Ada", "familyName": "Lovelace", "displayName": "Ada Lovelace",
                "emails": ["ada@example.com"], "phones": ["+15551234567"],
                "organizationName": "Analytical Engines", "organizationTitle": "Mathematician",
                "addresses": ["12 St James Square, London"], "notes": "First programmer"
            }"#,
        )
        .unwrap();
        assert_eq!(structured(&full), SHAPED);
        assert_eq!(
            request_body(&api, 0),
            json!({
                "names": [{"givenName": "Ada", "familyName": "Lovelace", "unstructuredName": "Ada Lovelace"}],
                "emailAddresses": [{"value": "ada@example.com"}],
                "phoneNumbers": [{"value": "+15551234567"}],
                "organizations": [{"name": "Analytical Engines", "title": "Mathematician"}],
                "addresses": [{"formattedValue": "12 St James Square, London"}],
                "notes": "First programmer",
            })
        );

        // A partial create only sends the fields given.
        let partial = run_tool(
            &api,
            "createContact",
            r#"{"displayName":"Grace Hopper","organizationTitle":"Admiral"}"#,
        )
        .unwrap();
        assert!(structured(&partial).contains("Grace Hopper"));
        assert_eq!(
            request_body(&api, 1),
            json!({
                "names": [{"unstructuredName": "Grace Hopper"}],
                "organizations": [{"title": "Admiral"}],
            })
        );

        // No name fields at all: no names entry is sent.
        let nameless = run_tool(&api, "createContact", r#"{"emails":["x@y.z"]}"#).unwrap();
        assert!(structured(&nameless).contains("Ada Lovelace"));
        let planned = request_body(&api, 2);
        assert_eq!(planned.get("names"), None);
        assert_eq!(
            planned["emailAddresses"],
            json!([{"value": "x@y.z"}])
        );

        let empty = run_tool(&api, "createContact", "{}");
        assert!(
            matches!(&empty, Err(error) if error.message.contains("Nothing to create")),
            "{empty:?}"
        );
    }

    #[test]
    fn updates_a_contact() {
        let get_url = person_url("people/c123");
        let api = client(&[
            (
                (Method::Get, get_url.as_str()),
                200,
                r#"{"resourceName":"people/c123","metadata":{"sources":[{"etag":"etag123"}]}}"#,
            ),
            (
                (
                    Method::Patch,
                    "https://people.test/v1/people/c123:updateContact?updatePersonFields=names%2CemailAddresses%2CphoneNumbers%2Corganizations%2Caddresses%2Cnotes&personFields=names%2CemailAddresses%2CphoneNumbers%2Corganizations%2Caddresses%2Cnotes",
                ),
                200,
                PERSON,
            ),
        ]);
        let response = run_tool(
            &api,
            "updateContact",
            r#"{
                "resourceName": "people/c123",
                "givenName": "Ada",
                "emails": ["ada@example.com"], "phones": ["+15551234567"],
                "organizationName": "Analytical Engines",
                "addresses": ["12 St James Square, London"],
                "notes": "First programmer"
            }"#,
        )
        .unwrap();
        assert_eq!(structured(&response), SHAPED);
        let planned = last_body(&api);
        assert_eq!(planned["etag"], json!("etag123"));
        assert_eq!(planned["resourceName"], json!("people/c123"));
        assert_eq!(planned["names"], json!([{"givenName": "Ada"}]));
        assert_eq!(
            planned["emailAddresses"],
            json!([{"value": "ada@example.com"}])
        );
    }

    #[test]
    fn refuses_empty_updates_and_missing_etags() {
        let api = client(&[]);
        let empty = run_tool(&api, "updateContact", r#"{"resourceName":"people/c123"}"#);
        assert!(
            matches!(&empty, Err(error) if error.message.contains("Nothing to update")),
            "{empty:?}"
        );

        let get_url = person_url("people/c123");
        let no_etag = client(&[(
            (Method::Get, get_url.as_str()),
            200,
            r#"{"resourceName":"people/c123"}"#,
        )]);
        let missing = run_tool(
            &no_etag,
            "updateContact",
            r#"{"resourceName":"people/c123","notes":"x"}"#,
        );
        assert!(
            matches!(&missing, Err(error) if error.message.contains("did not return an etag")),
            "{missing:?}"
        );
    }

    #[test]
    fn deletes_a_contact() {
        let api = client(&[(
            (
                Method::Delete,
                "https://people.test/v1/people/c123:deleteContact",
            ),
            200,
            "",
        )]);
        let response = run_tool(&api, "deleteContact", r#"{"resourceName":"people/c123"}"#).unwrap();
        assert_eq!(structured(&response), r#"{"deleted":true}"#);
    }

    // ------------------------------------------------------------------
    // Groups
    // ------------------------------------------------------------------

    #[test]
    fn lists_contact_groups() {
        let api = client(&[groups_answer()]);
        let response = run_tool(&api, "listContactGroups", "{}").unwrap();
        assert_eq!(
            structured(&response),
            r#"{"groups":[{"groupType":"USER_CONTACT","memberCount":2,"name":"Friends","resourceName":"contactGroups/friends"},{"groupType":"USER_CONTACT","memberCount":5,"name":"Family","resourceName":"contactGroups/family"}]}"#
        );
    }

    #[test]
    fn gets_a_contact_group() {
        let api = client(&[
            groups_answer(),
            (
                (
                    Method::Get,
                    "https://people.test/v1/contactGroups/friends?maxMembers=1000",
                ),
                200,
                r#"{"resourceName":"contactGroups/friends","name":"Friends","groupType":"USER_CONTACT","memberCount":2,"memberResourceNames":["people/c1","people/c2"]}"#,
            ),
            (
                (
                    Method::Get,
                    "https://people.test/v1/contactGroups/family?maxMembers=1000",
                ),
                200,
                r#"{"resourceName":"contactGroups/family","name":"Family","memberCount":5}"#,
            ),
        ]);

        // By name: one list call resolves it.
        let by_name = run_tool(&api, "getContactGroup", r#"{"group":"friends"}"#).unwrap();
        assert!(structured(&by_name).contains(r#""members":["people/c1","people/c2"]"#));

        // By resource name: no list call needed.
        let by_id = run_tool(
            &api,
            "getContactGroup",
            r#"{"group":"contactGroups/family"}"#,
        )
        .unwrap();
        assert_eq!(
            structured(&by_id),
            r#"{"memberCount":5,"name":"Family","resourceName":"contactGroups/family"}"#
        );
    }

    #[test]
    fn reports_unknown_or_ambiguous_groups() {
        let api = client(&[groups_answer()]);
        let missing = run_tool(&api, "getContactGroup", r#"{"group":"Nope"}"#);
        assert!(
            matches!(&missing, Err(error) if error.message.contains(r#"No contact group named "Nope""#)
                && error.message.contains("Friends, Family")),
            "{missing:?}"
        );

        let dupe = client(&[(
            (Method::Get, GROUPS_URL),
            200,
            r#"{"contactGroups":[
                {"resourceName":"contactGroups/a","name":"Club"},
                {"resourceName":"contactGroups/b","name":"club"},
                {"name":"Club"},
                {"resourceName":"contactGroups/d"}
            ]}"#,
        )]);
        let ambiguous = run_tool(&dupe, "getContactGroup", r#"{"group":"CLUB"}"#);
        assert!(
            matches!(&ambiguous, Err(error) if error.message.contains("Several contact groups are named")
                && error.message.contains("contactGroups/a, contactGroups/b")),
            "{ambiguous:?}"
        );

        let empty = client(&[((Method::Get, GROUPS_URL), 200, "{}")]);
        let none = run_tool(&empty, "getContactGroup", r#"{"group":"Nope"}"#);
        assert!(
            matches!(&none, Err(error) if error.message == r#"No contact group named "Nope"; your groups are: "#),
            "{none:?}"
        );
    }

    #[test]
    fn creates_renames_and_deletes_groups() {
        let api = client(&[
            groups_answer(),
            (
                (Method::Post, "https://people.test/v1/contactGroups"),
                200,
                r#"{"resourceName":"contactGroups/new","name":"New","groupType":"USER_CONTACT","memberCount":0}"#,
            ),
            (
                (
                    Method::Get,
                    "https://people.test/v1/contactGroups/friends",
                ),
                200,
                r#"{"resourceName":"contactGroups/friends","name":"Friends","etag":"getag"}"#,
            ),
            (
                (Method::Put, "https://people.test/v1/contactGroups/friends"),
                200,
                r#"{"resourceName":"contactGroups/friends","name":"Pals","memberCount":2}"#,
            ),
            (
                (
                    Method::Delete,
                    "https://people.test/v1/contactGroups/family?deleteContacts=true",
                ),
                200,
                "",
            ),
            (
                (Method::Delete, "https://people.test/v1/contactGroups/friends"),
                200,
                "",
            ),
        ]);

        let created = run_tool(&api, "createContactGroup", r#"{"name":"New"}"#).unwrap();
        assert_eq!(
            structured(&created),
            r#"{"groupType":"USER_CONTACT","memberCount":0,"name":"New","resourceName":"contactGroups/new"}"#
        );
        assert_eq!(request_body(&api, 1), json!({"name": "New"}));

        let renamed = run_tool(
            &api,
            "renameContactGroup",
            r#"{"group":"Friends","name":"Pals"}"#,
        )
        .unwrap();
        assert_eq!(
            structured(&renamed),
            r#"{"memberCount":2,"name":"Pals","resourceName":"contactGroups/friends"}"#
        );
        assert_eq!(
            last_body(&api),
            json!({
                "contactGroup": {"resourceName": "contactGroups/friends", "etag": "getag", "name": "Pals"},
                "updateGroupFields": "name",
            })
        );

        let deleted = run_tool(
            &api,
            "deleteContactGroup",
            r#"{"group":"contactGroups/family","deleteContacts":true}"#,
        )
        .unwrap();
        assert_eq!(structured(&deleted), r#"{"deleted":true}"#);

        // Without deleteContacts the query parameter is omitted.
        let kept = run_tool(
            &api,
            "deleteContactGroup",
            r#"{"group":"contactGroups/friends"}"#,
        )
        .unwrap();
        assert_eq!(structured(&kept), r#"{"deleted":true}"#);
    }

    fn modify_client() -> Api<Fake> {
        client(&[
            groups_answer(),
            (
                (
                    Method::Post,
                    "https://people.test/v1/contactGroups/friends/members:modify",
                ),
                200,
                "{}",
            ),
            (
                (Method::Get, "https://people.test/v1/contactGroups/friends"),
                200,
                r#"{"resourceName":"contactGroups/friends","name":"Friends","memberCount":3}"#,
            ),
        ])
    }

    #[test]
    fn modifies_group_members() {
        let api = modify_client();
        let added = run_tool(
            &api,
            "modifyGroupMembers",
            r#"{"group":"Friends","addResourceNames":["people/c1"]}"#,
        )
        .unwrap();
        assert_eq!(
            structured(&added),
            r#"{"memberCount":3,"name":"Friends","resourceName":"contactGroups/friends"}"#
        );
        assert_eq!(
            request_body(&api, 1),
            json!({"resourceNamesToAdd": ["people/c1"]})
        );

        let api = modify_client();
        let removed = run_tool(
            &api,
            "modifyGroupMembers",
            r#"{"group":"Friends","removeResourceNames":["people/c2"]}"#,
        )
        .unwrap();
        assert!(structured(&removed).contains(r#""memberCount":3"#));
        assert_eq!(
            request_body(&api, 1),
            json!({"resourceNamesToRemove": ["people/c2"]})
        );

        let api = modify_client();
        let both = run_tool(
            &api,
            "modifyGroupMembers",
            r#"{"group":"Friends","addResourceNames":["people/c1"],"removeResourceNames":["people/c2"]}"#,
        )
        .unwrap();
        assert!(structured(&both).contains(r#""memberCount":3"#));
        assert_eq!(
            request_body(&api, 1),
            json!({"resourceNamesToAdd": ["people/c1"], "resourceNamesToRemove": ["people/c2"]})
        );

        let api = modify_client();
        let neither = run_tool(&api, "modifyGroupMembers", r#"{"group":"Friends"}"#);
        assert!(
            matches!(&neither, Err(error) if error.message.contains("Nothing to change")),
            "{neither:?}"
        );
    }

    // ------------------------------------------------------------------
    // Other contacts
    // ------------------------------------------------------------------

    #[test]
    fn lists_other_contacts() {
        let api = client(&[(
            (
                Method::Get,
                "https://people.test/v1/otherContacts?readMask=names%2CemailAddresses%2CphoneNumbers&pageSize=25",
            ),
            200,
            r#"{"otherContacts": [{"resourceName": "otherContacts/c9", "names": [{"displayName": "Nora"}], "emailAddresses": [{"value": "nora@example.com"}]}], "nextPageToken": "tok"}"#,
        )]);
        let response = run_tool(&api, "listOtherContacts", r#"{"pageSize":25}"#).unwrap();
        assert_eq!(
            structured(&response),
            r#"{"contacts":[{"addresses":[],"displayName":"Nora","emails":["nora@example.com"],"familyName":"","givenName":"","notes":"","organization":null,"phones":[],"resourceName":"otherContacts/c9"}],"nextPageToken":"tok"}"#
        );
    }

    #[test]
    fn copies_an_other_contact() {
        let api = client(&[(
            (
                Method::Post,
                "https://people.test/v1/otherContacts/c9:copyOtherContactToMyContactsGroup",
            ),
            200,
            PERSON,
        )]);
        let response = run_tool(
            &api,
            "copyOtherContact",
            r#"{"resourceName":"otherContacts/c9"}"#,
        )
        .unwrap();
        assert_eq!(structured(&response), SHAPED);
        assert_eq!(last_body(&api), json!({"readMask": FIELDS}));
    }

    // ------------------------------------------------------------------
    // Failure plumbing
    // ------------------------------------------------------------------

    #[test]
    fn surfaces_transport_and_api_failures() {
        let failing = Api::new(BASE, Fake::failing());
        let failed = run(&failing, &request("listContacts", "{}"));
        assert!(
            matches!(&failed, Err(error) if error.message == "transport boom" && error.status.is_none()),
            "{failed:?}"
        );

        let garbled = client(&[(
            (
                Method::Get,
                "https://people.test/v1/people/me/connections?personFields=names%2CemailAddresses%2CphoneNumbers%2Corganizations%2Caddresses%2Cnotes",
            ),
            200,
            "<html>",
        )]);
        let parsed = run(&garbled, &request("listContacts", "{}"));
        assert!(
            matches!(&parsed, Err(error) if error.status.is_none()),
            "{parsed:?}"
        );

        let uncanned = client(&[]);
        let missing = uncanned.get("https://people.test/v1/nope");
        assert!(
            matches!(&missing, Err(error) if error.message.contains("no canned answer")),
            "{missing:?}"
        );

        let denied = client(&[(
            (
                Method::Post,
                "https://people.test/v1/people:searchContacts",
            ),
            403,
            "forbidden",
        )]);
        let forbidden = run_tool(&denied, "searchContacts", r#"{"query":"x"}"#);
        assert!(
            matches!(&forbidden, Err(error) if error.message == "The API answered 403: forbidden"),
            "{forbidden:?}"
        );
    }

    #[test]
    fn surfaces_failing_sends() {
        // When the transport itself fails, each tool's first send surfaces
        // the failure instead of panicking on an uncanned answer.
        let cases = [
            ("searchContacts", r#"{"query":"x"}"#),
            ("getContactByEmail", r#"{"email":"x@y.z"}"#),
            ("getContact", r#"{"resourceName":"people/c1"}"#),
            ("listContacts", "{}"),
            ("createContact", r#"{"givenName":"x"}"#),
            ("updateContact", r#"{"resourceName":"people/c1","notes":"x"}"#),
            ("deleteContact", r#"{"resourceName":"people/c1"}"#),
            ("listContactGroups", "{}"),
            ("getContactGroup", r#"{"group":"contactGroups/a"}"#),
            ("createContactGroup", r#"{"name":"x"}"#),
            ("renameContactGroup", r#"{"group":"contactGroups/a","name":"y"}"#),
            ("deleteContactGroup", r#"{"group":"contactGroups/a"}"#),
            (
                "modifyGroupMembers",
                r#"{"group":"contactGroups/a","addResourceNames":["people/c1"]}"#,
            ),
            ("listOtherContacts", "{}"),
            ("copyOtherContact", r#"{"resourceName":"otherContacts/c1"}"#),
        ];
        for (tool, body) in cases {
            let api = Api::new(BASE, Fake::failing());
            let result = run_tool(&api, tool, body);
            assert!(
                matches!(&result, Err(error) if error.message == "transport boom"),
                "{tool}"
            );
        }
    }

    #[test]
    fn refuses_bad_input() {
        let api = client(&[]);
        let malformed = run(&api, &request("createContact", "{oops"));
        assert!(
            matches!(&malformed, Err(error) if error.status.is_none()),
            "{malformed:?}"
        );

        let missing = run(&api, &request("createContact", "{}"));
        assert!(
            matches!(&missing, Err(error) if error.message.contains("Nothing to create")),
            "{missing:?}"
        );

        let unknown = run(&api, &request("nope", "{}"));
        assert!(
            matches!(&unknown, Err(error) if error.message == "no tool nope"),
            "{unknown:?}"
        );
    }

    #[test]
    fn rejects_malformed_input_for_every_tool() {
        // Every tool parses its input first: malformed JSON fails before
        // anything is sent.
        for tool in [
            "searchContacts",
            "getContactByEmail",
            "getContact",
            "listContacts",
            "createContact",
            "updateContact",
            "deleteContact",
            "listContactGroups",
            "getContactGroup",
            "createContactGroup",
            "renameContactGroup",
            "deleteContactGroup",
            "modifyGroupMembers",
            "listOtherContacts",
            "copyOtherContact",
        ] {
            let api = client(&[]);
            let result = run_tool(&api, tool, "{oops");
            assert!(
                matches!(&result, Err(error) if error.status.is_none()),
                "{tool}"
            );
            assert!(api.transport.seen.borrow().is_empty(), "{tool}");
        }
    }

    #[test]
    fn maps_tool_failures_through_call_tool() {
        let unknown = Gcontacts::call_tool(request("nope", "{}")).unwrap();
        assert_eq!(unknown.structured, None);
    }

    #[test]
    fn off_wasm_nothing_is_sent() {
        // Failures surface as tool errors, not call errors.
        let sent = Gcontacts::call_tool(request("listContacts", "{}")).unwrap();
        assert_eq!(sent.structured, None);

        let guest = Guest.send(Request::get("https://people.test/"));
        assert!(matches!(&guest, Err(ConnectorError::Upstream(m)) if m.contains("wasm32-wasip2")));
    }

    #[test]
    fn trims_a_trailing_slash_off_the_base() {
        let api = Api::new("https://people.test/v1/", Fake::answering(&[]));
        assert_eq!(api.base, BASE);
    }

    #[test]
    fn sends_a_user_agent() {
        let api = client(&[(
            (
                Method::Get,
                "https://people.test/v1/people/me/connections?personFields=names%2CemailAddresses%2CphoneNumbers%2Corganizations%2Caddresses%2Cnotes",
            ),
            200,
            r#"{"connections": []}"#,
        )]);
        run_tool(&api, "listContacts", "{}").unwrap();
        let agent = header(&last_request(&api), "user-agent").unwrap();
        assert!(
            agent.starts_with("agentrq-gcontacts-connector/"),
            "unexpected user-agent {agent}"
        );
    }

    #[test]
    fn manifest_is_embedded() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["name"], "gcontacts");
        assert_eq!(manifest["auth"]["schemes"][0]["type"], "oauth2");
        assert_eq!(
            manifest["auth"]["schemes"][0]["scopes"][0],
            "https://www.googleapis.com/auth/contacts"
        );
        assert_eq!(
            manifest["permissions"]["http"]["allow"][0],
            "https://people.googleapis.com"
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
                "searchContacts",
                "getContactByEmail",
                "getContact",
                "listContacts",
                "createContact",
                "updateContact",
                "deleteContact",
                "listContactGroups",
                "getContactGroup",
                "createContactGroup",
                "renameContactGroup",
                "deleteContactGroup",
                "modifyGroupMembers",
                "listOtherContacts",
                "copyOtherContact",
            ]
        );
    }
}
