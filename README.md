# AgentRQ Connectors

The public index of [AgentRQ](https://agentrq.com) connectors.

A connector is a GitHub repository that carries the **`agentrq-connector`** topic
and an **`agentrq-connector.json`** at its root. Nothing else is required: there
is no registry to apply to, no submission form, and nobody arbitrating a
namespace. Add the topic, commit the manifest, and the next nightly run lists it.

That openness is the point, and it is also the caveat — **discovery is not
endorsement**. The index says a repository exists and what it claims about
itself. It does not vouch for the code, which is why every listing carries an
owner, a star count and a last-pushed date rather than a badge.

## The file

```
https://raw.githubusercontent.com/agentrq/agentrq-os-connectors/main/connectors.json
```

Rebuilt every day at **00:00 UTC**, and committed only when something actually
changed — so `git log` on [`connectors.json`](connectors.json) is a readable
history of connectors appearing, moving and going quiet.

```jsonc
{
  "schemaVersion": 1,
  "topic": "agentrq-connector",
  "manifest": "agentrq-connector.json",
  "generatedAt": "2026-10-10T00:00:00.000Z",
  "truncated": false,        // true only if GitHub's 1000-result cap hid entries
  "count": 42,               // repositories carrying the topic
  "ok": 39,                  // of those, how many the app could install
  "warned": 7,               // how many carry at least one warning
  "weights": { "popularity": 60, "freshness": 30, "completeness": 10 },
  "connectors": [
    {
      "rank": 1,             // 1-based, in the published order
      "score": 83.71,        // 0-100, see Ranking
      "ok": true,            // false when the manifest or its release is broken
      "warnings": [],        // things worth saying that are not faults; see below
      "fullName": "acme/jira-connector",
      "owner": "acme",
      "name": "jira-connector",
      "description": "Jira issues, as agent tools.",
      "homepage": "https://acme.example",
      "license": "MIT",      // as GitHub detected it, '' when it could not
      "topics": ["jira"],    // the discovery topic itself is removed
      "stars": 420,
      "forks": 17,
      "openIssues": 3,
      "archived": false,
      "createdAt": "2026-01-02T00:00:00Z",
      "pushedAt": "2026-10-09T00:00:00Z",
      "defaultBranch": "main",
      "url": "https://github.com/acme/jira-connector",
      "manifest": { /* the normalised manifest; every field below */ },
      "asset": {             // the release asset, confirmed to exist
        "name": "jira-connector-1.2.0.tgz",
        "size": 184320,
        "downloadUrl": "https://github.com/acme/jira-connector/releases/download/v1.2.0/...",
        "publishedAt": "2026-10-01T00:00:00Z"
      }
    },
    {
      "rank": 41,
      "ok": true,
      "nameConflict": ["someone-else/jira"],   // only when another repo claims the same name
      "warnings": ["The name \"jira\" is also claimed by someone-else/jira."]
      // ...the same repository fields as above
    },
    {
      "rank": 42,
      "ok": false,
      "reason": "Release \"v1.2.0\" has no asset named \"jira.tgz\".",
      "fullName": "acme/half-finished"
      // ...the same repository fields as above
    }
  ]
}
```

## What the index checks, and what it refuses to decide

The index is built nightly by one machine. The app that installs a connector is
built separately and runs wherever somebody put it, possibly months behind or
ahead. That gap splits every check in two.

**Errors** (`ok: false`, with a `reason`) are facts about the document or the
repository that no version of anything could accept — a file that is not JSON, a
name that is not a name, a `sha256` that is not a digest, a hook level nobody
recognises, a release tag that does not exist, a release missing the asset the
manifest pins. The index states these with authority.

**Warnings** (`ok: true`, with `warnings`) are judgements that depend on a
version, or on something outside the document:

- a key this index has never heard of — a typo, or a field a newer app knows about
- no `artifact` declared yet, so there is nothing to install
- the release is a draft, or a pre-release
- the repository is archived
- the manifest's `license` contradicts the one GitHub detects in the repository
- `hooks.toolCall: "decide"` — the connector can approve tool calls on your behalf, not only refuse them
- the manifest `name` is claimed by another repository as well

Both are published. A repository with the topic and a broken manifest is **listed
as broken with the reason**, not dropped: the author needs to see why — a
silently missing connector is undebuggable from the outside — and anyone
following a link to it needs the index to explain itself rather than look empty.

Two things the index deliberately does **not** decide:

- **Whether `engines.agentrq` is satisfied.** The form is checked; satisfaction
  is not, because this job has no app version to satisfy it against. The app
  re-asks at install time, against the version of itself that is actually
  running.
- **Which of two connectors owns a contested name.** Both are listed, both say
  who they collide with. The index has no way to tell the original from the
  impostor, and quietly hiding one would be it taking a side it cannot defend.

## Ranking

Each connector scores out of 100, from three signals:

| Signal | Weight | How it is measured |
| --- | --- | --- |
| **Popularity** | 60 | `log10(stars + 1)` against a 10,000-star ceiling. Log, not linear, because the distance from 0 to 50 stars is the meaningful one and linearly it would be invisible next to 5,000 → 10,000. |
| **Freshness** | 30 | Half-life decay on `pushedAt`, halving every 90 days. |
| **Completeness** | 10 | Seven checks: the manifest and its release both check out, there are no warnings, and the listing has a description, a license, a homepage, at least one other topic, and is not archived. |

Working connectors are ranked above broken ones whatever their stars; ties break
on score, then stars, then full name — the last one arbitrary but *stable*, so
two equal connectors do not swap places on every nightly run.

The weights encode an opinion worth stating plainly. Popularity is the loudest
signal but not the only one: a brand-new connector with no stars would otherwise
be unrankable, and the list would calcify into whatever was published first.
Freshness is the counterweight — something still maintained outranks a more
popular project abandoned two years ago, which is the judgement a person
browsing would make anyway. Completeness is the only signal an author can max
out on day one, which gives them something to do about their rank other than
wait.

## Publishing a connector

1. Add the `agentrq-connector` topic to your repository.
2. Commit an `agentrq-connector.json` at the root of its default branch.

```json
{
  "name": "jira-connector",
  "displayName": "Jira",
  "version": "1.2.0",
  "description": "Jira issues, as agent tools.",
  "license": "MIT",
  "homepage": "https://github.com/acme/jira-connector",
  "keywords": ["jira", "issues"],
  "engines": { "agentrq": "^1.4" },
  "mcp": { "tools": ["createTask"], "workspace": ["loadMemory"] },
  "hooks": { "toolCall": "deny" },
  "net": ["*.atlassian.net"],
  "config": [{ "key": "token", "type": "secret", "label": "API token" }],
  "provides": { "drawers": [{ "format": "gantt", "entry": "drawers/gantt.mjs" }] },
  "artifact": {
    "release": "v1.2.0",
    "asset": "jira-connector-1.2.0.tgz",
    "sha256": "4f3c…64 hex characters"
  }
}
```

| Field | | |
| --- | --- | --- |
| `name` | required | An address, not a label — lowercase words joined by single hyphens. It ends up in a route, a registry key and the install directory. |
| `version` | required | `1`, `1.4`, `1.4.2`, `v1.4.2` or `1.4.2-rc.1`. |
| `description` | required | One sentence. The listing is mostly this. |
| `license` | required | An SPDX identifier, or `UNLICENSED`. A connector runs with real access, so the terms are part of the decision to install it — which is why silence is not an option. Spelled exactly: `mit` is rejected with the right spelling in the message. |
| `engines.agentrq` | required | `1.4.0`, `^1.4`, `~1.4` or `>=1.4`. Anything else is refused rather than guessed at. |
| `artifact` | optional | The release, the asset file name, and its SHA-256. The index confirms the release and the asset really exist. Without it the connector lists but nothing can install it. |
| `mcp.tools`, `mcp.workspace` | optional | Tool names the connector calls. |
| `hooks.toolCall` | optional | `deny` (it may only refuse a tool call) or `decide` (it may approve one). Two levels because refusing and approving are not the same ask: a connector that only refuses is a guardrail, one that approves is answering yes on your behalf to the question the app exists to put in front of you. |
| `net` | optional | Hostnames the connector says it will contact. **A declaration, not a restriction** — nothing enforces it, and presenting it as a boundary would be worse than saying nothing. It is published because it tells a reader something true about what the connector is for. |
| `config` | optional | Fields the user fills in: `string`, `secret`, `boolean` or `number`. |
| `provides.drawers` | optional | Diagram formats the connector draws, each with a `.js`/`.mjs` entry inside the package. No segment may begin with a dot, which is what makes `..` unmatchable. |
| `displayName`, `homepage`, `keywords` | optional | Presentation. Keywords are capped at 16. |

## Working on the index

```sh
npm test          # unit tests, no network, no disk
npm run coverage  # the same, with line coverage
npm run build     # rebuild connectors.json (set GITHUB_TOKEN to avoid rate limits)
```

[`scripts/manifest.mjs`](scripts/manifest.mjs) is validation: pure functions, no
network, no clock. [`scripts/build-index.mjs`](scripts/build-index.mjs) is
everything else, and takes every side effect as an argument — which is why the
partitioned search, the release checks and the rate-limit handling are testable
without a network at all. There are no dependencies.

Two GitHub limits shape it:

- **1000 results maximum** for any one search. Hence the search is partitioned
  across disjoint star buckets, and reports `truncated` when even that was not
  enough rather than quietly returning a short list.
- **Rate limits** — ten search requests a minute unauthenticated. This is why
  the index exists as a file at all: the app used to run this search from every
  install, and a popular release spent the whole budget on the first few people
  to open the panel. With the Actions token the nightly job gets 5,000 requests
  an hour, which is what makes a per-connector release check affordable.

## License

AGPL-3.0-only. See [LICENSE](LICENSE).
