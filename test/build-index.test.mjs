// Copyright 2026 Contextual, Inc. https://agentrq.com
// This notice may not be modified or removed.
// SPDX-License-Identifier: AGPL-3.0-only

/**
 * Tests for the catalogue builder.
 *
 * Nothing here touches the network or the disk: `buildIndex` and `main` take
 * every side effect as an argument, which is the only reason the two-topic
 * search, the release checks and the rate-limit handling are testable at all.
 *
 * The first test is the important one. `repositories` is a compatibility
 * contract with `PublicList` in `crates/infrastructure/src/github_catalog.rs`,
 * and a change that breaks it would otherwise only show up as a server that
 * cannot read the file.
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { mkdtemp, readFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { execPath } from 'node:process'
import { fileURLToPath } from 'node:url'

import {
  CATALOG_TOPIC,
  EXCLUDED_FILE,
  INDEX_FILE,
  MANIFEST_FILE,
  REMOTE_FILE,
  REMOTE_TOPIC,
  SCHEMA_VERSION,
  STAR_BUCKETS,
  TOPICS,
  WEIGHTS,
  buildIndex,
  buildQuery,
  completeness,
  createFetchers,
  describeFailure,
  excludedFile,
  flagNameConflicts,
  freshness,
  isUnchanged,
  latestReleaseUrl,
  main,
  popularity,
  rank,
  rawUrl,
  readConnectors,
  repoWarnings,
  scoreEntry,
  searchRepos,
  searchUrl,
  serialize,
  toEntry,
  toRepo,
  toRow,
  verifyRelease,
} from '../scripts/build-index.mjs'

const SCRIPT_PATH = fileURLToPath(new URL('../scripts/build-index.mjs', import.meta.url))

const NOW = Date.parse('2026-10-10T00:00:00.000Z')
const daysAgo = (days) => new Date(NOW - days * 86_400_000).toISOString()

/** A search result item as GitHub returns one. */
function item(overrides = {}) {
  return {
    full_name: 'acme/gmail',
    name: 'gmail',
    owner: { login: 'acme', avatar_url: 'https://avatars.test/acme' },
    description: 'Gmail for agents',
    license: { spdx_id: 'MIT' },
    topics: [CATALOG_TOPIC, 'gmail'],
    stargazers_count: 10,
    archived: false,
    pushed_at: daysAgo(1),
    default_branch: 'main',
    html_url: 'https://github.com/acme/gmail',
    private: false,
    ...overrides,
  }
}

function manifestFor(name = 'gmail', overrides = {}) {
  return JSON.stringify({
    manifestVersion: 1,
    mcpVersion: '2026-07-28',
    name,
    title: name,
    version: '0.1.0',
    description: `Does ${name} things.`,
    instructions: `Use the ${name} tools to do ${name} things.`,
    icons: [{ src: 'data:image/svg+xml;base64,AAA', mimeType: 'image/svg+xml', sizes: ['any'] }],
    tools: [{ name: 'list' }],
    ...overrides,
  })
}

function releaseFor(names = ['gmail'], overrides = {}) {
  return {
    tag_name: 'v0.1.0',
    prerelease: false,
    published_at: daysAgo(2),
    assets: names.map((name) => ({ name: `${name}.wasm`, size: 2048, digest: `sha256:${'a'.repeat(64)}` })),
    ...overrides,
  }
}

/** An entry as `toEntry` returns one, for the scoring functions. */
function entry(overrides = {}) {
  return {
    ok: true,
    warnings: [],
    fullName: 'acme/gmail',
    stars: 10,
    pushedAt: daysAgo(1),
    description: 'Gmail for agents',
    topics: [CATALOG_TOPIC, 'gmail'],
    archived: false,
    connectors: [{ name: 'gmail', description: 'Does things.', icons: 1 }],
    bundles: [],
    ...overrides,
  }
}

/**
 * A GitHub whose searches, files and releases are described per URL.
 *
 * `totals` says how many results a query claims, which is the only way to
 * express the case that never shows up in a fixture taken from a live response:
 * a total above the 1000-result cap.
 */
function fakeGitHub({ totals = {}, itemsFor = () => [], files = {}, releases = {} } = {}) {
  const calls = []
  async function fetchJson(url) {
    if (String(url).includes('/releases/latest')) {
      calls.push({ url })
      const fullName = String(url).split('/repos/')[1].split('/releases')[0]
      return Object.prototype.hasOwnProperty.call(releases, fullName) ? releases[fullName] : null
    }
    const parsed = new URL(url)
    const query = parsed.searchParams.get('q')
    const page = Number(parsed.searchParams.get('page'))
    calls.push({ query, page })
    return { total_count: totals[query] ?? itemsFor(query, page).length, items: itemsFor(query, page) }
  }
  async function fetchText(url) {
    calls.push({ url })
    return Object.prototype.hasOwnProperty.call(files, url) ? files[url] : null
  }
  return { fetchJson, fetchText, calls }
}

const fileAt = (fullName, path, branch = 'main') => `https://raw.githubusercontent.com/${fullName}/${branch}/${path}`
const repoOf = (overrides) => toRepo(item(overrides))

