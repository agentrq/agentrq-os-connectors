# outlook-mail — Outlook Mail connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for Microsoft Graph v1.0 mail, designed around what agents actually do
with mail: browse a folder, search with full bodies in one call, read, send, reply
and forward, manage drafts, organize mail by outcome, fetch attachments, and work
across shared mailboxes.

![icon](icon.svg)

## Tools

Task-shaped tools: each one completes a job in a single call, making whatever
API requests the job needs internally. Every mail tool takes an optional `mailbox`
(default `"me"`): the signed-in user's mailbox, or the address of a shared or
delegated mailbox.

| Tool | What it does |
|---|---|
| `searchMessages` | The one mail-finding tool: with a KQL-style `query` it searches every folder (or just `folder` when given) and returns full message contents — headers, plain-text or HTML body (`includeHtml`), attachment metadata; without a query it browses one folder, newest first. `unreadOnly` filters to unread in both modes; `maxResults` up to 50, `skip` pages |
| `getMessage` | Read one message with its full body |
| `sendMessage` | Send an email: `to`/`cc`/`bcc`, `subject`, `body` as text or HTML (`bodyType`), attachments ride along in the same call |
| `replyToMessage` | Reply in one call via the Graph reply action: threading, quoting and `Re:` handled by the server; `replyAll` answers everyone |
| `forwardMessage` | Forward in one call: optional note plus the original, quoted by the server |
| `saveDraft` | Create a draft, or update it when `draftId` is given; `inReplyToMessageId` threads the draft as a reply |
| `sendDraft` | Send an existing draft |
| `discardDraft` | Permanently discard a draft (answers `{"deleted": true}`; Graph returns 204) |
| `organizeMessage` | Mark read/unread, archive, delete, or move messages — by IDs or by search `query` for bulk action. Delete moves to Deleted Items and is recoverable; nothing here permanently deletes mail |
| `getAttachment` | Download an attachment by attachment ID or file name; returns file name, MIME type, size, base64 content |
| `listMailAccounts` | The signed-in mailbox. Shared/delegated mailboxes cannot be enumerated (see below) — pass their address as `mailbox` |

## Mailboxes and scopes

Every mail tool targets `{root}/v1.0/me/...` by default. Passing `mailbox` swaps
`/me` for `/users/{address}` (e.g. `POST /users/{address}/sendMail`), which Graph
serves when the signed-in user has a sharing or delegation grant on that mailbox.
If there is no grant, Graph answers with an error and the tool surfaces it as a
named tool error — the connector never masks it.

The OAuth scopes are:

- `https://graph.microsoft.com/Mail.ReadWrite` — the signed-in user's mailbox
- `https://graph.microsoft.com/Mail.Send` — sending from the signed-in mailbox
- `https://graph.microsoft.com/Mail.ReadWrite.Shared` — shared/delegated mailboxes
- `https://graph.microsoft.com/Mail.Send.Shared` — sending from shared/delegated mailboxes
- `offline_access` — refresh tokens for the host

The `.Shared` scopes are what make the `mailbox` parameter work on other
people's mail; without a sharing/delegation grant on the target mailbox Graph
refuses the call. Neither `.Shared` scope requires admin consent.

Microsoft provides **no** Graph endpoint that enumerates "mailboxes I can
access", so `listMailAccounts` returns only the signed-in account and documents
the limit. Shared-mailbox addresses are always passed explicitly via `mailbox`.

## Auth

OAuth 2.0 (authorization code + PKCE) against
`https://login.microsoftonline.com/common/oauth2/v2.0/`. The host runs the
sign-in and injects the Bearer token; the connector code never sees it.
Configure the `client_id` / `client_secret` inputs and the `api_base` input
(default `https://graph.microsoft.com`).

## Build & test

Built with the AgentRQ `wasmrs` connector toolchain (`oauth2` template):

```sh
cargo test          # unit tests (100% line and region coverage required)
cargo clippy --all-targets   # zero warnings (one known dual-SDK needless_question_mark, as in gmail)
```

`sdk/connectors-sdk` is a host-side test double of the private AgentRQ
`connectors-sdk`, providing the same API surface so tests run anywhere. The
real SDK validates `manifest.json` at compile time and performs HTTP inside
the `wasm32-wasip2` guest.

Failed upstream calls are tool results, not connector errors: a step that the
API refuses answers with a tool error naming the step (e.g. `"sending the
message failed: the API answered 403: …"`), so the model can react.
