# gdocs — Google Docs connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the Google Docs API v1. The tools are shaped around what an agent
wants to accomplish — read a document's text, create a document with content,
apply typed edits — rather than the API's endpoints. A tool may make several
HTTP calls to finish its job; the agent never sees the choreography.

![icon](icon.svg)

## Tools

| Tool | What it does | Calls |
|---|---|---|
| `readDocument` | Read a document as plain text: title plus the text of its paragraphs, lists (kept as indented `- ` items) and tables (tab-separated cells) | one `documents.get`, asking only for the title and the structural content |
| `createDocument` | Create a document with a title, optionally with initial text | `documents.create`, then one `batchUpdate` inserting the text at the body's start when `text` is given |
| `editDocument` | Apply typed edits — `append`, `insert`, `replace`, `deleteRange` — in one batch | one `documents.get` to resolve the append position (only when an edit appends), then one `batchUpdate` |

`editDocument` indices are UTF-16 code-unit offsets, as the Docs API counts them.
Appends share the resolved end position, advancing past each appended text so
several appends land in order. An empty `edits` array is a no-op returning
`appliedEdits: 0`.

## What changed in 0.2.0

The 0.1.0 tools mirrored the API; 0.2.0 replaces them with task-shaped tools:

| Before (0.1.0) | After (0.2.0) | Rationale |
|---|---|---|
| `getDocument` returned the raw API structure (body elements, styles, tabs) | `readDocument` returns `{documentId, title, text}` | Agents want to *read* the document, not interpret the API's structural model. The connector walks paragraphs, lists and tables into plain text, and asks the API only for the title and content so formatting never costs tokens. |
| `createDocument` created an empty document; adding content needed a second tool call | `createDocument` takes optional `text` and writes it in the same call | "Create a doc with this content" completes in one call: `documents.create` followed by one `insertText` batchUpdate. |
| `batchUpdate` took raw API request objects (`insertText`, `deleteContentRange`, …) | `editDocument` takes typed edits (`append`/`insert`/`replace`/`deleteRange`) | Agents describe intent instead of hand-building API request shapes. The connector translates the typed ops into one `batchUpdate` call, resolving the append position itself. |

## Auth

OAuth 2.0 (authorization code + PKCE), scope
`https://www.googleapis.com/auth/documents`. The host runs the sign-in and
injects the bearer token; the connector code never sees it. Configure the
`client_id` / `client_secret` inputs and the `api_base` input
(default `https://docs.googleapis.com`).

## Build & test

Built with the AgentRQ `wasmrs` connector toolchain (`oauth2` template):

```sh
cargo test          # unit tests (100% line and function coverage required)
```

`sdk/connectors-sdk` is a host-side test double of the private AgentRQ
`connectors-sdk`, providing the same API surface so tests run anywhere. The
real SDK validates `manifest.json` at compile time and performs HTTP inside
the `wasm32-wasip2` guest.
