# gtasks — Google Tasks connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the Google Tasks API v1.

Unlike a 1:1 mapping of the API, the tools are **task-shaped**: each one
finishes an outcome in a single call. Lists and tasks are found by **ID or
by title** — the connector resolves names internally (one extra lookup when
needed) instead of making the agent do a `list` call first. Adding a task to
"Groceries", completing "Buy milk", or moving a task to another list each
take one tool call, even though the connector may make several HTTP requests
to get there.

![icon](icon.svg)

## Tools

| Tool | What it does in one call |
|---|---|
| `listTaskLists` | List all your task lists (ID and title) |
| `createTaskList` | Create a new task list |
| `renameTaskList` | Rename a list, by ID or current title |
| `deleteTaskList` | Delete a list, by ID or title (answers `{"deleted": true}`) |
| `listTasks` | List tasks on a list — open tasks by default, with due-date filters and an option to include completed ones |
| `addTask` | Add a task: the list is found by ID or title; place it on top, as a subtask, or after a sibling task |
| `updateTask` | Change a task's title, notes, or due date; find it by ID or title |
| `completeTask` | Mark a task complete; find it by ID or title |
| `reopenTask` | Reopen a completed task; find it by ID or title |
| `moveTask` | Reorder a task, nest it as a subtask, or move it to another list — lists and tasks by ID or title |
| `deleteTask` | Delete a task, by ID or title (answers `{"deleted": true}`) |
| `clearCompletedTasks` | Hide all completed tasks on a list (answers `{"cleared": true}`) |

Every tool takes an optional `list` (a list ID or title); omit it and the
default list is used. A title that matches nothing fails with the lists you
do have, so the agent can pick one; a task title that matches several tasks
fails with their IDs so the agent can be specific.

`due` is date-only: it is passed through as given and the API discards any
time portion. There are no times of day and no reminders in Google Tasks.

## Auth

OAuth 2.0 (authorization code + PKCE), scope
`https://www.googleapis.com/auth/tasks`. The host runs the sign-in and
injects the Bearer token; the connector code never sees it. Configure the
`client_id` / `client_secret` inputs and the `api_base` input
(default `https://tasks.googleapis.com/tasks/v1`).

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
