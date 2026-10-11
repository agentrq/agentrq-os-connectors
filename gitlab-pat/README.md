# gitlab-pat — GitLab connector (personal access token)

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the GitLab REST API (v4), authenticated with a **personal
access token** you create. The 22 tools are shaped around what an agent wants
to accomplish — reading files as text, writing them without a create/update
split, fetching issues and merge requests together with their notes, and
proposing multi-file changes as a merge request — instead of mirroring the
API's endpoints one to one. One tool may make several API calls internally to
complete the job.

![icon](icon.svg)

## Tools

| Tool | What it does |
|---|---|
| `getProject` | Project details |
| `listProjects` | Projects you can access |
| `createProject` | Create a project |
| `readFile` | File content as text at a branch, tag or commit (binary files come back base64 with `is_binary`) |
| `listDirectory` | A directory's files and subdirectories as `name`/`path`/`type` entries |
| `writeFile` | Create or update a file with plain-text content — one call either way |
| `deleteFile` | Delete a file |
| `listBranches` | List branches |
| `listCommits` | List commits, optionally by path |
| `getCommit` | One commit with its diff |
| `listIssues` | List issues |
| `getIssue` | One issue, with its notes |
| `createIssue` | Create an issue |
| `updateIssue` | Update title, description, state, labels, assignees |
| `commentOnIssue` | Comment on an issue |
| `listMergeRequests` | List merge requests |
| `getMergeRequest` | One merge request, with its notes |
| `createMergeRequest` | Open a merge request |
| `acceptMergeRequest` | Merge a merge request |
| `proposeChanges` | Create a branch, commit every file in one commit, open the merge request |
| `getCurrentUser` | The token owner's profile |
| `listPipelines` | Pipelines |

`project_id` accepts a numeric project ID or a `namespace/project` path —
the connector URL-encodes it for you. Issues and merge requests are
addressed by their project-scoped internal ID (`issue_iid`,
`merge_request_iid`), as in the GitLab UI URLs.

## What changed in 0.2.0

The tool surface was redesigned around agent outcomes instead of API
endpoints:

| Before | After | Why |
|---|---|---|
| `getFile` (base64) | `readFile` | Returns decoded text directly; binary files come back as `content_base64` with `is_binary: true`. Agents stop hand-decoding base64. |
| `listRepositoryTree` | `listDirectory` | Slimmer `{name, path, type}` entries; the recursive/pagination knobs are gone. |
| `createFile` + `updateFile` | `writeFile` | One call creates or updates: the connector probes existence and picks the right call. Content is plain text (`encoding: "text"`), never base64. |
| `createIssueNote` | `commentOnIssue` | Verb-first name, same behavior. |
| `getIssue` | `getIssue` (enhanced) | Now returns `{issue, notes}` — the discussion comes along, no second call. |
| `getMergeRequest` | `getMergeRequest` (enhanced) | Now returns `{merge_request, notes}`. The diff stays one `getCommit` on the merge request's SHA away. |
| — | `proposeChanges` (new) | The flagship composed tool: creates the branch, commits all files in **one** multi-action commit (probing each file so existing files are updated and missing files are created), and opens the merge request. Returns `{merge_request_iid, url}`. The title doubles as the commit message. |

`deleteFile`'s `commit_message` is now optional, and a successful delete
answers `{"deleted": true}`.

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