test('a published row carries exactly the fields the server deserialises', () => {
  // The contract with PublicList/Repository in github_catalog.rs. Everything
  // after `private` is an extra key the server ignores, and anything that moves
  // above this line changes what the server reads.
  const row = toRow({ ...entry(), rank: 1, score: 50, repo: repoOf().repo })

  assert.deepEqual(Object.keys(row).slice(0, 9), [
    'full_name',
    'description',
    'stargazers_count',
    'html_url',
    'owner',
    'license',
    'pushed_at',
    'topics',
    'private',
  ])
  assert.deepEqual(row.owner, { avatar_url: 'https://avatars.test/acme' })
  assert.deepEqual(row.license, { spdx_id: 'MIT' })
  assert.equal(row.private, false)
})

test('a row for a remote connector carries its server instead of a release', () => {
  const row = toRow({ ...entry({ remote: { url: 'https://x.test/mcp', name: 'x' }, connectors: [] }), rank: 1, score: 1, repo: repoOf().repo })
  assert.equal(row.remote.url, 'https://x.test/mcp')
  assert.equal('release' in row, false)
})

test('a row names the repositories it collides with, only when it does', () => {
  const plain = toRow({ ...entry(), rank: 1, score: 1, repo: repoOf().repo })
  assert.equal('nameConflict' in plain, false)

  const contested = toRow({ ...entry({ nameConflict: ['other/gmail'] }), rank: 1, score: 1, repo: repoOf().repo })
  assert.deepEqual(contested.nameConflict, ['other/gmail'])
})

test('a row carries its bundles only when the repository declares some', () => {
  const bundles = [{ name: 'google', title: 'Google', description: '', connectors: ['gmail'] }]
  assert.deepEqual(toRow({ ...entry({ bundles }), rank: 1, score: 1, repo: repoOf().repo }).bundles, bundles)
})

test('buildQuery searches public repositories only, by topic or by slice', () => {
  assert.equal(buildQuery(CATALOG_TOPIC), `topic:${CATALOG_TOPIC} is:public`)
  assert.equal(buildQuery(REMOTE_TOPIC, '10..49'), `topic:${REMOTE_TOPIC} is:public stars:10..49`)
  assert.deepEqual(TOPICS, [CATALOG_TOPIC, REMOTE_TOPIC])
})

test('searchUrl asks for full pages, most starred first', () => {
  const url = new URL(searchUrl('topic:x', 3))
  assert.equal(url.origin + url.pathname, 'https://api.github.com/search/repositories')
  assert.equal(url.searchParams.get('per_page'), '100')
  assert.equal(url.searchParams.get('page'), '3')
  assert.equal(url.searchParams.get('sort'), 'stars')
})

test('rawUrl and latestReleaseUrl point at the default branch and the newest release', () => {
  assert.equal(rawUrl({ fullName: 'acme/gmail', defaultBranch: 'trunk' }, 'a/b.json'), 'https://raw.githubusercontent.com/acme/gmail/trunk/a/b.json')
  assert.equal(latestReleaseUrl('acme/gmail'), 'https://api.github.com/repos/acme/gmail/releases/latest')
})

test('toRepo keeps what the catalogue shows and what it ranks on', () => {
  const repo = repoOf()
  assert.equal(repo.fullName, 'acme/gmail')
  assert.equal(repo.owner, 'acme')
  assert.equal(repo.license, 'MIT')
  assert.equal(repo.stars, 10)
  assert.equal(repo.defaultBranch, 'main')
  assert.equal(repo.isRemote, false)
  assert.deepEqual(repo.topics, [CATALOG_TOPIC, 'gmail'].sort())
})

test('toRepo fills in what a sparse search result leaves out', () => {
  const { repo, ...flat } = toRepo({ full_name: 'acme/bare' })
  assert.equal(flat.name, 'bare')
  assert.equal(flat.stars, 0)
  assert.equal(flat.defaultBranch, 'main')
  assert.equal(repo.description, null, 'the server reads a missing description as null, not ""')
  assert.equal(repo.owner, null)
  assert.equal(repo.license, null)
  assert.equal(repo.pushed_at, null)
  assert.equal(repo.html_url, 'https://github.com/acme/bare')
})

test('toRepo drops a licence GitHub could not identify', () => {
  assert.equal(repoOf({ license: { spdx_id: 'NOASSERTION' } }).license, '')
  assert.equal(repoOf({ license: null }).license, '')
})

test('toRepo survives a result with nothing in it at all', () => {
  assert.equal(toRepo(undefined).fullName, '')
})

test('a repository with only the remote topic is a remote connector', () => {
  assert.equal(repoOf({ topics: [REMOTE_TOPIC] }).isRemote, true)
  assert.equal(repoOf({ topics: [REMOTE_TOPIC, CATALOG_TOPIC] }).isRemote, false, 'a repository that ships a component is one')
})

test('searchRepos runs one query per topic when everything fits under the cap', async () => {
  const github = fakeGitHub({ itemsFor: (query) => (query.startsWith(`topic:${CATALOG_TOPIC}`) ? [item()] : []) })
  const { repos, truncated } = await searchRepos(github.fetchJson)

  assert.equal(repos.length, 1)
  assert.equal(truncated, false)
  assert.deepEqual(github.calls.map((c) => c.query), [buildQuery(CATALOG_TOPIC), buildQuery(REMOTE_TOPIC)])
})

test('searchRepos pages through a topic, reusing the response that counted it', async () => {
  const totals = { [buildQuery(CATALOG_TOPIC)]: 150 }
  const github = fakeGitHub({
    totals,
    itemsFor: (query, page) =>
      query.startsWith(`topic:${CATALOG_TOPIC}`)
        ? Array.from({ length: page === 1 ? 100 : 50 }, (_, i) => item({ full_name: `acme/c${page}-${i}` }))
        : [],
  })

  const { repos } = await searchRepos(github.fetchJson)
  assert.equal(repos.length, 150)
})

