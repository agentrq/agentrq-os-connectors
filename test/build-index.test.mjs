// Copyright 2026 Contextual, Inc. https://agentrq.com
// This notice may not be modified or removed.

/**
 * Tests for the index builder.
 *
 * Nothing here touches the network or the disk: `buildIndex` and `main` take
 * every side effect as an argument, which is the only reason the partitioned
 * search and the rate-limit handling are testable at all.
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
  MANIFEST_FILE,
  SCHEMA_VERSION,
  STAR_BUCKETS,
  TOPIC,
  WEIGHTS,
  buildIndex,
  buildQuery,
  completeness,
  createFetchers,
  describeFailure,
  freshness,
  isUnchanged,
  main,
  manifestUrl,
  parseManifest,
  popularity,
  rank,
  scoreEntry,
  searchRepos,
  searchUrl,
  serialize,
  toRepo,
} from '../scripts/build-index.mjs'

const SCRIPT_PATH = fileURLToPath(new URL('../scripts/build-index.mjs', import.meta.url))

const NOW = Date.parse('2026-10-10T00:00:00.000Z')
const daysAgo = (days) => new Date(NOW - days * 86_400_000).toISOString()

/** A search result item as GitHub returns one. */
function item(overrides = {}) {
  return {
    full_name: 'acme/jira',
    name: 'jira',
    owner: { login: 'acme' },
    description: 'Jira for agents',
    homepage: 'https://acme.test',
    license: { spdx_id: 'MIT' },
    topics: [TOPIC, 'jira'],
    stargazers_count: 10,
    forks_count: 2,
    open_issues_count: 1,
    archived: false,
    created_at: daysAgo(400),
    pushed_at: daysAgo(1),
    default_branch: 'main',
    html_url: 'https://github.com/acme/jira',
    ...overrides,
  }
}

function manifestFor(name = 'jira', overrides = {}) {
  return JSON.stringify({ name, version: '1.0.0', description: 'Does a thing.', license: 'MIT', ...overrides })
}

/**
 * A GitHub whose search results are described per query.
 *
 * `totals` says how many results a query claims; `items` says which it returns.
 * Together they can express the one case that matters most and never happens in
 * a fixture taken from a live response: a total above the 1000 result cap.
 */
function fakeGitHub({ totals = {}, itemsFor = () => [], manifests = {} } = {}) {
  const calls = []
  async function fetchJson(url) {
    const parsed = new URL(url)
    const query = parsed.searchParams.get('q')
    const page = Number(parsed.searchParams.get('page'))
    calls.push({ query, page })
    return { total_count: totals[query] ?? itemsFor(query, page).length, items: itemsFor(query, page) }
  }
  async function fetchText(url) {
    calls.push({ url })
    return Object.prototype.hasOwnProperty.call(manifests, url) ? manifests[url] : null
  }
  return { fetchJson, fetchText, calls }
}

test('buildQuery covers the whole topic, or one slice of it', () => {
  assert.equal(buildQuery(), `topic:${TOPIC}`)
  assert.equal(buildQuery('10..49'), `topic:${TOPIC} stars:10..49`)
})

test('searchUrl asks for full pages, most starred first', () => {
  const url = new URL(searchUrl('topic:x', 3))
  assert.equal(url.origin + url.pathname, 'https://api.github.com/search/repositories')
  assert.equal(url.searchParams.get('per_page'), '100')
  assert.equal(url.searchParams.get('page'), '3')
  assert.equal(url.searchParams.get('sort'), 'stars')
  assert.equal(url.searchParams.get('order'), 'desc')
})

test('manifestUrl reads the default branch, whatever it is called', () => {
  assert.equal(
    manifestUrl({ fullName: 'acme/jira', defaultBranch: 'trunk' }),
    `https://raw.githubusercontent.com/acme/jira/trunk/${MANIFEST_FILE}`,
  )
})

test('toRepo keeps the fields the index publishes', () => {
  const repo = toRepo(item())
  assert.equal(repo.fullName, 'acme/jira')
  assert.equal(repo.owner, 'acme')
  assert.equal(repo.license, 'MIT')
  assert.deepEqual(repo.topics, ['jira'], 'the discovery topic itself is not a describing topic')
  assert.equal(repo.stars, 10)
  assert.equal(repo.defaultBranch, 'main')
})

test('toRepo fills in what a sparse search result leaves out', () => {
  const repo = toRepo({ full_name: 'acme/bare' })
  assert.equal(repo.owner, 'acme')
  assert.equal(repo.name, 'bare')
  assert.equal(repo.description, '')
  assert.equal(repo.license, '')
  assert.deepEqual(repo.topics, [])
  assert.equal(repo.stars, 0)
  assert.equal(repo.archived, false)
  assert.equal(repo.defaultBranch, 'main', 'a repository always has a default branch, even when the field is empty')
  assert.equal(repo.url, 'https://github.com/acme/bare')
})

