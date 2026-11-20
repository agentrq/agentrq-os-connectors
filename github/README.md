# github — GitHub connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the GitHub REST API. 22 outcome-oriented tools for repositories,
branches, commits, files, issues, pull requests and Actions workflow runs —
designed so the agent makes one call per outcome instead of choreographing API
calls itself.

![icon](icon.svg)

## Tools

| Tool | What it does |
|---|---|
| `getRepo` | Repository details |
| `listRepos` | Repositories you can access |
| `createRepo` | Create a repository |
| `readFile` | Read a file's content as decoded text (binary files come back as base64 with `isBinary: true`) |
| `listDirectory` | List a directory's entries (`name`, `path`, `type`, `sha`, `size`); defaults to the root |
| `writeFile` | Create or update a file with plain-text content; resolves the current SHA internally |
| `deleteFile` | Delete a file; resolves its SHA internally |
| `listBranches` | List branches |
| `listCommits` | List commits, optionally by branch and path |
| `getCommit` | One commit with its changed files and stats |
| `listIssues` | List issues, optionally by state and labels |
| `getIssue` | One issue **with its comments** |
| `createIssue` | Create an issue |
| `updateIssue` | Update title, body, state, labels, assignees |
| `commentOnIssue` | Comment on an issue |
| `listPulls` | List pull requests |
| `getPull` | One pull request **with its changed files** (`filename`, `status`, `additions`, `deletions`) |
| `createPull` | Open a pull request |
| `mergePull` | Merge a pull request |
| `proposeChanges` | Write files to a new branch and open a pull request — the whole flow in one call |
| `getAuthenticatedUser` | The signed-in user's profile |
| `listWorkflowRuns` | Actions workflow runs |

## What changed in 0.2.0

The tool surface was redesigned around agent outcomes instead of API
endpoints (repo `AGENTS.md`: connectors are not 1:1 API mappings). One tool
may make several API calls internally; the agent makes one call per outcome.

| Before (0.1.x) | After (0.2.0) | Rationale |
|---|---|---|
| `getContents` (file *or* directory, base64) | `readFile` + `listDirectory` | The dual file/dir behavior was confusing; `readFile` also decodes base64 internally, so agents stop hand-decoding |
| `createOrUpdateFile` (base64 in, SHA in) | `writeFile` (plain text in, no SHA) | The connector resolves the current SHA itself (404 → create); the agent passes plain text |
| `deleteFile` (SHA in) | `deleteFile` (no SHA) | The connector resolves the SHA itself |
| `getIssue` | `getIssue` **+ comments** | Reading an issue with its discussion in one call (2 API calls internally) |
| `getPull` | `getPull` **+ files** | Reviewing a PR with its diff stats in one call (2 API calls internally) |
| `createIssueComment` | `commentOnIssue` | Verb-first naming, consistent with the other tools |
| — | `proposeChanges` (new) | The flagship composed tool: resolve base → create tree (one call for N files) → commit → branch → PR (5 API calls internally). "Make these edits and open a PR" goes from ~6 agent calls to one |

Everything else (`getRepo`, `listRepos`, `createRepo`, `listBranches`,
`listCommits`, `getCommit`, `listIssues`, `createIssue`, `updateIssue`,
`listPulls`, `createPull`, `mergePull`, `getAuthenticatedUser`,
`listWorkflowRuns`) is unchanged.

## Auth

OAuth 2.0 (authorization code + PKCE), scope `repo`. The host runs the
sign-in and injects the Bearer token; the connector code never sees it.
Configure the `client_id` / `client_secret` inputs and the `api_base` input
(default `https://api.github.com`).

To sign in you need a GitHub OAuth App (Settings → Developer settings →
OAuth Apps). Set its Authorization callback URL to the host's callback —
`https://connectors.agentrq.com/oauth/callback` for the hosted server, or
`http://127.0.0.1:<port>/oauth/callback` when signing in from the CLI.

## Build & test

Built with the AgentRQ `wasmrs` connector toolchain (`oauth2` template):

```sh
cargo test          # unit tests (100% line and function coverage required)
```

`sdk/connectors-sdk` is a host-side test double of the private AgentRQ
`connectors-sdk`, providing the same API surface so tests run anywhere. The
real SDK validates `manifest.json` at compile time and performs HTTP inside
the `wasm32-wasip2` guest.
