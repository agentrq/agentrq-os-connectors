# gdocs — Google Docs connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the Google Docs API v1. Read a document's content, create new
documents, and apply batch edits (insert/delete text, replace text, formatting).

![icon](icon.svg)

## Tools

| Tool | Maps to | What it does |
|---|---|---|
| `getDocument` | `documents.get` | Read title, body structure, tabs, formatting |
| `createDocument` | `documents.create` | Create an empty document with a title |
| `batchUpdate` | `documents.batchUpdate` | Apply edit requests, with optional `writeControl` guards |

## Auth

OAuth 2.0 (authorization code + PKCE), scope
`https://www.googleapis.com/auth/documents`. The host runs the sign-in and
injects the bearer token; the connector code never sees it. Configure the
`client_id` / `client_secret` inputs and the `api_base` input
(default `https://docs.googleapis.com`).

## Build & test

Built with the AgentRQ `wasmrs` connector toolchain (`oauth2` template):

```sh
cargo test          # unit tests (100% line coverage required)
```

`sdk/connectors-sdk` is a host-side test double of the private AgentRQ
`connectors-sdk`, providing the same API surface so tests run anywhere. The
real SDK validates `manifest.json` at compile time and performs HTTP inside
the `wasm32-wasip2` guest.