test('searchRepos partitions a topic by stars once it outgrows the cap', async () => {
  const totals = { [buildQuery(CATALOG_TOPIC)]: 4000 }
  for (const bucket of STAR_BUCKETS) totals[buildQuery(CATALOG_TOPIC, bucket)] = 1
  const github = fakeGitHub({
    totals,
    itemsFor: (query) => (query.includes('stars:') ? [item({ full_name: `acme/${query.slice(-3)}` })] : []),
  })

  const { repos, truncated } = await searchRepos(github.fetchJson)
  assert.equal(truncated, false)
  assert.equal(repos.length, STAR_BUCKETS.length)
})

test('searchRepos reports truncation rather than hiding it', async () => {
  const totals = { [buildQuery(CATALOG_TOPIC)]: 40_000, [buildQuery(CATALOG_TOPIC, '0')]: 2000 }
  for (const bucket of STAR_BUCKETS) totals[buildQuery(CATALOG_TOPIC, bucket)] ??= 0
  const github = fakeGitHub({ totals, itemsFor: (query, page) => (query.endsWith('stars:0') ? [item({ full_name: `acme/z${page}` })] : []) })

  const { repos, truncated } = await searchRepos(github.fetchJson)
  assert.equal(truncated, true)
  assert.equal(repos.length, 10, 'only the reachable thousand, in ten pages of one')
})

test('searchRepos keeps one row for a repository carrying both topics', async () => {
  const github = fakeGitHub({ itemsFor: () => [item({ topics: [CATALOG_TOPIC, REMOTE_TOPIC] })] })
  const { repos } = await searchRepos(github.fetchJson)

  assert.equal(repos.length, 1)
  assert.deepEqual(repos[0].topics, [CATALOG_TOPIC, REMOTE_TOPIC].sort())
  assert.equal(repos[0].isRemote, false)
})

test('searchRepos unions the topics when the two searches disagree', async () => {
  // A repository seen as remote-only in one pass and as a component in the
  // other must end up as both, or whichever search ran last decides what it is.
  const github = fakeGitHub({
    itemsFor: (query) =>
      query.startsWith(`topic:${CATALOG_TOPIC}`) ? [item({ topics: [CATALOG_TOPIC] })] : [item({ topics: [REMOTE_TOPIC] })],
  })
  const { repos } = await searchRepos(github.fetchJson)

  assert.equal(repos.length, 1)
  assert.deepEqual(repos[0].topics, [CATALOG_TOPIC, REMOTE_TOPIC].sort())
  assert.equal(repos[0].isRemote, false)
})

test('searchRepos leaves out a private repository GitHub returned anyway', async () => {
  const github = fakeGitHub({ itemsFor: () => [item({ private: true }), item({ full_name: 'acme/public' })] })
  const { repos } = await searchRepos(github.fetchJson)
  assert.deepEqual(repos.map((r) => r.fullName), ['acme/public'])
})

test('searchRepos treats a response with no body as no results', async () => {
  const { repos } = await searchRepos(async () => null)
  assert.deepEqual(repos, [])
})

test('readConnectors reads the root manifest when there is no index', async () => {
  const github = fakeGitHub({ files: { [fileAt('acme/gmail', MANIFEST_FILE)]: manifestFor('gmail') } })
  const read = await readConnectors(repoOf(), github.fetchText)

  assert.equal(read.ok, true)
  assert.deepEqual(read.connectors.map((c) => c.name), ['gmail'])
  assert.equal(read.connectors[0].path, MANIFEST_FILE)
})

test('readConnectors follows the index when a repository publishes several', async () => {
  const github = fakeGitHub({
    files: {
      [fileAt('acme/gmail', INDEX_FILE)]: JSON.stringify({
        manifestVersion: 1,
        connectors: [{ manifest: 'drive/manifest.json' }, { manifest: 'gmail/manifest.json' }],
        bundles: [{ name: 'google', connectors: ['gmail', 'drive'] }],
      }),
      [fileAt('acme/gmail', 'gmail/manifest.json')]: manifestFor('gmail'),
      [fileAt('acme/gmail', 'drive/manifest.json')]: manifestFor('drive'),
    },
  })
  const read = await readConnectors(repoOf(), github.fetchText)

  assert.deepEqual(read.connectors.map((c) => c.name), ['drive', 'gmail'])
  assert.deepEqual(read.bundles[0].connectors, ['gmail', 'drive'])
  assert.deepEqual(read.warnings, [])
})

test('readConnectors warns about a bundle naming a connector the repository does not publish', async () => {
  const github = fakeGitHub({
    files: {
      [fileAt('acme/gmail', INDEX_FILE)]: JSON.stringify({
        manifestVersion: 1,
        connectors: [{ manifest: 'gmail/manifest.json' }],
        bundles: [{ name: 'google', connectors: ['gmail', 'drive'] }],
      }),
      [fileAt('acme/gmail', 'gmail/manifest.json')]: manifestFor('gmail'),
    },
  })
  const read = await readConnectors(repoOf(), github.fetchText)
  assert.match(read.warnings.join('\n'), /the bundle "google" lists drive/)
})

