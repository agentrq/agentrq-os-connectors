# gcal — Google Calendar connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the Google Calendar API v3. List agendas, fetch event details,
and make scheduling changes.

![icon](icon.svg)

## Tools

| Tool | Maps to | What it does |
|---|---|---|
| `listEvents` | `events.list` | List events, with `timeMin`/`timeMax`, `q`, `singleEvents`, `orderBy` |
| `getEvent` | `events.get` | One event's full details |
| `createEvent` | `events.insert` | Create an event (`sendUpdates` to notify guests) |
| `updateEvent` | `events.patch` | Patch semantics — only given fields change |
| `deleteEvent` | `events.delete` | Delete an event |

## Auth

OAuth 2.0 (authorization code + PKCE), scope
`https://www.googleapis.com/auth/calendar`. The host runs the sign-in and
injects the bearer token; the connector code never sees it. Configure the
`client_id` / `client_secret` inputs and the `api_base` input
(default `https://www.googleapis.com`).

## Build & test

Built with the AgentRQ `wasmrs` connector toolchain (`oauth2` template):

```sh
cargo test          # unit tests (100% line coverage required)
```

`sdk/connectors-sdk` is a host-side test double of the private AgentRQ
`connectors-sdk`, providing the same API surface so tests run anywhere. The
real SDK validates `manifest.json` at compile time and performs HTTP inside
the `wasm32-wasip2` guest.
