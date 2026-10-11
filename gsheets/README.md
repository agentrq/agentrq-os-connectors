# gsheets — Google Sheets connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the Google Sheets API v4. Read ranges, write cells, append rows,
clear ranges, create spreadsheets, inspect structure, and run batch updates.

![icon](icon.svg)

## Tools

| Tool | Maps to | What it does |
|---|---|---|
| `getValues` | `spreadsheets.values.get` | Read a range (`Sheet1!A1:B10`) |
| `updateValues` | `spreadsheets.values.update` | Write values to a range (`USER_ENTERED` by default) |
| `appendValues` | `spreadsheets.values.append` | Append rows to a table |
| `clearValues` | `spreadsheets.values.clear` | Clear a range |
| `createSpreadsheet` | `spreadsheets.create` | Create a spreadsheet |
| `getSpreadsheet` | `spreadsheets.get` | Inspect titles, sheets, grid properties |
| `batchUpdate` | `spreadsheets.batchUpdate` | Structural batch requests |

## Auth

OAuth 2.0 (authorization code + PKCE), scope
`https://www.googleapis.com/auth/spreadsheets`. The host runs the sign-in and
injects the bearer token; the connector code never sees it. Configure the
`client_id` / `client_secret` inputs and the `api_base` input
(default `https://sheets.googleapis.com`).

## Build & test

Built with the AgentRQ `wasmrs` connector toolchain (`oauth2` template):

```sh
cargo test          # unit tests (100% line coverage required)
```

`sdk/connectors-sdk` is a host-side test double of the private AgentRQ
`connectors-sdk`, providing the same API surface so tests run anywhere. The
real SDK validates `manifest.json` at compile time and performs HTTP inside
the `wasm32-wasip2` guest.