test('toRepo drops a license GitHub could not identify', () => {
  assert.equal(toRepo(item({ license: { spdx_id: 'NOASSERTION' } })).license, '')
  assert.equal(toRepo(item({ license: null })).license, '')
})

test('toRepo survives a result with nothing in it at all', () => {
  const repo = toRepo(undefined)
  assert.equal(repo.fullName, '')
  assert.equal(repo.url, 'https://github.com/')
})

test('searchRepos runs one query when everything fits under the cap', async () => {
  const github = fakeGitHub({ itemsFor: () => [item()] })
  const { repos, truncated } = await searchRepos(github.fetchJson)
  assert.equal(repos.length, 1)
  assert.equal(truncated, false)
  assert.deepEqual(github.calls, [{ query: `topic:${TOPIC}`, page: 1 }], 'no second request for a single-page result')
})

test('searchRepos pages through a query, reusing the response that counted it', async () => {
  const totals = { [`topic:${TOPIC}`]: 150 }
  const itemsFor = (query, page) =>
    Array.from({ length: page === 1 ? 100 : 50 }, (_, i) => item({ full_name: `acme/c${page}-${i}` }))
  const github = fakeGitHub({ totals, itemsFor })

  const { repos } = await searchRepos(github.fetchJson)
  assert.equal(repos.length, 150)
  assert.deepEqual(github.calls.map((c) => c.page), [1, 2])
})

test('searchRepos partitions by stars once the topic outgrows the cap', async () => {
  const totals = { [`topic:${TOPIC}`]: 4000 }
  for (const bucket of STAR_BUCKETS) totals[`topic:${TOPIC} stars:${bucket}`] = 1
  const github = fakeGitHub({ totals, itemsFor: (query) => [item({ full_name: `acme/${query.slice(-3)}` })] })

  const { repos, truncated } = await searchRepos(github.fetchJson)
  assert.equal(truncated, false)
  assert.equal(repos.length, STAR_BUCKETS.length)
  assert.deepEqual(
    github.calls.slice(1).map((c) => c.query),
    STAR_BUCKETS.map((bucket) => `topic:${TOPIC} stars:${bucket}`),
  )
})

test('searchRepos deduplicates a repository that moved between two buckets', async () => {
  const totals = { [`topic:${TOPIC}`]: 4000 }
  for (const bucket of STAR_BUCKETS) totals[`topic:${TOPIC} stars:${bucket}`] = 1
  const github = fakeGitHub({ totals, itemsFor: () => [item()] })

  const { repos } = await searchRepos(github.fetchJson)
  assert.equal(repos.length, 1, 'the same full name in two buckets is one connector, not two')
})

test('searchRepos reports truncation rather than hiding it', async () => {
  const totals = { [`topic:${TOPIC}`]: 40_000 }
  for (const bucket of STAR_BUCKETS) totals[`topic:${TOPIC} stars:${bucket}`] = 0
  totals[`topic:${TOPIC} stars:0`] = 2000
  const github = fakeGitHub({ totals, itemsFor: (query, page) => (query.endsWith('stars:0') ? [item({ full_name: `acme/z${page}` })] : []) })

  const { repos, truncated } = await searchRepos(github.fetchJson)
  assert.equal(truncated, true)
  assert.equal(repos.length, 10, 'only the reachable thousand, in ten pages of one')
})

test('searchRepos treats a response with no body as no results', async () => {
  const { repos, truncated } = await searchRepos(async () => null)
  assert.deepEqual(repos, [])
  assert.equal(truncated, false)
})

test('parseManifest accepts a well-formed manifest and normalises it', () => {
  const parsed = parseManifest(manifestFor('jira-issues', { keywords: ['jira', '', 'issues'] }))
  assert.equal(parsed.ok, true)
  assert.equal(parsed.manifest.name, 'jira-issues')
  assert.equal(parsed.manifest.displayName, 'jira-issues', 'the name stands in when no display name is given')
  assert.deepEqual(parsed.manifest.keywords, ['jira', 'issues'])
  assert.equal(parsed.manifest.homepage, '')
})

test('parseManifest keeps a display name when there is one', () => {
  const parsed = parseManifest(manifestFor('jira', { displayName: 'Jira Issues', homepage: 'https://x.test' }))
  assert.equal(parsed.manifest.displayName, 'Jira Issues')
  assert.equal(parsed.manifest.homepage, 'https://x.test')
})