test('readConnectors passes on the reason a document is broken', async () => {
  const badIndex = fakeGitHub({ files: { [fileAt('acme/gmail', INDEX_FILE)]: '{' } })
  assert.match((await readConnectors(repoOf(), badIndex.fetchText)).reason, /not valid JSON/)

  const noManifest = fakeGitHub({ files: {} })
  assert.match((await readConnectors(repoOf(), noManifest.fetchText)).reason, new RegExp(`No ${MANIFEST_FILE}`))
})

test('readConnectors refuses two manifests claiming one name', async () => {
  const github = fakeGitHub({
    files: {
      [fileAt('acme/gmail', INDEX_FILE)]: JSON.stringify({
        manifestVersion: 1,
        connectors: [{ manifest: 'a/manifest.json' }, { manifest: 'b/manifest.json' }],
      }),
      [fileAt('acme/gmail', 'a/manifest.json')]: manifestFor('gmail'),
      [fileAt('acme/gmail', 'b/manifest.json')]: manifestFor('gmail'),
    },
  })
  assert.match((await readConnectors(repoOf(), github.fetchText)).reason, /both named "gmail"/)
})

test('verifyRelease confirms the component is there to install', async () => {
  const github = fakeGitHub({ releases: { 'acme/gmail': releaseFor(['gmail']) } })
  const result = await verifyRelease(repoOf(), [{ name: 'gmail' }], github.fetchJson, { several: false })

  assert.equal(result.ok, true)
  assert.deepEqual(result.warnings, [])
  assert.equal(result.release.tag, 'v0.1.0')
  assert.equal(result.release.assets[0].name, 'gmail.wasm')
})

test('verifyRelease refuses a repository with no release, or no component in it', async () => {
  const none = fakeGitHub({ releases: {} })
  assert.match((await verifyRelease(repoOf(), [], none.fetchJson, { several: false })).reason, /No published release/)

  const empty = fakeGitHub({ releases: { 'acme/gmail': releaseFor([], { assets: [{ name: 'notes.txt' }] }) } })
  assert.match((await verifyRelease(repoOf(), [], empty.fetchJson, { several: false })).reason, /has no .wasm asset/)
})

test('verifyRelease requires an asset named after each connector when there are several', async () => {
  const github = fakeGitHub({ releases: { 'acme/gmail': releaseFor(['gmail']) } })
  const result = await verifyRelease(repoOf(), [{ name: 'gmail' }, { name: 'drive' }], github.fetchJson, { several: true })

  assert.equal(result.ok, false)
  assert.match(result.reason, /missing "drive.wasm"/)
})

test('verifyRelease takes any component name for a single connector', async () => {
  const github = fakeGitHub({ releases: { 'acme/gmail': releaseFor([], { assets: [{ name: 'anything.wasm', digest: 'sha256:x' }] }) } })
  assert.equal((await verifyRelease(repoOf(), [{ name: 'gmail' }], github.fetchJson, { several: false })).ok, true)
})

test('verifyRelease warns about an asset an install cannot verify', async () => {
  const github = fakeGitHub({ releases: { 'acme/gmail': releaseFor([], { assets: [{ name: 'gmail.wasm' }], prerelease: true }) } })
  const result = await verifyRelease(repoOf(), [{ name: 'gmail' }], github.fetchJson, { several: false })

  assert.equal(result.ok, true)
  assert.match(result.warnings.join('\n'), /no digest for "gmail.wasm"/)
  assert.match(result.warnings.join('\n'), /pre-release/)
})

test('verifyRelease downgrades a failed check to a warning', async () => {
  // A timeout says nothing about the release. Dropping a working connector out
  // of the catalogue on a bad network day would be the worse answer.
  const result = await verifyRelease(repoOf(), [], async () => {
    throw new Error('fetch failed')
  }, { several: false })

  assert.equal(result.ok, true)
  assert.match(result.warnings[0], /Could not check the latest release: fetch failed/)
})

test('repoWarnings says when a repository is archived or says nothing about itself', () => {
  assert.deepEqual(repoWarnings({ archived: false, description: 'x' }), [])
  assert.match(repoWarnings({ archived: true, description: 'x' })[0], /archived/)
  assert.match(repoWarnings({ archived: false, description: '' })[0], /no description/)
})

test('toEntry publishes a wasm connector whose documents and release check out', async () => {
  const github = fakeGitHub({
    files: { [fileAt('acme/gmail', MANIFEST_FILE)]: manifestFor('gmail') },
    releases: { 'acme/gmail': releaseFor(['gmail']) },
  })
  const result = await toEntry(repoOf(), github)

  assert.equal(result.ok, true)
  assert.deepEqual(result.warnings, [])
  assert.equal(result.connectors[0].title, 'gmail')
  assert.equal(result.release.tag, 'v0.1.0')
})

test('toEntry excludes a wasm connector the catalogue does not accept the licence of', async () => {
  const github = fakeGitHub({ files: { [fileAt('acme/gmail', MANIFEST_FILE)]: manifestFor('gmail') } })
  for (const license of [{ spdx_id: 'GPL-3.0' }, null]) {
    const result = await toEntry(repoOf({ license }), github)
    assert.equal(result.ok, false)
    assert.match(result.reason, /catalogue lists wasm connectors under/)
  }
})

