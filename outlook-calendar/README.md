# outlook-calendar — Outlook Calendar connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the Outlook calendar API (Microsoft Graph v1.0). List agendas, fetch
event details, make scheduling changes, and find free time. Works with personal
Microsoft accounts and Microsoft 365 work or school accounts.

![icon](icon.svg)

## Tools

| Tool | Graph call | What it does |
|---|---|---|
| `listEvents` | `GET …/calendarView` or `GET …/events` | Events in a time window (recurring expanded), or the most recent events |
| `getEvent` | `GET …/events/{id}` | One event's full details, description as plain text |
| `createEvent` | `POST …/events` | Create an event from flat fields (subject, times, attendees, …) |
| `updateEvent` | `PATCH …/events/{id}` | Patch semantics — only given fields change |
| `deleteEvent` | `DELETE …/events/{id}` | Delete an event (204 → `{"deleted": true}`) |
| `findFreeTime` | `GET …/calendarView` × calendars + slot computation | Free slots of at least `durationMinutes` in a window, across calendars |

`findFreeTime` answers `{slots: [{start, end}]}` in chronological order, formatted in
the window's offset. Busy blocks from every searched calendar are merged (overlapping
or adjacent blocks leave no phantom slot); events marked `showAs: free` and cancelled
events never block a slot.

## Design notes (all verified in the official Graph docs, not assumed)

- **No text search.** Microsoft documents `$search` for messages, people and directory
  objects only — not events — so `listEvents` has no `query` parameter. Narrow the
  time window instead.
- **`findFreeTime` reads agendas, not `getSchedule`.** The docs mark `getSchedule`
  as *not supported* for delegated personal Microsoft accounts; `calendarView`
  works for both account types with the same `Calendars.ReadWrite` scope.
- **Times.** Agents pass RFC 3339 with offsets; the connector converts to Graph's
  `dateTime` wall time + named `timeZone` on write, and renders answers back in
  RFC 3339 via the `Prefer: outlook.timezone` header. `timeZone` accepts the Windows
  or IANA names Graph documents. A DST transition inside a `findFreeTime` window
  may shift slot labels by the transition amount — the window's own offset is used
  throughout.
- **Reads are shaped.** Raw event JSON never leaves the connector: list/get/create/
  update answer flat `{id, subject, start, end, isAllDay, showAs, location,
  attendees[], description, preview, webLink}` objects.

## Auth

OAuth 2.0 + PKCE against the Microsoft identity platform
(`login.microsoftonline.com/common`). Delegated scopes:

- `https://graph.microsoft.com/Calendars.ReadWrite` — the docs' delegated
  permission for every endpoint this connector calls, for work/school *and*
  personal accounts (no admin consent needed).
- `offline_access` — the identity platform's documented scope for refresh tokens.

## Internal structure

All tools go through `call_with` with an injected sender: production sends over
HTTP, tests drain queued responses, so every line after a send is covered. Each
tool is `plan_*` / `shape_*` / `combine_*` pure helpers around that seam, and
`findFreeTime` composes one `calendarView` query per calendar with pure slot
computation — the multi-call choreography lives in the connector, never in the
agent. RFC 3339 parsing/formatting is dependency-free (`parse_moment` /
`format_moment`).

## References

- [List events](https://learn.microsoft.com/en-us/graph/api/user-list-events)
- [List calendarView](https://learn.microsoft.com/en-us/graph/api/calendar-list-calendarview)
- [Create event](https://learn.microsoft.com/en-us/graph/api/calendar-post-events)
- [Update event](https://learn.microsoft.com/en-us/graph/api/event-update)
- [Delete event](https://learn.microsoft.com/en-us/graph/api/event-delete)
- [Get event](https://learn.microsoft.com/en-us/graph/api/event-get)
- [getSchedule](https://learn.microsoft.com/en-us/graph/api/calendar-getschedule) (not used — personal accounts unsupported)
- [Use the $search query parameter](https://learn.microsoft.com/en-us/graph/search-query-parameter) (not available for events)