test('parseManifest caps a keyword list rather than republishing an essay', () => {
  const keywords = Array.from({ length: 40 }, (_, i) => `k${i}`)
  assert.equal(parseManifest(manifestFor('jira', { keywords })).manifest.keywords.length, 16)
})

test('parseManifest ignores a keywords field that is not a list', () => {
  assert.deepEqual(parseManifest(manifestFor('jira', { keywords: 'jira' })).manifest.keywords, [])
})

test('parseManifest says a missing file is missing, by name', () => {
  for (const absent of [null, undefined]) {
    const parsed = parseManifest(absent)
    assert.equal(parsed.ok, false)
    assert.match(parsed.reason, new RegExp(MANIFEST_FILE))
  }
})

test('parseManifest reports unparseable JSON with the parser reason', () => {
  const parsed = parseManifest('{ nope')
  assert.equal(parsed.ok, false)
  assert.match(parsed.reason, /not valid JSON/)
})

test('parseManifest refuses a document that is not an object', () => {
  for (const source of ['[]', '"text"', 'null']) {
    const parsed = parseManifest(source)
    assert.equal(parsed.ok, false)
    assert.match(parsed.reason, /must be a JSON object/)
  }
})

test('parseManifest refuses a name that is not an address', () => {
  for (const name of ['Jira Issues', 'jira_issues', '', 'jira--issues', '-jira']) {
    const parsed = parseManifest(JSON.stringify({ name, version: '1.0.0', description: 'x', license: 'MIT' }))
    assert.equal(parsed.ok, false, `"${name}" should not be a name`)
    assert.match(parsed.reason, /"name"/)
  }
})

test('parseManifest accepts the version shapes people actually publish', () => {
  for (const version of ['1', '1.4', '1.4.2', 'v1.4.2', '1.4.2-rc.1']) {
    assert.equal(parseManifest(manifestFor('jira', { version })).ok, true, version)
  }
})

test('parseManifest refuses a version it could not compare', () => {
  const parsed = parseManifest(manifestFor('jira', { version: 'latest' }))
  assert.equal(parsed.ok, false)
  assert.match(parsed.reason, /"version"/)
})

test('parseManifest requires a description and a license', () => {
  const noDescription = parseManifest(JSON.stringify({ name: 'jira', version: '1.0.0', license: 'MIT' }))
  assert.match(noDescription.reason, /"description"/)

  const noLicense = parseManifest(JSON.stringify({ name: 'jira', version: '1.0.0', description: 'x' }))
  assert.match(noLicense.reason, /"license"/)
})

test('popularity is a log curve, flattening past the ceiling', () => {
  assert.equal(popularity(0), 0)
  assert.equal(popularity(10_000), 1)
  assert.equal(popularity(50_000), 1, 'clamped, so no connector can outrank the scale itself')
  assert.ok(popularity(50) - popularity(0) > popularity(10_000) - popularity(5000))
})

test('popularity treats nonsense as no stars', () => {
  assert.equal(popularity(undefined), 0)
  assert.equal(popularity(-5), 0)
})

test('freshness halves over the half-life', () => {
  assert.equal(Math.round(freshness(daysAgo(0), NOW) * 100) / 100, 1)
  assert.equal(Math.round(freshness(daysAgo(90), NOW) * 100) / 100, 0.5)
  assert.ok(freshness(daysAgo(365), NOW) < 0.1)
})

test('freshness scores an unknown push date as nothing, and a future one as today', () => {
  assert.equal(freshness('', NOW), 0)
  assert.equal(freshness(undefined, NOW), 0)
  assert.equal(freshness(daysAgo(-30), NOW), 1)
})

test('completeness rewards a listing that introduces itself', () => {
  const full = { ok: true, description: 'x', license: 'MIT', homepage: 'https://x.test', topics: ['jira'], archived: false }
  assert.equal(completeness(full), 1)

  const bare = { ok: false, description: '', license: '', homepage: '', topics: [], archived: true }
  assert.equal(completeness(bare), 0)
})

test('completeness accepts details the manifest carries instead of the repository', () => {
  const entry = {
    ok: true,
    description: 'x',
    license: '',
    homepage: '',
    topics: ['jira'],
    archived: false,
    manifest: { license: 'MIT', homepage: 'https://x.test' },
  }
  assert.equal(completeness(entry), 1)
})

