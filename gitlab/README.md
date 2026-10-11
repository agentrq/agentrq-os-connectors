# gitlab — GitLab connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the GitLab REST API (v4). Projects, branches, commits, files,
issues, merge requests and pipelines — 22 tools that mirror the API's
endpoints and parameter names.

![icon](icon.svg)

## Tools

| Tool | Maps to | What it does |
|---|---|---|
| `getProject` | `GET /projects/:id` | Project details |
| `listProjects` | `GET /projects` | Projects you can access |
| `createProject` | `POST /projects` | Create a project |
| `listBranches` | `GET /projects/:id/repository/branches` | List branches |
| `listCommits` | `GET /projects/:id/repository/commits` | List commits, optionally by path |
| `getCommit` | `GET /projects/:id/repository/commits/:sha` | One commit with its diff |
| `getFile` | `GET /projects/:id/repository/files/:file_path` | File contents (base64) |
| `listRepositoryTree` | `GET /projects/:id/repository/tree` | Directory listing |
| `createFile` | `POST /projects/:id/repository/files/:file_path` | Create a file |
| `updateFile` | `PUT /projects/:id/repository/files/:file_path` | Update a file |
| `deleteFile` | `DELETE /projects/:id/repository/files/:file_path` | Delete a file |
| `listIssues` | `GET /projects/:id/issues` | List issues |
| `getIssue` | `GET /projects/:id/issues/:issue_iid` | One issue |
| `createIssue` | `POST /projects/:id/issues` | Create an issue |
| `updateIssue` | `PUT /projects/:id/issues/:issue_iid` | Update title, description, state, labels, assignees |
| `createIssueNote` | `POST /projects/:id/issues/:issue_iid/notes` | Comment on an issue |
| `listMergeRequests` | `GET /projects/:id/merge_requests` | List merge requests |
| `getMergeRequest` | `GET /projects/:id/merge_requests/:merge_request_iid` | One merge request |
| `createMergeRequest` | `POST /projects/:id/merge_requests` | Open a merge request |
| `acceptMergeRequest` | `PUT /projects/:id/merge_requests/:merge_request_iid/merge` | Merge a merge request |
| `getCurrentUser` | `GET /user` | The signed-in user's profile |
| `listPipelines` | `GET /projects/:id/pipelines` | Pipelines |

`project_id` accepts a numeric project ID or a `namespace/project` path —
the connector URL-encodes it for you. Issues and merge requests are
addressed by their project-scoped internal ID (`issue_iid`,
`merge_request_iid`), as in the GitLab UI URLs.

## Auth

OAuth 2.0 (authorization code + PKCE), scope `api`. The host runs the
sign-in and injects the Bearer <redacted>; the connector code never sees it.
Configure the `client_id` / `client_secret` inputs and the `api_base` input
(default `https://gitlab.com/api/v4`).

To sign in you need a GitLab OAuth application (User Settings → Applications
on GitLab.com, or Admin → Applications for a self-managed instance). Set its
redirect URI to the host's callback —
`https://connectors.agentrq.com/oauth/callback` for the hosted server, or
`http://127.0.0.1:<port>/oauth/callback` when signing in from the CLI.

## Build & test

Built with the AgentRQ `wasmrs` connector toolchain (`oauth2` template):

```sh
cargo test          # unit tests (100% line coverage required)
```

`sdk/connectors-sdk` is a host-side test double of the private AgentRQ
`connectors-sdk`, providing the same API surface so tests run anywhere. The
real SDK validates `manifest.json` at compile time and performs HTTP inside
the `wasm32-wasip2` guest.