test('toEntry accepts every licence the policy names', async () => {
  const github = fakeGitHub({
    files: { [fileAt('acme/gmail', MANIFEST_FILE)]: manifestFor('gmail') },
    releases: { 'acme/gmail': releaseFor(['gmail']) },
  })
  for (const id of ['MIT', 'Apache-2.0', 'AGPL-3.0', 'AGPL-3.0-only', 'AGPL-3.0-or-later']) {
    const result = await toEntry(repoOf({ license: { spdx_id: id } }), github)
    assert.equal(result.ok, true, id)
  }
})

test('the licence policy does not apply to a remote MCP server', async () => {
  // There is no code to be licensed: the repository is a pointer to somebody's
  // hosted server, and refusing it over its own licence would be refusing to
  // list the service.
  const github = fakeGitHub({
    files: { [fileAt('acme/gmail', REMOTE_FILE)]: JSON.stringify({ url: 'https://mcp.acme.test/mcp', name: 'acme', description: 'x' }) },
  })
  const result = await toEntry(repoOf({ topics: [REMOTE_TOPIC], license: { spdx_id: 'GPL-3.0' } }), github)

  assert.equal(result.ok, true)
  assert.equal(result.remote.url, 'https://mcp.acme.test/mcp')
  assert.deepEqual(result.connectors, [])
})

test('toEntry excludes a remote repository with no remote file', async () => {
  const github = fakeGitHub({ files: {} })
  const result = await toEntry(repoOf({ topics: [REMOTE_TOPIC] }), github)

  assert.equal(result.ok, false)
  assert.match(result.reason, new RegExp(`No ${REMOTE_FILE}`))
})

test('toEntry keeps what it managed to read when the release is the problem', async () => {
  const github = fakeGitHub({ files: { [fileAt('acme/gmail', MANIFEST_FILE)]: manifestFor('gmail') }, releases: {} })
  const result = await toEntry(repoOf(), github)

  assert.equal(result.ok, false)
  assert.match(result.reason, /No published release/)
  assert.equal(result.connectors[0].name, 'gmail', 'so the author can see what was read of theirs')
})

test('toEntry excludes a connector with no icon or no instructions, naming the rule', async () => {
  const noIcons = fakeGitHub({
    files: { [fileAt('acme/gmail', MANIFEST_FILE)]: manifestFor('gmail', { icons: [] }) },
    releases: { 'acme/gmail': releaseFor(['gmail']) },
  })
  const withoutIcons = await toEntry(repoOf(), noIcons)
  assert.equal(withoutIcons.ok, false)
  assert.match(withoutIcons.reason, /"icons" is required/)

  const noInstructions = fakeGitHub({
    files: { [fileAt('acme/gmail', MANIFEST_FILE)]: manifestFor('gmail', { instructions: '' }) },
    releases: { 'acme/gmail': releaseFor(['gmail']) },
  })
  const withoutInstructions = await toEntry(repoOf(), noInstructions)
  assert.equal(withoutInstructions.ok, false)
  assert.match(withoutInstructions.reason, /"instructions" is required/)
})

test('toEntry reports every connector that fails the listing bar, not just the first', async () => {
  const github = fakeGitHub({
    files: {
      [fileAt('acme/suite', INDEX_FILE)]: JSON.stringify({
        manifestVersion: 1,
        connectors: [{ manifest: 'a/manifest.json' }, { manifest: 'b/manifest.json' }],
      }),
      [fileAt('acme/suite', 'a/manifest.json')]: manifestFor('gmail', { icons: [] }),
      [fileAt('acme/suite', 'b/manifest.json')]: manifestFor('drive', { instructions: '' }),
    },
    releases: { 'acme/suite': releaseFor(['gmail', 'drive']) },
  })
  const result = await toEntry(repoOf({ full_name: 'acme/suite', name: 'suite' }), github)

  assert.equal(result.ok, false)
  assert.match(result.reason, /a\/manifest\.json.*"icons" is required/)
  assert.match(result.reason, /b\/manifest\.json.*"instructions" is required/)
})

test('toEntry gathers warnings from the documents and the repository', async () => {
  const github = fakeGitHub({
    files: { [fileAt('acme/gmail', MANIFEST_FILE)]: manifestFor('gmail', { extra: 1 }) },
    releases: { 'acme/gmail': releaseFor([], { assets: [{ name: 'gmail.wasm' }] }) },
  })
  const result = await toEntry(repoOf({ archived: true }), github)

  const text = result.warnings.join('\n')
  assert.equal(result.ok, true)
  assert.match(text, /a key the server does not know/)
  assert.match(text, /no digest/)
  assert.match(text, /archived/)
})

test('popularity is a log curve, flattening past the ceiling', () => {
  assert.equal(popularity(0), 0)
  assert.equal(popularity(10_000), 1)
  assert.equal(popularity(50_000), 1)
  assert.equal(popularity(undefined), 0)
  assert.ok(popularity(50) - popularity(0) > popularity(10_000) - popularity(5000))
})

test('freshness halves over the half-life', () => {
  assert.equal(Math.round(freshness(daysAgo(0), NOW) * 100) / 100, 1)
  assert.equal(Math.round(freshness(daysAgo(90), NOW) * 100) / 100, 0.5)
  assert.equal(freshness('', NOW), 0)
  assert.equal(freshness(daysAgo(-30), NOW), 1)
})

test('completeness rewards a row that introduces itself', () => {
  assert.equal(completeness(entry()), 1)
  assert.equal(
    completeness(entry({ warnings: ['x'], description: '', topics: [CATALOG_TOPIC], archived: true, connectors: [{ name: 'g', description: '', icons: 0 }] })),
    0,
  )
})

