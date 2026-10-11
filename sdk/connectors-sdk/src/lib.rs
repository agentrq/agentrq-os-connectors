// Copyright 2026 Contextual, Inc. https://agentrq.com
// This notice may not be modified or removed.
// SPDX-License-Identifier: AGPL-3.0-only

//! Host-side test double for the private AgentRQ `connectors-sdk`.
//!
//! The real SDK lives on the AgentRQ host and is not public. This crate
//! provides the same API surface the connectors use — `http::{Method,
//! Request, Response}`, `Connector`, `ConnectorError`, `ToolRequest`,
//! `ToolResponse`, `host::config`, `manifest!` and `export_connector!` — so
//! the connectors' unit tests run anywhere with `cargo test`.
//!
//! Differences from the real SDK, by design:
//! - `manifest!` embeds the manifest but does not validate it.
//! - `export_connector!` is a no-op (the real one exports WASM bindings).
//! - `Request::send` always fails: HTTP only happens inside the
//!   `wasm32-wasip2` guest on the host.

use serde::Deserialize;
use serde_json::Value;
use std::fmt;

pub mod host {
    /// Manifest inputs injected by the host. Always `None` off-host, so
    /// connectors fall back to their defaults under `cargo test`.
    pub fn config(_name: &str) -> Option<String> {
        None
    }
}

pub mod http {
    use super::{ConnectorError, Value};
    use serde::Deserialize;

    /// HTTP methods the connectors use.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Method {
        Get,
        Post,
        Put,
        Patch,
        Delete,
    }

    /// An outbound HTTP request. The host adds auth after it leaves the guest.
    #[derive(Debug, Clone)]
    pub struct Request {
        pub method: Method,
        pub url: String,
        pub headers: Vec<(String, String)>,
        pub body: Vec<u8>,
    }

    impl Request {
        pub fn new(method: Method, url: impl Into<String>) -> Self {
            Self {
                method,
                url: url.into(),
                headers: Vec::new(),
                body: Vec::new(),
            }
        }

        pub fn get(url: impl Into<String>) -> Self {
            Self::new(Method::Get, url)
        }

        pub fn post(url: impl Into<String>) -> Self {
            Self::new(Method::Post, url)
        }

        pub fn header(mut self, name: &str, value: &str) -> Self {
            self.headers.push((name.to_string(), value.to_string()));
            self
        }

        pub fn json(mut self, value: &Value) -> Result<Self, ConnectorError> {
            self.body = serde_json::to_vec(value)
                .map_err(|e| ConnectorError::InvalidInput(e.to_string()))?;
            self.headers
                .push(("content-type".to_string(), "application/json".to_string()));
            Ok(self)
        }

        /// Off-host there is no guest to send from; the real SDK performs
        /// the request inside `wasm32-wasip2`.
        pub fn send(&self) -> Result<Response, ConnectorError> {
            Err(ConnectorError::Upstream(
                "HTTP send is only available in the wasm32-wasip2 guest".to_string(),
            ))
        }
    }

    /// An HTTP response.
    #[derive(Debug, Clone)]
    pub struct Response {
        pub status: u16,
        pub headers: Vec<(String, String)>,
        pub body: Vec<u8>,
    }

    impl Response {
        pub fn json<'de, T: Deserialize<'de>>(&'de self) -> Result<T, ConnectorError> {
            serde_json::from_slice(&self.body).map_err(|e| ConnectorError::Upstream(e.to_string()))
        }
    }
}

/// A tool call dispatched to the connector.
#[derive(Debug, Default)]
pub struct ToolRequest {
    pub tool: String,
    pub body: String,
}

impl ToolRequest {
    /// Parses the call's JSON body into the tool's input struct.
    pub fn json<'de, T: Deserialize<'de>>(&'de self) -> Result<T, ConnectorError> {
        serde_json::from_str(&self.body).map_err(|e| ConnectorError::InvalidInput(e.to_string()))
    }
}

/// A tool call's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResponse {
    /// JSON payload; `None` for errors.
    pub structured: Option<String>,
}

impl ToolResponse {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            structured: Some(text.into()),
        }
    }

    pub fn structured(value: &Value) -> Result<Self, ConnectorError> {
        serde_json::to_string(value)
            .map(|structured| Self {
                structured: Some(structured),
            })
            .map_err(|e| ConnectorError::Upstream(e.to_string()))
    }

    pub fn error(message: impl Into<String>) -> Self {
        let _ = message.into();
        Self { structured: None }
    }
}

