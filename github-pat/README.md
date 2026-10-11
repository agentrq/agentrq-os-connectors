# github-pat — GitHub connector (personal access token)

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the GitHub REST API, authenticated with a **fine-grained
personal access token** you create. 20 tools mirroring the API's endpoints
and parameter names: repositories, branches, commits, files, issues, pull
requests and Actions workflow runs.

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
| `getAuthenticatedUser` | `GET /user` | The token owner's profile |
| `listWorkflowRuns` | `GET /repos/{owner}/{repo}/actions/runs` | Actions workflow runs |

## Auth

Paste a **fine-grained personal access token** at sign-in
(GitHub → Settings → Developer settings → Personal access tokens →
Fine-grained tokens → Generate new token). This is where you choose the
repositories the connector can touch:

- **Repository access:** Only select repositories → pick the ones you want.
- **Permissions** (grant what you will use):
  - Contents: Read and write — files, branches, commits
  - Issues: Read and write — issues and comments
  - Pull requests: Read and write — pull requests
  - Actions: Read — workflow runs
  - Metadata: Read is added automatically.

The host injects the token as `Authorization: Bearer …` after each request
leaves the connector; the connector code never sees it.

Two things to know:

- Fine-grained tokens **expire** (GitHub requires an expiry, at most a year).
  When it lapses, generate a new token and sign in again.
- `createRepo` does not work with a fine-grained token: GitHub answers
  `POST /user/repos` with "Resource not accessible by personal access
  token". For repository creation use the [`github`](../github/README.md)
  connector (OAuth), which asks for the `repo` scope instead.

## Build & test

Built with the AgentRQ `wasmrs` connector toolchain:

```sh
cargo test          # unit tests (100% line coverage required)
```

`sdk/connectors-sdk` is a host-side test double of the private AgentRQ
`connectors-sdk`, providing the same API surface so tests run anywhere. The
real SDK validates `manifest.json` at compile time and performs HTTP inside
the `wasm32-wasip2` guest.