test('completeness counts a remote server as having both a description and an icon', () => {
  const remote = entry({ connectors: [], remote: { url: 'https://x.test', description: 'x' } })
  assert.equal(completeness(remote), 1)
})

test('scoreEntry sums the weights into a readable percent', () => {
  assert.equal(scoreEntry(entry({ stars: 10_000, pushedAt: daysAgo(0) }), NOW), 100)
  assert.deepEqual(WEIGHTS, { popularity: 60, freshness: 30, completeness: 10 })
})

test('rank numbers the catalogue, and breaks ties the same way every run', () => {
  const entries = [entry({ fullName: 'z/one' }), entry({ fullName: 'a/one' }), entry({ fullName: 'b/two', stars: 500 })]
  const ranked = rank(entries, NOW)

  assert.deepEqual(ranked.map((e) => e.fullName), ['b/two', 'a/one', 'z/one'])
  assert.deepEqual(ranked.map((e) => e.rank), [1, 2, 3])
  assert.deepEqual(rank([...entries].reverse(), NOW).map((e) => e.fullName), ['b/two', 'a/one', 'z/one'])
})

test('flagNameConflicts names both claimants and drops neither', () => {
  const entries = [
    entry({ fullName: 'acme/gmail' }),
    entry({ fullName: 'other/gmail' }),
    entry({ fullName: 'acme/drive', connectors: [{ name: 'drive' }] }),
    entry({ fullName: 'acme/broken', ok: false, connectors: [] }),
  ]
  const flagged = flagNameConflicts(entries)

  assert.deepEqual(flagged[0].nameConflict, ['other/gmail'])
  assert.match(flagged[0].warnings[0], /The name "gmail" is also claimed by other\/gmail/)
  assert.equal(flagged[2].nameConflict, undefined)
  assert.equal(flagged[3].nameConflict, undefined, 'a repository publishing nothing claims no name')
  assert.equal(flagged.length, 4)
})

test('flagNameConflicts reports every contested name of a repository publishing several', () => {
  const entries = [
    entry({ fullName: 'acme/suite', connectors: [{ name: 'gmail' }, { name: 'drive' }] }),
    entry({ fullName: 'other/gmail', connectors: [{ name: 'gmail' }] }),
    entry({ fullName: 'other/drive', connectors: [{ name: 'drive' }] }),
  ]
  const flagged = flagNameConflicts(entries)

  assert.deepEqual(flagged[0].nameConflict, ['other/gmail', 'other/drive'])
  assert.match(flagged[0].warnings[0], /The names "gmail", "drive" are also claimed/)
})

test('flagNameConflicts counts a remote connector by the name it installs as', () => {
  const entries = [
    entry({ fullName: 'acme/gmail' }),
    entry({ fullName: 'other/remote', connectors: [], remote: { url: 'https://x.test', name: 'gmail' } }),
  ]
  assert.deepEqual(flagNameConflicts(entries)[1].nameConflict, ['acme/gmail'])
})

test('buildIndex publishes what the server reads, and what it ignores', async () => {
  const repos = [item(), item({ full_name: 'acme/drive', name: 'drive', stargazers_count: 500 })]
  const github = fakeGitHub({
    itemsFor: (query) => (query.startsWith(`topic:${CATALOG_TOPIC}`) ? repos : []),
    files: {
      [fileAt('acme/gmail', MANIFEST_FILE)]: manifestFor('gmail'),
      [fileAt('acme/drive', MANIFEST_FILE)]: manifestFor('drive'),
    },
    releases: { 'acme/gmail': releaseFor(['gmail']), 'acme/drive': releaseFor(['drive']) },
  })

  const { index, excluded } = await buildIndex({ ...github, now: () => NOW, concurrency: 1 })
  assert.equal(index.schemaVersion, SCHEMA_VERSION)
  assert.equal(index.generatedAt, '2026-10-10T00:00:00.000Z')
  assert.deepEqual(index.topics, TOPICS)
  assert.deepEqual(index.counts, { found: 2, listed: 2, excluded: 0, warned: 0 })
  assert.deepEqual(index.repositories.map((r) => r.full_name), ['acme/drive', 'acme/gmail'])
  assert.equal(index.excluded, EXCLUDED_FILE, 'the catalogue points at the companion file, it does not carry the rows')
  assert.deepEqual(excluded, [])
  assert.match(index.$comment, /do not edit/)
})

test('buildIndex lists an excluded repository with the reason, outside the catalogue', async () => {
  const github = fakeGitHub({
    itemsFor: (query) => (query.startsWith(`topic:${CATALOG_TOPIC}`) ? [item({ license: { spdx_id: 'GPL-3.0' } })] : []),
  })
  const { index, excluded } = await buildIndex({ ...github, now: () => NOW })

  assert.deepEqual(index.repositories, [], 'the server must not offer a row it cannot install')
  assert.equal(index.excluded, EXCLUDED_FILE)
  assert.equal(index.counts.excluded, 1)
  assert.deepEqual(excluded[0].full_name, 'acme/gmail')
  assert.equal(excluded[0].license, 'GPL-3.0')
  assert.match(excluded[0].reason, /Licensed as "GPL-3.0"/)
})

