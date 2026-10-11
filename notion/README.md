# notion — Notion connector (OAuth)

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the Notion API, authenticated with **OAuth 2.0** — the host runs the
sign-in, so the agent never sees the token. 12 outcome-oriented tools for pages,
data sources (Notion's databases), Markdown content and comments — designed so the
agent makes one call per outcome instead of choreographing API calls itself. The
tool surface is identical to the [`notion-pat`](../notion-pat/README.md) connector
(API key); only the auth differs.

![icon](icon.svg)

## Tools

| Tool | What it does |
|---|---|
| `search` | Search the workspace for pages and data sources by title |
| `getPage` | One page with properties as plain values; `includeContent` also reads the body as Markdown |
| `createPage` | Create a page in one call: parent data source or page by title or id, plain-value properties, optional Markdown content |
| `updatePage` | Update a page's properties by name; `trash` moves it to trash or restores it |
| `movePage` | Move a page to a new parent page (title or id) |
| `queryDataSource` | Query a data source by title or id with simple filters and sorts; rows come back with plain-value properties |
| `getDataSource` | A data source's schema: property names, types and select options |
| `createDataSource` | Create a data source under a parent page with a simple property schema, in one call |
| `appendContent` | Append Markdown to a page (converted to blocks internally) |
| `listComments` | Comments on a page |
| `createComment` | Comment on a page, or reply with `discussionId` |
| `getCurrentUser` | The authenticated user or bot |

## How it beats the 1:1 API mapping

Notion's official MCP server exposes one tool per endpoint, so an agent creating
a task means five calls: search the database, read its schema, build the
property JSON (`{"title": [{"text": {"content": "..."}}]}`), create the page,
append the blocks. Here that is one `createPage` call:

```json
{
  "parent": {"dataSource": "Tasks"},
  "properties": {"Name": "Write the launch post", "Status": "In progress", "Due": "2026-10-20"},
  "content": "## Outline\n- [ ] Draft\n- [ ] Review"
}
```

Titles resolve to ids inside the connector (an ambiguous title fails listing the
matches). Properties read and write as plain values — strings, numbers,
booleans, date strings, arrays of names — mapped through the data source schema
internally. Filters stay plain too: `{"Status": "Done"}` means equals,
`{"Due": {"before": "2026-12-01"}}` uses an operator, combined with `and`/`or`
groups.

## API version

The connector pins `Notion-Version: 2026-03-11` (the current version): databases
are queried as data sources (`POST /v1/data_sources/{id}/query`), trashed pages
report `inTrash`, and block appends use the `position` object. Page bodies are
read through `GET /v1/pages/{id}/markdown`.

## Auth

OAuth 2.0 authorization code with PKCE against `https://api.notion.com/v1/oauth/authorize`.
The integration's capabilities are configured in Notion (not via OAuth scopes).
Prefer a pasted token instead? Use [`notion-pat`](../notion-pat/README.md).
