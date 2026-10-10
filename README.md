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
  "ok": 39,                  // of those, how many have a manifest that parses
  "weights": { "popularity": 60, "freshness": 30, "completeness": 10 },
  "connectors": [
    {
      "rank": 1,             // 1-based, in the published order
      "score": 83.71,        // 0-100, see below
      "ok": true,            // false when the manifest is missing or broken
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
      "manifest": {          // present when ok; see below
        "name": "jira-connector",
        "displayName": "Jira",
        "version": "1.2.0",
        "description": "Jira issues, as agent tools.",
        "license": "MIT",
        "homepage": "",
        "keywords": ["jira", "issues"]
      }
    },
    {
      "rank": 42,
      "ok": false,
      "reason": "\"version\" must look like \"1.4.0\". Got \"latest\".",
      "fullName": "acme/half-finished"
      // ...the same repository fields as above
    }
  ]
}
```

A repository with the topic but a manifest that does not parse is **listed as
broken with the reason**, not dropped. The author needs to see why — a silently
missing connector is undebuggable from the outside — and anyone following a link
to it needs the index to explain itself rather than look empty.

## Ranking

Each connector scores out of 100, from three signals:

| Signal | Weight | How it is measured |
| --- | --- | --- |
| **Popularity** | 60 | `log10(stars + 1)` against a 10,000-star ceiling. Log, not linear, because the distance from 0 to 50 stars is the meaningful one and linearly it would be invisible next to 5,000 → 10,000. |
| **Freshness** | 30 | Half-life decay on `pushedAt`, halving every 90 days. |
| **Completeness** | 10 | Six checks: the manifest parses, and the listing has a description, a license, a homepage, at least one other topic, and is not archived. |

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
2. Commit an `agentrq-connector.json` at the root of its default branch:

```json
{
  "name": "jira-connector",
  "displayName": "Jira",
  "version": "1.2.0",
  "description": "Jira issues, as agent tools.",
  "license": "MIT",
  "homepage": "https://github.com/acme/jira-connector",
  "keywords": ["jira", "issues"]
}
```

`name` is an address rather than a label — lowercase words joined by single
hyphens — because it ends up in routes and directory names. `license` is
required: a connector runs with real access, so the terms it is offered under
are part of the decision to install it. Use an SPDX identifier, or `UNLICENSED`
if no permission is granted.

The index checks only those four fields plus the shape of the file. The app
validates the rest at install time, against the version of itself that is
actually running — rules enforced here by a nightly job could declare a
connector broken for reasons no user's install agrees with.

## Working on the index

```sh
npm test          # unit tests, no network, no disk
npm run coverage  # the same, with line coverage
npm run build     # rebuild connectors.json (set GITHUB_TOKEN to avoid rate limits)
```

[`scripts/build-index.mjs`](scripts/build-index.mjs) is the whole thing, with no
dependencies. Everything that touches the network is injected, which is why the
partitioned search and the rate-limit handling are testable at all.

Two GitHub limits shape it:

- **1000 results maximum** for any one search. Hence the search is partitioned
  across disjoint star buckets, and reports `truncated` when even that was not
  enough rather than quietly returning a short list.
- **Rate limits** — ten search requests a minute unauthenticated. This is why
  the index exists as a file at all: the app used to run this search from every
  install, and a popular release spent the whole budget on the first few people
  to open the panel.

## License

GPL-3.0-only. See [LICENSE](LICENSE).