test('buildIndex counts the rows carrying a warning', async () => {
  const github = fakeGitHub({
    itemsFor: (query) => (query.startsWith(`topic:${CATALOG_TOPIC}`) ? [item({ archived: true })] : []),
    files: { [fileAt('acme/gmail', MANIFEST_FILE)]: manifestFor('gmail') },
    releases: { 'acme/gmail': releaseFor(['gmail']) },
  })
  const { index } = await buildIndex({ ...github, now: () => NOW })

  assert.equal(index.counts.listed, 1)
  assert.equal(index.counts.warned, 1)
  assert.match(index.repositories[0].warnings[0], /archived/)
})

test('buildIndex works through a long list in bounded batches', async () => {
  const repos = Array.from({ length: 13 }, (_, i) => item({ full_name: `acme/c${i}`, name: `c${i}` }))
  const github = fakeGitHub({ itemsFor: (query) => (query.startsWith(`topic:${CATALOG_TOPIC}`) ? repos : []) })
  let inFlight = 0
  let peak = 0

  const { index } = await buildIndex({
    fetchJson: github.fetchJson,
    fetchText: async (url) => {
      inFlight += 1
      peak = Math.max(peak, inFlight)
      await new Promise((resolve) => setImmediate(resolve))
      inFlight -= 1
      return url.endsWith(MANIFEST_FILE) ? manifestFor(url.split('/')[4]) : null
    },
    now: () => NOW,
    concurrency: 4,
  })

  assert.equal(index.counts.found, 13)
  assert.ok(peak <= 4, `expected at most 4 concurrent reads, saw ${peak}`)
})

test('buildIndex carries truncation through to the published file', async () => {
  const totals = { [buildQuery(CATALOG_TOPIC)]: 40_000, [buildQuery(CATALOG_TOPIC, '0')]: 2000 }
  for (const bucket of STAR_BUCKETS) totals[buildQuery(CATALOG_TOPIC, bucket)] ??= 0
  const github = fakeGitHub({ totals, itemsFor: () => [] })

  assert.equal((await buildIndex({ ...github, now: () => NOW })).index.truncated, true)
})

test('isUnchanged ignores the timestamp, and nothing else', () => {
  const index = { generatedAt: 'a', counts: { listed: 1 }, repositories: [] }
  assert.equal(isUnchanged({ ...index, generatedAt: 'b' }, index), true)
  assert.equal(isUnchanged({ ...index, counts: { listed: 2 } }, index), false)
  assert.equal(isUnchanged(null, index), false)
})

test('serialize writes a readable file that ends in a newline', () => {
  assert.equal(serialize({ count: 0 }), '{\n  "count": 0\n}\n')
})

test('describeFailure finds a message in whatever it is handed', () => {
  assert.equal(describeFailure(new Error('boom')), 'boom')
  assert.equal(describeFailure('boom'), 'boom')
  assert.equal(describeFailure(null), 'Unknown error')
})

test('createFetchers sends the token and the API version', async () => {
  const seen = []
  const fetchImpl = async (url, options) => {
    seen.push({ url, headers: options.headers })
    return { ok: true, status: 200, json: async () => ({ total_count: 0 }), text: async () => 'body' }
  }
  const { fetchJson, fetchText } = createFetchers('tok', fetchImpl)

  assert.deepEqual(await fetchJson('https://api.github.com/x'), { total_count: 0 })
  assert.equal(await fetchText('https://raw.githubusercontent.com/x'), 'body')
  assert.equal(seen[0].headers.authorization, 'Bearer tok')
  assert.equal(seen[0].headers['x-github-api-version'], '2022-11-28')
  assert.equal(seen[1].headers.accept, 'text/plain')
})

test('createFetchers sends no authorization header without a token', async () => {
  let headers
  await createFetchers('', async (url, options) => {
    headers = options.headers
    return { ok: true, status: 200, json: async () => ({}) }
  }).fetchJson('https://api.github.com/x')
  assert.equal('authorization' in headers, false)
})

test('createFetchers reads a 404 as absence, not failure', async () => {
  const fetchImpl = async () => ({ ok: false, status: 404, statusText: 'Not Found' })
  const { fetchJson, fetchText } = createFetchers('tok', fetchImpl)
  assert.equal(await fetchJson('https://api.github.com/x'), null)
  assert.equal(await fetchText('https://raw.githubusercontent.com/x'), null)
})

test('createFetchers throws on anything else, naming the status and the url', async () => {
  const fetchImpl = async () => ({ ok: false, status: 403, statusText: 'rate limit exceeded' })
  await assert.rejects(
    () => createFetchers('tok', fetchImpl).fetchJson('https://api.github.com/x'),
    /403 rate limit exceeded for https:\/\/api.github.com\/x/,
  )
})

test('main writes the catalogue and reports the change', async () => {
  const github = fakeGitHub({ itemsFor: (query) => (query.startsWith(`topic:${CATALOG_TOPIC}`) ? [item()] : []) })
  const written = []
  const outputs = []
  const logs = []

  const { changed, index, excluded } = await main({
    fetchers: { ...github, now: () => NOW },
    readPrevious: async () => {
      throw new Error('ENOENT')
    },
    readPreviousExcluded: async () => {
      throw new Error('ENOENT')
    },
    write: async (contents) => written.push(contents),
    writeExcluded: async (contents) => written.push(contents),
    setOutput: async (value) => outputs.push(value),
    log: (line) => logs.push(line),
  })

  assert.equal(changed, true)
  assert.equal(written.length, 2, 'the catalogue and its companion are written together')
  const catalogue = JSON.parse(written[0])
  const companion = JSON.parse(written[1])
  assert.equal(catalogue.counts.excluded, 1)
  assert.equal(catalogue.excluded, EXCLUDED_FILE)
  assert.equal(companion.excluded.length, 1)
  assert.equal(companion.generatedAt, catalogue.generatedAt, 'the two files are one build')
  assert.deepEqual(outputs, [true])
  assert.match(logs[0], /0 listed \(0 with warnings\), 1 excluded/)
  assert.equal(index.counts.found, 1)
  assert.equal(excluded.length, 1)
})

