# gitlab-pat — GitLab connector (personal access token)

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the GitLab REST API (v4), authenticated with a **personal
access token** you create. 22 tools mirroring the API's endpoints and
parameter names: projects, branches, commits, files, issues, merge requests
and pipelines.

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
| `getCurrentUser` | `GET /user` | The token owner's profile |
| `listPipelines` | `GET /projects/:id/pipelines` | Pipelines |

`project_id` accepts a numeric project ID or a `namespace/project` path —
the connector URL-encodes it for you. Issues and merge requests are
addressed by their project-scoped internal ID (`issue_iid`,
`merge_request_iid`), as in the GitLab UI URLs.

## Auth

Paste a **personal access token** at sign-in
(GitLab → User Settings → Access Tokens → Add new token). Give it the `api`
scope for the full toolset, or narrower scopes for read-only use (`read_api`,
`read_repository`). The token's scope decides what the connector can touch.

The host injects the token as `Authorization: Bearer …` after each request
leaves the connector; the connector code never sees it.

One thing to know: personal access tokens **expire** (GitLab defaults to a
year at most). When it lapses, create a new token and sign in again.

For OAuth 2.0 sign-in instead, use the [`gitlab`](../gitlab/README.md)
connector, which asks for the `api` scope.

## Build & test

Built with the AgentRQ `wasmrs` connector toolchain:

```sh
cargo test          # unit tests (100% line coverage required)
```

`sdk/connectors-sdk` is a host-side test double of the private AgentRQ
`connectors-sdk`, providing the same API surface so tests run anywhere. The
real SDK validates `manifest.json` at compile time and performs HTTP inside
the `wasm32-wasip2` guest.
