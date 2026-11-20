# gmail — Gmail connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the Gmail API v1, designed around what agents actually do with
mail: search and read with full bodies in one call, send/reply/forward without
hand-building MIME, manage drafts, organize mail by outcome, fetch attachments,
and manage labels.

![icon](icon.svg)

## Tools

Task-shaped tools: each one completes a job in a single call, making whatever
API requests the job needs internally. Every tool takes an optional `userId`
(default `"me"`).

| Tool | What it does |
|---|---|
| `searchEmails` | Search the mailbox and get full message contents in one call: headers, plain-text and optional HTML bodies, attachment metadata (`query`, `maxResults` up to 50, `includeHtml`) |
| `readThread` | Read a whole conversation: every message with headers and bodies, chronological |
| `sendEmail` | Send an email; the connector builds the MIME (plain text, HTML alternative, attachments) and base64url-encodes it |
| `replyToEmail` | Reply in one call: reads the original's headers, addresses the sender, sets `Re:` / `In-Reply-To` / `References`, threads it |
| `forwardEmail` | Forward in one call: optional note plus the original's headers and text (original attachments are not carried) |
| `saveDraft` | Create a draft, or update it when `draftId` is given; `inReplyToMessageId` threads the draft as a reply |
| `sendDraft` | Send an existing draft |
| `discardDraft` | Permanently discard a draft (answers `{"deleted": true}`; Gmail returns 204) |
| `organizeEmail` | Archive, trash, untrash, mark read/unread, or add/remove labels on messages and threads — by IDs or by search `query` for bulk action |
| `getAttachment` | Download an attachment by attachment ID or file name; returns file name, MIME type, size, base64 content |
| `manageLabels` | List, create, rename, or delete mailbox labels |
| `getProfile` | The account's email address and total message/thread counts |

## What changed in 0.2.0

The 34 endpoint-mirroring tools became 12 outcome-oriented tools. The agent no
longer does list→get N+1 choreography, hand-assembles RFC 2822 MIME, or
remembers which label dance archives a message.

| Before (0.1.x) | After (0.2.0) | Notes |
|---|---|---|
| `listMessages` + `getMessage` | `searchEmails` | One call: search, then full bodies, headers, attachment metadata |
| `listThreads` + `getThread` | `readThread` | One call: the whole conversation, chronological |
| `sendMessage` | `sendEmail` | Takes `to`/`subject`/`body`/`htmlBody`/`attachments`; the connector builds and base64url-encodes the MIME |
| `getMessage` + `sendMessage` by hand | `replyToEmail` | New: threading headers and `Re:` subject handled internally |
| `getMessage` + `sendMessage` by hand | `forwardEmail` | New: `Fwd:` subject, note plus original headers/text |
| `createDraft` + `updateDraft` | `saveDraft` | `draftId` present → update; `inReplyToMessageId` threads as a reply |
| `sendDraft` | `sendDraft` | Same job; the parameter is now `draftId` (was `id`) |
| `deleteDraft` | `discardDraft` | Same job, clearer name |
| `trashMessage`, `untrashMessage`, `modifyMessage`, `batchModifyMessages`, `trashThread`, `untrashThread`, `modifyThread` | `organizeEmail` | One `action` enum (`archive`, `trash`, `untrash`, `markRead`, `markUnread`, `addLabels`, `removeLabels`) over message IDs, thread IDs, or a bulk search `query` |
| `getMessageAttachment` | `getAttachment` | Also resolves by file `filename`, not just attachment ID |
| `listLabels`, `getLabel`, `createLabel`, `updateLabel`, `patchLabel`, `deleteLabel` | `manageLabels` | One `action` enum (`list`, `create`, `rename`, `delete`); single-label fetch is covered by `list` |
| `getProfile` | `getProfile` | Same job; shaped to `emailAddress`, `messagesTotal`, `threadsTotal` |

Deliberately dropped:

- `insertMessage`, `importMessage` — migration edge cases; agents move mail with `forwardEmail`.
- `deleteMessage`, `batchDeleteMessages`, `deleteThread` — permanent-delete footguns. Trash is recoverable, matching the Gmail skill guidance; nothing in 0.2.0 permanently deletes mail.
- `listHistory`, `watchMailbox`, `stopMailbox` — push-notification plumbing agents never use.
- `listDrafts`, `getDraft` — draft discovery via `searchEmails` with `query: "in:draft"`.

## Auth

OAuth 2.0 (authorization code + PKCE), scope
`https://www.googleapis.com/auth/gmail.modify`. The host runs the sign-in and
injects the Bearer <redacted>; the connector code never sees it. Configure the
`client_id` / `client_secret` inputs and the `api_base` input
(default `https://www.googleapis.com`).

## Build & test

Built with the AgentRQ `wasmrs` connector toolchain (`oauth2` template):

```sh
cargo test          # unit tests (100% line and function coverage required)
cargo clippy --all-targets   # zero warnings
```

`sdk/connectors-sdk` is a host-side test double of the private AgentRQ
`connectors-sdk`, providing the same API surface so tests run anywhere. The
real SDK validates `manifest.json` at compile time and performs HTTP inside
the `wasm32-wasip2` guest.

Failed upstream calls are tool results, not connector errors: a step that the
API refuses answers with a tool error naming the step (e.g. `"sending the
message failed: the API answered 403: …"`), so the model can react.