/// Errors a connector can return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectorError {
    NotFound(String),
    InvalidInput(String),
    Upstream(String),
}

impl fmt::Display for ConnectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(m) | Self::InvalidInput(m) | Self::Upstream(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ConnectorError {}

/// A connector: one `call_tool` entry point. Resources and prompts are
/// omitted; these connectors expose tools only.
pub trait Connector {
    fn call_tool(request: ToolRequest) -> Result<ToolResponse, ConnectorError>;
}

/// Embeds the manifest as `MANIFEST`. The real SDK also validates it.
#[macro_export]
macro_rules! manifest {
    ($path:literal) => {
        pub const MANIFEST: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", $path));
    };
}

/// Exports the WASM component bindings. No-op off-host.
#[macro_export]
macro_rules! export_connector {
    ($t:ty) => {};
}

#[cfg(test)]
mod tests {
    use super::host::config;
    use super::http::{Method, Request, Response};
    use super::{ConnectorError, ToolRequest, ToolResponse};
    use serde_json::{json, Value};

    #[test]
    fn config_is_empty_off_host() {
        assert_eq!(config("api_base"), None);
    }

    #[test]
    fn builds_requests() {
        let get = Request::get("https://a.test/x").header("accept", "application/json");
        assert_eq!(get.method, Method::Get);
        assert_eq!(get.url, "https://a.test/x");
        assert_eq!(
            get.headers,
            vec![("accept".to_string(), "application/json".to_string())]
        );

        let post = Request::post("https://a.test/y")
            .json(&json!({"a": 1}))
            .unwrap();
        assert_eq!(post.method, Method::Post);
        let body: Value = serde_json::from_slice(&post.body).unwrap();
        assert_eq!(body, json!({"a": 1}));
        assert!(post
            .headers
            .contains(&("content-type".to_string(), "application/json".to_string())));

        let put = Request::new(Method::Put, "https://a.test/z");
        assert_eq!(put.method, Method::Put);
        assert_eq!(Request::new(Method::Patch, "u").method, Method::Patch);
        assert_eq!(Request::new(Method::Delete, "u").method, Method::Delete);
    }

    #[test]
    fn send_fails_off_host() {
        let err = Request::get("https://a.test").send().unwrap_err();
        assert!(matches!(err, ConnectorError::Upstream(_)));
        assert!(err.to_string().contains("wasm32-wasip2"));
    }

    #[test]
    fn parses_tool_bodies() {
        let req = ToolRequest {
            tool: "get".into(),
            body: r#"{"a":1}"#.into(),
        };
        let v: Value = req.json().unwrap();
        assert_eq!(v, json!({"a": 1}));

        let bad = ToolRequest {
            tool: "get".into(),
            body: "nope".into(),
        };
        assert!(matches!(
            bad.json::<Value>(),
            Err(ConnectorError::InvalidInput(_))
        ));
    }

    #[test]
    fn tool_responses() {
        let ok = ToolResponse::structured(&json!({"a": 1})).unwrap();
        assert_eq!(ok.structured.as_deref(), Some(r#"{"a":1}"#));

        let text = ToolResponse::text("hi");
        assert_eq!(text.structured.as_deref(), Some("hi"));

        let err = ToolResponse::error("boom");
        assert_eq!(err.structured, None);
        assert_eq!(err, ToolResponse::error("other"));
    }

    #[test]
    fn errors_display() {
        assert_eq!(ConnectorError::NotFound("x".into()).to_string(), "x");
        assert_eq!(ConnectorError::InvalidInput("y".into()).to_string(), "y");
    }

    #[test]
    fn parses_responses() {
        let res = Response {
            status: 200,
            headers: Vec::new(),
            body: br#"{"a":1}"#.to_vec(),
        };
        let v: Value = res.json().unwrap();
        assert_eq!(v, json!({"a": 1}));

        let bad = Response {
            status: 200,
            headers: Vec::new(),
            body: b"<html>".to_vec(),
        };
        assert!(matches!(
            bad.json::<Value>(),
            Err(ConnectorError::Upstream(_))
        ));
    }
}