test('scoreEntry sums the weights into a readable percent', () => {
  const perfect = {
    ok: true,
    stars: 10_000,
    pushedAt: daysAgo(0),
    description: 'x',
    license: 'MIT',
    homepage: 'https://x.test',
    topics: ['jira'],
    archived: false,
  }
  assert.equal(scoreEntry(perfect, NOW), WEIGHTS.popularity + WEIGHTS.freshness + WEIGHTS.completeness)

  const nothing = { ok: false, stars: 0, pushedAt: '', description: '', license: '', homepage: '', topics: [], archived: true }
  assert.equal(scoreEntry(nothing, NOW), 0)
})

test('rank puts installable connectors above broken ones, whatever their stars', () => {
  const broken = { ok: false, fullName: 'acme/broken', stars: 9000, pushedAt: daysAgo(0), description: 'x', license: 'MIT', homepage: '', topics: [], archived: false }
  const working = { ok: true, fullName: 'acme/working', stars: 1, pushedAt: daysAgo(200), description: '', license: '', homepage: '', topics: [], archived: false }

  const ranked = rank([broken, working], NOW)
  assert.deepEqual(ranked.map((e) => e.fullName), ['acme/working', 'acme/broken'])
  assert.deepEqual(ranked.map((e) => e.rank), [1, 2])
})

test('rank breaks a tie the same way on every run', () => {
  const base = { ok: true, stars: 5, pushedAt: daysAgo(10), description: 'x', license: 'MIT', homepage: '', topics: [], archived: false }
  const entries = [{ ...base, fullName: 'z/one' }, { ...base, fullName: 'a/one' }]
  assert.deepEqual(rank(entries, NOW).map((e) => e.fullName), ['a/one', 'z/one'])
  assert.deepEqual(rank([...entries].reverse(), NOW).map((e) => e.fullName), ['a/one', 'z/one'])
})

test('rank prefers the more starred of two otherwise identical connectors', () => {
  // Same score is reachable because the score is rounded; stars decide it.
  const base = { ok: true, pushedAt: daysAgo(10), description: 'x', license: 'MIT', homepage: '', topics: [], archived: false }
  const ranked = rank([{ ...base, fullName: 'a/low', stars: 1 }, { ...base, fullName: 'b/high', stars: 1 }], NOW)
  assert.equal(ranked[0].rank, 1)
})

test('buildIndex reads every manifest and publishes a ranked index', async () => {
  const repos = [item(), item({ full_name: 'acme/slack', name: 'slack', stargazers_count: 500 })]
  const github = fakeGitHub({
    itemsFor: () => repos,
    manifests: {
      [`https://raw.githubusercontent.com/acme/jira/main/${MANIFEST_FILE}`]: manifestFor('jira'),
      [`https://raw.githubusercontent.com/acme/slack/main/${MANIFEST_FILE}`]: manifestFor('slack'),
    },
  })

  const index = await buildIndex({ ...github, now: () => NOW, concurrency: 1 })
  assert.equal(index.schemaVersion, SCHEMA_VERSION)
  assert.equal(index.topic, TOPIC)
  assert.equal(index.generatedAt, '2026-10-10T00:00:00.000Z')
  assert.equal(index.count, 2)
  assert.equal(index.ok, 2)
  assert.deepEqual(index.weights, WEIGHTS)
  assert.deepEqual(index.connectors.map((c) => c.fullName), ['acme/slack', 'acme/jira'])
  assert.equal(index.connectors[0].manifest.name, 'slack')
})

test('buildIndex lists a repository with no manifest, with the reason', async () => {
  const github = fakeGitHub({ itemsFor: () => [item()] })
  const index = await buildIndex({ ...github, now: () => NOW })

  assert.equal(index.count, 1)
  assert.equal(index.ok, 0)
  assert.equal(index.connectors[0].ok, false)
  assert.match(index.connectors[0].reason, new RegExp(MANIFEST_FILE))
})

test('buildIndex distinguishes a manifest it could not read from one that is bad', async () => {
  const github = fakeGitHub({ itemsFor: () => [item()] })
  const index = await buildIndex({
    fetchJson: github.fetchJson,
    fetchText: async () => {
      throw new Error('fetch failed')
    },
    now: () => NOW,
  })
  assert.match(index.connectors[0].reason, /Could not read .*fetch failed/)
})

test('buildIndex works through a long list in bounded batches', async () => {
  const repos = Array.from({ length: 13 }, (_, i) => item({ full_name: `acme/c${i}`, name: `c${i}` }))
  const github = fakeGitHub({ itemsFor: () => repos })
  let inFlight = 0
  let peak = 0

  const index = await buildIndex({
    fetchJson: github.fetchJson,
    fetchText: async (url) => {
      inFlight += 1
      peak = Math.max(peak, inFlight)
      await new Promise((resolve) => setImmediate(resolve))
      inFlight -= 1
      return manifestFor(url.split('/')[4])
    },
    now: () => NOW,
    concurrency: 4,
  })

  assert.equal(index.count, 13)
  assert.ok(peak <= 4, `expected at most 4 concurrent reads, saw ${peak}`)
})

