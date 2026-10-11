# gmail — Gmail connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the Gmail API v1. Search and read messages and threads, send
mail, create and send drafts, manage labels, trash and restore, permanently
delete, track mailbox history, and set up push notifications.

![icon](icon.svg)

## Tools

Inputs mirror the Gmail API v1 camelCase parameter names (`userId`, `maxResults`,
`labelIds`, ...). Every tool takes an optional `userId` (default `"me"`).

| Tool | Maps to | What it does |
|---|---|---|
| `getProfile` | `users.getProfile` | Get the Gmail profile |
| `listMessages` | `users.messages.list` | List messages (`q`, `pageToken`, `maxResults`, `labelIds`, `includeSpamTrash`) |
| `getMessage` | `users.messages.get` | Get a message (`format`, `metadataHeaders`) |
| `sendMessage` | `users.messages.send` | Send a message (`raw`: base64url RFC 2822) |
| `insertMessage` | `users.messages.insert` | Insert a message directly (`raw`, `internalDateSource`, `deleted`) |
| `importMessage` | `users.messages.import` | Import a message (`raw`, `internalDateSource`, `neverMarkSpam`, `processForCalendar`, `deleted`) |
| `trashMessage` | `users.messages.trash` | Move a message to trash |
| `untrashMessage` | `users.messages.untrash` | Remove a message from trash |
| `deleteMessage` | `users.messages.delete` | Permanently delete a message (answers `{"deleted": true}`; Gmail returns 204) |
| `modifyMessage` | `users.messages.modify` | Add/remove labels on a message |
| `batchModifyMessages` | `users.messages.batchModify` | Add/remove labels on many messages |
| `batchDeleteMessages` | `users.messages.batchDelete` | Permanently delete many messages (answers `{"deleted": true}`; Gmail returns 204) |
| `getMessageAttachment` | `users.messages.attachments.get` | Get an attachment of a message |
| `listThreads` | `users.threads.list` | List threads (same filters as `listMessages`) |
| `getThread` | `users.threads.get` | Get a thread (`format`, `metadataHeaders`) |
| `modifyThread` | `users.threads.modify` | Add/remove labels on a thread |
| `trashThread` | `users.threads.trash` | Move a thread to trash |
| `untrashThread` | `users.threads.untrash` | Remove a thread from trash |
| `deleteThread` | `users.threads.delete` | Permanently delete a thread (answers `{"deleted": true}`; Gmail returns 204) |
| `listLabels` | `users.labels.list` | List labels |
| `getLabel` | `users.labels.get` | Get a label |
| `createLabel` | `users.labels.create` | Create a label |
| `updateLabel` | `users.labels.update` | Replace a label |
| `patchLabel` | `users.labels.patch` | Update fields of a label |
| `deleteLabel` | `users.labels.delete` | Delete a label (answers `{"deleted": true}`; Gmail returns 204) |
| `createDraft` | `users.drafts.create` | Create a draft (`raw`: base64url RFC 2822) |
| `updateDraft` | `users.drafts.update` | Replace a draft |
| `listDrafts` | `users.drafts.list` | List drafts |
| `getDraft` | `users.drafts.get` | Get a draft (`format`) |
| `sendDraft` | `users.drafts.send` | Send a draft |
| `deleteDraft` | `users.drafts.delete` | Delete a draft (answers `{"deleted": true}`; Gmail returns 204) |
| `listHistory` | `users.history.list` | List mailbox history changes |
| `watchMailbox` | `users.watch` | Set up push notifications to a Pub/Sub topic |
| `stopMailbox` | `users.stop` | Stop push notifications |

The `settings.*` subtree (filters, send-as identities, forwarding, vacation
responder, IMAP/POP, delegates, client-side encryption) is intentionally not
exposed: those endpoints are not offered by the skill this connector backs.

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
