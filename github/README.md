# github — GitHub connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the GitHub REST API. Repositories, branches, commits, files,
issues, pull requests and Actions workflow runs — 20 tools that mirror the
API's endpoints and parameter names.

![icon](icon.svg)

## Tools

| Tool | Maps to | What it does |
|---|---|---|
| `getRepo` | `GET /repos/{owner}/{repo}` | Repository details |
| `listRepos` | `GET /user/repos` | Repositories you can access |
| `createRepo` | `POST /user/repos` | Create a repository |
| `listBranches` | `GET /repos/{owner}/{repo}/branches` | List branches |
| `listCommits` | `GET /repos/{owner}/{repo}/commits` | List commits, optionally by path |
| `getCommit` | `GET /repos/{owner}/{repo}/commits/{ref}` | One commit with files and stats |
| `getContents` | `GET /repos/{owner}/{repo}/contents/{path}` | File or directory contents |
| `createOrUpdateFile` | `PUT /repos/{owner}/{repo}/contents/{path}` | Create or update a file (base64) |
| `deleteFile` | `DELETE /repos/{owner}/{repo}/contents/{path}` | Delete a file |
| `listIssues` | `GET /repos/{owner}/{repo}/issues` | List issues |
| `getIssue` | `GET /repos/{owner}/{repo}/issues/{number}` | One issue |
| `createIssue` | `POST /repos/{owner}/{repo}/issues` | Create an issue |
| `updateIssue` | `PATCH /repos/{owner}/{repo}/issues/{number}` | Update title, body, state, labels, assignees |
| `createIssueComment` | `POST /repos/{owner}/{repo}/issues/{number}/comments` | Comment on an issue |
| `listPulls` | `GET /repos/{owner}/{repo}/pulls` | List pull requests |
| `getPull` | `GET /repos/{owner}/{repo}/pulls/{number}` | One pull request |
| `createPull` | `POST /repos/{owner}/{repo}/pulls` | Open a pull request |
| `mergePull` | `PUT /repos/{owner}/{repo}/pulls/{number}/merge` | Merge a pull request |
| `getAuthenticatedUser` | `GET /user` | The signed-in user's profile |
| `listWorkflowRuns` | `GET /repos/{owner}/{repo}/actions/runs` | Actions workflow runs |

## Auth

OAuth 2.0 (authorization code + PKCE), scope `repo`. The host runs the
sign-in and injects the Bearer <redacted>; the connector code never sees it.
Configure the `client_id` / `client_secret` inputs and the `api_base` input
(default `https://api.github.com`).

To sign in you need a GitHub OAuth App (Settings → Developer settings →
OAuth Apps). Set its Authorization callback URL to the host's callback —
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
