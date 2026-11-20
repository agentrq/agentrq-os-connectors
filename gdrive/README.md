# gdrive — Google Drive connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the Google Drive API v3. Search files, read file content and
metadata, create files and folders, upload content, rename, move, trash and
restore, copy, permanently delete, and share.

![icon](icon.svg)

## Tools

| Tool | Maps to | What it does |
|---|---|---|
| `searchFiles` | `files.list` | Search or list files (`q`, `pageSize`, `orderBy`, ...) |
| `readFile` | `files.get` + `files.export` | Read a file's content: downloads regular files, exports Google Docs/Sheets/Slides |
| `getFile` | `files.get` | Get a file's metadata by ID |
| `createFile` | `files.create` | Create a file or folder; `mimeType: application/vnd.google-apps.folder` makes a folder |
| `updateFile` | `files.update` | Rename, move (`addParents`/`removeParents`), trash/restore (`trashed`), replace content |
| `copyFile` | `files.copy` | Copy a file |
| `deleteFile` | `files.delete` | Permanently delete a file (answers `{"deleted": true}`; Drive returns 204) |
| `shareFile` | `permissions.create` | Share a file with a user, group, domain, or anyone |

Inputs mirror the Drive API v3 parameter names (`fileId`, `pageSize`, `q`, ...).

`readFile` fetches the file's metadata first, then downloads regular files
directly (`files.get?alt=media`) or exports Google-native files (Docs, Sheets,
Slides) to `exportMimeType` (default `text/plain`). It answers
`{fileId, name, mimeType, size, contentBase64, exportedAs?}`, where
`exportedAs` is present only when the content was exported.

## What changed in 0.2.0

- **New: `readFile`** — the connector can now read a file's bytes, the biggest
  gap in the old surface. One call does the whole job: metadata fetch, then
  download or export.
- **Renamed: `listFiles` → `searchFiles`** — same parameters and behavior; the
  new name says what the agent accomplishes instead of echoing the API.
- **Renamed: `createPermission` → `shareFile`** — same parameters and
  behavior; the old name was Drive API jargon for granting a permission.

Uploading content (`contentBase64` on `createFile` / `updateFile`) uses
`uploadType=multipart`: the file metadata as JSON plus the base64-decoded
content in a `multipart/related` body. `contentMimeType` sets the uploaded
part's type; it defaults to `mimeType` on create and to
`application/octet-stream` otherwise.

## Auth

OAuth 2.0 (authorization code + PKCE), scope
`https://www.googleapis.com/auth/drive`. The host runs the sign-in and
injects the Bearer <redacted>; the connector code never sees it. Configure the
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
