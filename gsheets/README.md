# gsheets — Google Sheets connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the Google Sheets API v4. The tools are shaped around what an
agent wants to accomplish — read a table, write cells, create a populated
spreadsheet — rather than the API's endpoints, so most jobs finish in one
call.

![icon](icon.svg)

## Tools

| Tool | What it does |
|---|---|
| `getValues` | Read raw values from a range (`Sheet1!A1:B10`) |
| `readTable` | Read a range as a table: first row becomes headers, the rest become objects |
| `updateValues` | Write values to a range (`USER_ENTERED` by default) |
| `appendValues` | Append rows to a table |
| `clearValues` | Clear a range |
| `createSpreadsheet` | Create a spreadsheet, optionally with named sheets and their initial rows |
| `getSpreadsheet` | Inspect titles, sheets, grid properties |
| `batchUpdate` | Structural batch requests (escape hatch for formatting/structure) |

### What changed in 0.2.0

| Before | After | Why |
|---|---|---|
| `getValues` returned nested arrays | `readTable` added: `{headers, rows: [{col: value}], rowCount}` | Agents want tables, not `values[3][1]`; the raw read stays for index math |
| `createSpreadsheet` made an empty spreadsheet (1+N calls to populate) | `createSpreadsheet` takes `sheets: [{title, rows}]` and writes each sheet's rows in the same call | Creating a populated spreadsheet used to need one call per sheet |
| — | Everything else unchanged | `updateValues`, `appendValues`, `clearValues`, `getSpreadsheet`, `batchUpdate` were already task-shaped; the redesign only fills the two composed gaps |

`readTable` reuses `getValues`' fetch (same parameters), so choosing between
them is only about the shape of the answer. Sheets whose titles need quoting
in A1 notation are quoted automatically when `createSpreadsheet` writes their
rows.

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
