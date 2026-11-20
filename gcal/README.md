# gcal — Google Calendar connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the Google Calendar API v3. List agendas, fetch event details,
make scheduling changes, and find free time.

![icon](icon.svg)

## Tools

| Tool | Maps to | What it does |
|---|---|---|
| `listEvents` | `events.list` | List events, with `timeMin`/`timeMax`, `q`, `singleEvents`, `orderBy` |
| `getEvent` | `events.get` | One event's full details |
| `createEvent` | `events.insert` | Create an event (`sendUpdates` to notify guests) |
| `updateEvent` | `events.patch` | Patch semantics — only given fields change |
| `deleteEvent` | `events.delete` | Delete an event |
| `findFreeTime` | `freebusy.query` + slot computation | Free slots of at least `durationMinutes` in a window, across calendars |

`findFreeTime` answers `{slots: [{start, end}]}` in chronological order,
formatted in the window's offset. Busy blocks from every searched calendar
are merged (overlapping or adjacent blocks leave no phantom slot); a
calendar the API reports an error for becomes a tool error rather than a
silent lie about free time.

## What changed in 0.2.0

The event tools were already task-shaped, so the redesign keeps them and
fills the one composed gap:

- **New: `findFreeTime`.** "Find a 30-minute slot next week" used to force
  the agent to `listEvents` and do interval math by hand. Now one call runs
  a single `freebusy.query` and the connector computes the free slots
  itself — a pure, fully unit-tested sweep over the merged busy intervals.
- **Internal structure.** All tools now go through `call_with` with an
  injected sender (the pattern from `gdrive`): production sends over HTTP,
  tests drain queued responses, so every line after a send is covered.
  Each tool is `plan_*` / `parse_*` / `combine_*` pure helpers. Behavior
  of the five event tools is unchanged.

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