test('main leaves the file alone when nothing moved', async () => {
  const github = fakeGitHub({ itemsFor: (query) => (query.startsWith(`topic:${CATALOG_TOPIC}`) ? [item()] : []) })
  const fetchers = { ...github, now: () => NOW }
  const first = await main({
    fetchers,
    readPrevious: async () => '',
    readPreviousExcluded: async () => '',
    write: async () => {},
    writeExcluded: async () => {},
    setOutput: async () => {},
    log: () => {},
  })

  const written = []
  const writtenExcluded = []
  const logs = []
  const { changed } = await main({
    fetchers,
    // A day later: the same catalogue, a new timestamp, and nothing to commit.
    readPrevious: async () => serialize({ ...first.index, generatedAt: '2026-10-09T00:00:00.000Z' }),
    readPreviousExcluded: async () => serialize(excludedFile(first.excluded, '2026-10-09T00:00:00.000Z')),
    write: async (contents) => written.push(contents),
    writeExcluded: async (contents) => writtenExcluded.push(contents),
    setOutput: async () => {},
    log: (line) => logs.push(line),
  })

  assert.equal(changed, false)
  assert.deepEqual(written, [], 'a rewritten file would be a commit saying nothing happened')
  assert.deepEqual(writtenExcluded, [], 'same for the companion file')
  assert.match(logs[0], /No change/)
})

test('main says so out loud when the catalogue is incomplete', async () => {
  const totals = { [buildQuery(CATALOG_TOPIC)]: 40_000, [buildQuery(CATALOG_TOPIC, '0')]: 2000 }
  for (const bucket of STAR_BUCKETS) totals[buildQuery(CATALOG_TOPIC, bucket)] ??= 0
  const github = fakeGitHub({ totals, itemsFor: () => [] })
  const logs = []

  await main({
    fetchers: { ...github, now: () => NOW },
    readPrevious: async () => '',
    readPreviousExcluded: async () => '',
    write: async () => {},
    writeExcluded: async () => {},
    setOutput: async () => {},
    log: (line) => logs.push(line),
  })
  assert.match(logs[0], /incomplete/)
})

test('main hands the result to the Actions step that decides whether to commit', async () => {
  const github = fakeGitHub({ itemsFor: () => [] })
  const outputPath = join(await mkdtemp(join(tmpdir(), 'connectors-')), 'github-output')
  process.env.GITHUB_OUTPUT = outputPath
  try {
    await main({
      fetchers: { ...github, now: () => NOW },
      readPrevious: async () => '',
      readPreviousExcluded: async () => '',
      write: async () => {},
      writeExcluded: async () => {},
      log: () => {},
    })
    assert.equal(await readFile(outputPath, 'utf8'), 'changed=true\n')
  } finally {
    delete process.env.GITHUB_OUTPUT
  }
})

test('main writes nothing to an Actions output when it is not running in one', async () => {
  const github = fakeGitHub({ itemsFor: () => [] })
  delete process.env.GITHUB_OUTPUT
  const { changed } = await main({
    fetchers: { ...github, now: () => NOW },
    readPrevious: async () => '',
    readPreviousExcluded: async () => '',
    write: async () => {},
    writeExcluded: async () => {},
    log: () => {},
  })
  assert.equal(changed, true)
})

/** Runs the file as the program it becomes on the runner, with no network. */
function runCli(cwd, env = {}) {
  return new Promise((resolve) => {
    const child = spawn(
      execPath,
      ['--import', fileURLToPath(new URL('./fixtures/offline-github.mjs', import.meta.url)), SCRIPT_PATH],
      { cwd, env: { ...process.env, GITHUB_OUTPUT: '', ...env } },
    )
    let out = ''
    child.stdout.on('data', (chunk) => (out += chunk))
    child.stderr.on('data', (chunk) => (out += chunk))
    child.on('close', (code) => resolve({ code, out }))
  })
}

test('run as a program, it writes connectors.json into the working directory', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'connectors-cli-'))
  const { code, out } = await runCli(cwd)

  assert.equal(code, 0, out)
  const index = JSON.parse(await readFile(join(cwd, 'connectors.json'), 'utf8'))
  assert.deepEqual(index.topics, TOPICS)
  assert.deepEqual(index.repositories, [])
  assert.equal(index.excluded, EXCLUDED_FILE)
  const companion = JSON.parse(await readFile(join(cwd, 'excluded-connectors.json'), 'utf8'))
  assert.deepEqual(companion.excluded, [])
  assert.equal(companion.generatedAt, index.generatedAt, 'the two files are one build')
})

test('run as a program, it fails loudly when GitHub cannot be reached', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'connectors-cli-'))
  const { code, out } = await runCli(cwd, { OFFLINE_GITHUB_FAIL: '1' })

  assert.equal(code, 1, 'a schedule that cannot reach GitHub has to go red')
  assert.match(out, /fetch failed/)
})
