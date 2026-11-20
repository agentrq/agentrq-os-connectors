# gforms — Google Forms connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the Google Forms API v1.

Unlike a 1:1 mapping of the API, the tools are **task-shaped**: each one
finishes an outcome in a single call. Creating a form takes its questions
along — no separate add-question calls — questions are found by **item ID or
by title** (resolved inside the connector), and responses come back with
answers keyed by **question title** as plain text, so reading results never
needs an ID-mapping step first.

![icon](icon.svg)

## Tools

| Tool | What it does in one call |
|---|---|
| `createForm` | Create a form with its title, description, and questions |
| `getForm` | Get a form with its questions in readable shape (ID, title, type, required, options) |
| `addQuestions` | Append questions to a form; returns the added questions with their IDs |
| `updateFormInfo` | Change a form's title and/or description |
| `updateQuestion` | Change a question's title, description, required flag, or choices — by item ID or question title |
| `deleteQuestion` | Delete a question — by item ID or question title (answers `{"deleted": true}`) |
| `publishForm` | Publish or unpublish a form (publishing opens it for responses) |
| `getResponses` | Read responses with answers keyed by question title as plain text |

Questions are described with a simple `type`: `text`, `paragraph`,
`multipleChoice`, `checkboxes`, `dropdown`, `scale`, `date`, `time`.
Choice questions take `options`; scales take `low`/`high` (default 1–5) with
optional `lowLabel`/`highLabel`. A question title that matches nothing fails
with the questions the form does have, so the agent can pick one; a title
that matches several fails with their IDs so the agent can be specific.

`getResponses` pages with `pageSize`/`pageToken` and answers
`nextPageToken`. Deleting a form itself is not part of the Forms API — use
the Drive connector's delete for that.

## Auth

OAuth 2.0 (authorization code + PKCE), scopes
`https://www.googleapis.com/auth/forms.body` and
`https://www.googleapis.com/auth/forms.responses.readonly`. The host runs the
sign-in and injects the Bearer token; the connector code never sees it.
Configure the `client_id` / `client_secret` inputs and the `api_base` input
(default `https://forms.googleapis.com/v1`).

## Build & test

Built with the AgentRQ `wasmrs` connector toolchain (`oauth2` template):

```sh
cargo test          # unit tests (100% line and function coverage required)
```

`sdk/connectors-sdk` is a host-side test double of the private AgentRQ
`connectors-sdk`, providing the same API surface so tests run anywhere. The
real SDK validates `manifest.json` at compile time and performs HTTP inside
the `wasm32-wasip2` guest.

The multi-call tools stay unit-testable through a small `Transport` trait:
the guest implementation sends requests over HTTP, while tests answer from
canned responses and assert on the requests the tools planned.
