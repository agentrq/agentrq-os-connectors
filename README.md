# AgentRQ Connectors

The public catalogue of [AgentRQ](https://agentrq.com) connectors.

A **connector** is an MCP server compiled to a wasm component. You publish one by
adding the **`agentrq-connector`** topic to a public GitHub repository, committing
a `manifest.json`, and attaching the component to a release. A **remote MCP
server** is published the same way under **`agentrq-remote-connector`**, with a
`remote-connector.json` instead.

There is no registry to apply to, no submission form, and nobody arbitrating a
namespace. The next nightly run lists you. That openness is the point, and it is
also the caveat — **discovery is not endorsement**. The catalogue says a
repository exists, says what it claims about itself, and says what it checked.
It does not vouch for the code.

## The file

```
https://raw.githubusercontent.com/agentrq/agentrq-os-connectors/main/connectors.json
```

Point a server at it:

```sh
export CONNECTORS_PUBLIC_CATALOG_URL=https://raw.githubusercontent.com/agentrq/agentrq-os-connectors/main/connectors.json
```

Rebuilt every day at **00:00 UTC**, and committed only when something actually
changed — so `git log` on [`connectors.json`](connectors.json) is a readable
history of connectors appearing, moving and being excluded.

### Its `repositories` array is a contract

The server reads `repositories` with `PublicList` in
`crates/infrastructure/src/github_catalog.rs` of `agentrq/connectors`. Those nine
fields are spelled GitHub's way — snake_case, nullable — and **must not change**:

```jsonc
{
  "full_name": "acme/gmail-connector",
  "description": "Gmail for agents",      // null when the repository has none
  "stargazers_count": 420,
  "html_url": "https://github.com/acme/gmail-connector",
  "owner": { "avatar_url": "https://avatars.githubusercontent.com/u/1" },
  "license": { "spdx_id": "MIT" },
  "pushed_at": "2026-10-09T00:00:00Z",
  "topics": ["agentrq-connector", "gmail"],
  "private": false
}
```

Everything this catalogue adds rides along as extra keys on the same row. Neither
`PublicList` nor `Repository` uses `deny_unknown_fields`, so a server ignores all
of them — which is how the file is richer than the list it replaces without the
server needing to know anything new:

```jsonc
{
  // ...the nine fields above, then:
  "rank": 1,                 // 1-based, in the published order
  "score": 83.71,            // 0-100, see Ranking
  "warnings": [],            // things worth saying that are not faults
  "nameConflict": [],        // only when another repository claims the same name
  "connectors": [            // what this repository publishes, from its manifests
    {
      "name": "gmail",
      "title": "Gmail",
      "version": "0.1.0",
      "mcpVersion": "2026-07-28",
      "description": "Reads Gmail threads.",
      "websiteUrl": "https://mail.google.com",
      "icons": 1,
      "tools": ["listThreads", "draft"],
      "skills": false,
      "auth": "oauth2",
      "inputs": 1,
      "remote": false,
      "workflow": false,
      "path": "gmail/manifest.json"
    }
  ],
  "bundles": [],             // only when connectors-manifest.json declares some
  "release": {               // the latest release, confirmed to carry the component
    "tag": "v0.1.0",
    "publishedAt": "2026-10-01T00:00:00Z",
    "assets": [{ "name": "gmail.wasm", "size": 184320, "digest": "sha256:…" }]
  }
}
```

A row for a remote MCP server carries `remote` (`url`, `name`, `title`,
`description`) in place of `connectors` and `release`.

The file itself also carries `$comment`, `schemaVersion`, `generatedAt`,
`topics`, `acceptedLicenses`, `weights`, `truncated` (true only if GitHub's
1000-result cap hid entries), `counts`, and `excluded`.

## What is listed, and what is excluded

`repositories` holds only what a server could actually offer. Everything else
goes in `excluded`, with the reason, so an author can see why they are not in the
catalogue rather than wondering:

```jsonc
{
  "full_name": "acme/half-finished",
  "html_url": "https://github.com/acme/half-finished",
  "license": "GPL-3.0",
  "reason": "Licensed as \"GPL-3.0\". The catalogue lists wasm connectors under AGPL-3.0, Apache-2.0, MIT."
}
```

### Licences

A wasm connector is listed when its repository is **MIT**, **Apache-2.0** or
**AGPL-3.0** (`AGPL-3.0`, `AGPL-3.0-only` and `AGPL-3.0-or-later` are the same
licence — GitHub's search returns the short form, a detected `LICENSE` may carry
a long one, and refusing over which arrived would be a rule about the API rather
than about the licence).

A connector is code that runs on your machine with whatever its manifest asks
for, so "what may I do with this" is part of the decision to install, not
metadata. A repository with no licence GitHub can identify leaves that question
unanswered, and is excluded for the same reason.

The policy does **not** apply to remote MCP servers. There is no code to license
— the repository is a pointer to somebody's hosted service — so refusing one over
its own licence would be refusing to list the service.

### Icons and instructions

Every listed connector's `manifest.json` must carry a non-empty `icons` array
(each entry with an `https:` or `data:` `src`) and a non-empty `instructions`
string: guidance for the model using the connector. A connector without either
is excluded, with the reason naming the manifest and the missing field. The
`instructions` key is part of the manifest schema, so a manifest carrying it
still installs.

### Alphanumeric order

A `connectors-manifest.json` must list its connectors in alphanumeric order by
manifest path, compared code-point by code-point. The index is the order the
catalogue reads — and publishes — the connectors in, so it is the presentation
order for the repository; keeping it sorted keeps that order readable and
diffable. An index that is not sorted is excluded, with the reason naming the
two entries out of place.

### Errors and warnings

The server reads these same documents at install time, on one person's machine,
and tells that one person what went wrong. Nobody else ever sees it, least of all
the author — who has no failing install of their own connector. Reading them here
once a day, in public, is what turns a private install failure into something its
author can find.

It follows that this is stricter than the server in exactly one place, which is
what the split enforces:

- An **error** excludes the row. Most are things the server would refuse too: a
  document that is not JSON, a `manifestVersion` that is not 1, a name that is not
  a connector id, a path that leaves the repository, a release with no `.wasm`.
  Three are the catalogue's own bar: a manifest with no `icons`, or no
  `instructions`, or an index that does not list its connectors in alphanumeric
  order. The server installs whatever it can, but the catalogue lists
  only a connector that shows an icon, tells the model how to use it, and sits
  in an order a reader can scan.
- A **warning** is published beside the row and costs it points: a key the server
  does not know (today's server refuses it — a newer one may not), a connector
  with no description, a release asset with no digest for an install to
  verify, a pre-release, an archived repository, a contested name.

Two judgements are deliberately refused:

- **Whether a manifest's permissions, auth or limits are acceptable.** Those are
  the fields that say what a connector may *do*, and the screen that asks the
  user to agree to them is where they belong — checked against the host doing the
  asking, not against a nightly job.
- **Which of two connectors owns a contested name.** Both are listed, both name
  the other. Nothing here can tell the original from the impostor, and quietly
  hiding one would be the catalogue taking a side it cannot defend.

## Ranking

The server shows `repositories` in the order given, so this is the order a person
sees in Discover. Each row scores out of 100:

| Signal | Weight | How it is measured |
| --- | --- | --- |
| **Popularity** | 60 | `log10(stars + 1)` against a 10,000-star ceiling. Log, not linear: the distance from 0 to 50 stars is the meaningful one, and linearly it would be invisible next to 5,000 → 10,000. |
| **Freshness** | 30 | Half-life decay on `pushed_at`, halving every 90 days. |
| **Completeness** | 10 | Six checks: no warnings, the repository has a description, a connector has a description, a connector has an icon, there is a topic beyond the discovery one, and it is not archived. |

Ties break on score, then stars, then full name — the last one arbitrary but
*stable*, so two equal connectors do not swap places on every nightly run and
turn a quiet day into a commit.

The weights encode an opinion worth stating. Popularity is the loudest signal but
not the only one: a new connector with no stars would otherwise be unrankable,
and the catalogue would calcify into whatever was published first. Freshness is
the counterweight — something still maintained outranks a more popular project
abandoned two years ago, which is the judgement a person browsing would make
anyway. Completeness is the only signal an author can max out on day one, which
gives them something to do about their rank other than wait.

## Publishing

### A wasm connector

1. Add the **`agentrq-connector`** topic to a public repository.
2. License it MIT, Apache-2.0 or AGPL-3.0.
3. Commit a `manifest.json` at the root, and attach the built component to a
   release.

```json
{
  "$schema": "https://connectors.agentrq.com/schemas/manifest/v1.json",
  "manifestVersion": 1,
  "mcpVersion": "2026-07-28",
  "name": "gmail",
  "title": "Gmail",
  "version": "0.1.0",
  "description": "Reads Gmail threads and drafts messages.",
  "icons": [{ "src": "data:image/svg+xml;base64,…", "mimeType": "image/svg+xml", "sizes": ["any"] }],
  "tools": [{ "name": "listThreads", "inputSchema": { "type": "object" } }]
}
```

`manifestVersion`, `mcpVersion`, `name` and `version` are required. `name` is an
address rather than a label — `^[a-z][a-z0-9-]{0,62}$` — because it is the install
directory, the registry key, the prefix of every command (`gmail.listThreads`)
and the name of the release asset.

A repository publishing **several connectors** adds a `connectors-manifest.json`
at its root and names each release asset `<connector name>.wasm`:

```json
{
  "manifestVersion": 1,
  "connectors": [{ "manifest": "gmail/manifest.json" }, { "manifest": "drive/manifest.json" }],
  "bundles": [{ "name": "google", "title": "Google", "connectors": ["gmail", "drive"] }]
}
```

### A remote MCP server

Add the **`agentrq-remote-connector`** topic and a `remote-connector.json`:

```json
{
  "url": "https://mcp.example.com/mcp",
  "name": "example",
  "title": "Example",
  "description": "Search and edit Example documents."
}
```

Only `url` is required, and it must be `https:`.

## Working on the catalogue

```sh
npm test          # unit tests, no network, no disk
npm run coverage  # the same, with line coverage
npm run build     # rebuild connectors.json (set GITHUB_TOKEN to avoid rate limits)
```

[`scripts/manifest.mjs`](scripts/manifest.mjs) reads the three documents, as pure
functions with no network and no clock. Every rule in it exists to match one in
`crates/domain/src/catalog/` of `agentrq/connectors`; when that moves, this
follows. [`scripts/build-index.mjs`](scripts/build-index.mjs) is everything else,
and takes each side effect as an argument — which is why the two-topic search,
the release checks and the rate-limit handling are testable without a network.
There are no dependencies.

Two GitHub limits shape it:

- **1000 results maximum** for any one search. The search is therefore
  partitioned across disjoint star buckets, and reports `truncated` when even
  that was not enough rather than quietly returning a short list.
- **Rate limits** — ten search requests a minute unauthenticated, thirty with a
  token. This is why the catalogue is a file at all: a server that searched for
  itself would spend that budget on whoever opened Discover first. The Actions
  token's 5,000 requests an hour are also what makes a per-repository release
  check affordable.

## License

AGPL-3.0-only. See [LICENSE](LICENSE).