test('buildIndex carries truncation through to the published index', async () => {
  const totals = { [`topic:${TOPIC}`]: 40_000, [`topic:${TOPIC} stars:0`]: 2000 }
  for (const bucket of STAR_BUCKETS) totals[`topic:${TOPIC} stars:${bucket}`] ??= 0
  const github = fakeGitHub({ totals, itemsFor: () => [] })

  const index = await buildIndex({ ...github, now: () => NOW })
  assert.equal(index.truncated, true)
})

test('isUnchanged ignores the timestamp, and nothing else', () => {
  const index = { generatedAt: 'a', count: 1, connectors: [] }
  assert.equal(isUnchanged({ ...index, generatedAt: 'b' }, index), true)
  assert.equal(isUnchanged({ ...index, count: 2 }, index), false)
  assert.equal(isUnchanged(null, index), false, 'no previous index is a change')
})

test('serialize writes a readable file that ends in a newline', () => {
  const text = serialize({ count: 0 })
  assert.equal(text, '{\n  "count": 0\n}\n')
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
  assert.equal(seen[0].headers.Authorization, 'Bearer tok')
  assert.equal(seen[0].headers['X-GitHub-Api-Version'], '2022-11-28')
  assert.equal(seen[1].headers.Accept, 'text/plain')
})

test('createFetchers sends no Authorization header without a token', async () => {
  let headers
  const fetchImpl = async (url, options) => {
    headers = options.headers
    return { ok: true, status: 200, json: async () => ({}) }
  }
  await createFetchers('', fetchImpl).fetchJson('https://api.github.com/x')
  assert.equal('Authorization' in headers, false)
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

test('main writes the index and reports the change', async () => {
  const github = fakeGitHub({ itemsFor: () => [item()] })
  const written = []
  const outputs = []
  const logs = []

  const { changed, index } = await main({
    fetchers: { ...github, now: () => NOW },
    readPrevious: async () => {
      throw new Error('ENOENT')
    },
    write: async (contents) => written.push(contents),
    setOutput: async (value) => outputs.push(value),
    log: (line) => logs.push(line),
  })

  assert.equal(changed, true)
  assert.equal(written.length, 1)
  assert.equal(JSON.parse(written[0]).count, 1)
  assert.deepEqual(outputs, [true])
  assert.match(logs[0], /1 connectors/)
  assert.equal(index.count, 1)
})

test('main leaves the file alone when nothing moved', async () => {
  const github = fakeGitHub({ itemsFor: () => [item()] })
  const fetchers = { ...github, now: () => NOW }
  const first = await main({ fetchers, readPrevious: async () => '', write: async () => {}, setOutput: async () => {}, log: () => {} })

  const written = []
  const logs = []
  const { changed } = await main({
    fetchers,
    // A day later: the same connectors, a new timestamp, and nothing to commit.
    readPrevious: async () => serialize({ ...first.index, generatedAt: '2026-10-09T00:00:00.000Z' }),
    write: async (contents) => written.push(contents),
    setOutput: async () => {},
    log: (line) => logs.push(line),
  })

  assert.equal(changed, false)
  assert.deepEqual(written, [], 'a rewritten file would be a commit saying nothing happened')
  assert.match(logs[0], /No change/)
})

test('main says so out loud when the index is incomplete', async () => {
  const totals = { [`topic:${TOPIC}`]: 40_000, [`topic:${TOPIC} stars:0`]: 2000 }
  for (const bucket of STAR_BUCKETS) totals[`topic:${TOPIC} stars:${bucket}`] ??= 0
  const github = fakeGitHub({ totals, itemsFor: () => [] })
  const logs = []

  await main({
    fetchers: { ...github, now: () => NOW },
    readPrevious: async () => '',
    write: async () => {},
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
    await main({ fetchers: { ...github, now: () => NOW }, readPrevious: async () => '', write: async () => {}, log: () => {} })
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
    write: async () => {},
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
  assert.equal(index.topic, TOPIC)
  assert.equal(index.count, 0)
})

test('run as a program, it fails loudly when GitHub cannot be reached', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'connectors-cli-'))
  const { code, out } = await runCli(cwd, { OFFLINE_GITHUB_FAIL: '1' })

  assert.equal(code, 1, 'a schedule that cannot reach GitHub has to go red')
  assert.match(out, /fetch failed/)
})
