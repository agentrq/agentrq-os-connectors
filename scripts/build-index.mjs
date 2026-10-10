// Copyright 2026 Contextual, Inc. https://agentrq.com
// This notice may not be modified or removed.

/**
 * The public connector index, and how it is built.
 *
 * A connector is a GitHub repository carrying the `agentrq-connector` topic and
 * an `agentrq-connector.json` at its root. The topic makes it findable; anyone
 * may publish by adding it. There is no registry to run, no submission flow and
 * nobody arbitrating a namespace — which is why the index carries an owner and a
 * star count rather than a badge. **Discovery is not endorsement.**
 *
 * ## Why this repository exists at all
 *
 * The app used to run this search itself, against api.github.com, from every
 * install. GitHub's search allows ten requests a minute unauthenticated, so a
 * popular release spends that budget on the first few people to open the panel
 * and the rest see an empty catalogue. Here the search runs once a day, on one
 * machine, with a token, and every install reads one static JSON file off a CDN
 * that has no rate limit worth the name.
 *
 * It also makes the list *reviewable*: the ranking is a commit, so a connector
 * appearing, moving or vanishing shows up in `git log` instead of happening
 * silently inside somebody's app.
 *
 * ## Two GitHub limits shape everything below
 *
 * - **1000 results maximum** for any one search, however many exist. This is why
 *   the search is partitioned across star buckets rather than run once — the
 *   ceiling is not hypothetical, `topic:mcp-server` already returns well over
 *   28,000 repositories, of which only the first 1000 are reachable.
 * - **Secondary rate limits** on bursts. Hence the small concurrency pool rather
 *   than firing every manifest request at once.
 *
 * Everything that touches the network is injected, so the whole path is testable
 * without one.
 */

import { writeFile, readFile } from 'node:fs/promises'
import { argv, env, exit } from 'node:process'

import { MANIFEST_FILE, parseManifest } from './manifest.mjs'

export { MANIFEST_FILE }

/** The topic that makes a repository findable as a connector. */
export const TOPIC = 'agentrq-connector'

/** GitHub's maximum page size, and its hard ceiling on any single search. */
export const SEARCH_PAGE_SIZE = 100
export const SEARCH_RESULT_CAP = 1000

/**
 * Star ranges the search is split across when one query cannot reach everything.
 *
 * Split by stars rather than by date because the distribution is extreme — the
 * overwhelming majority of repositories have none — so the buckets get finer at
 * the bottom, where the crowd is. Each is a disjoint range, so a repository
 * appears in exactly one and no result is counted twice.
 */
export const STAR_BUCKETS = ['>=1000', '200..999', '50..199', '10..49', '3..9', '1..2', '0']

/** Bumped when the shape below changes in a way a reader has to know about. */
export const SCHEMA_VERSION = 1

/** The search query for the whole topic, or for one slice of it. */
export function buildQuery(starRange = '') {
  return starRange ? `topic:${TOPIC} stars:${starRange}` : `topic:${TOPIC}`
}

export function searchUrl(query, page) {
  const params = new URLSearchParams({
    q: query,
    per_page: String(SEARCH_PAGE_SIZE),
    page: String(page),
    sort: 'stars',
    order: 'desc',
  })
  return `https://api.github.com/search/repositories?${params}`
}

/** Where a repository's manifest lives, on whichever branch is its default. */
export function manifestUrl({ fullName, defaultBranch }) {
  return `https://raw.githubusercontent.com/${fullName}/${defaultBranch}/${MANIFEST_FILE}`
}

/** The fields of a search result the index actually publishes. */
export function toRepo(item) {
  const fullName = item?.full_name ?? ''
  return {
    fullName,
    owner: item?.owner?.login ?? fullName.split('/')[0] ?? '',
    name: item?.name ?? fullName.split('/')[1] ?? '',
    description: item?.description ?? '',
    homepage: item?.homepage ?? '',
    license: item?.license?.spdx_id && item.license.spdx_id !== 'NOASSERTION' ? item.license.spdx_id : '',
    topics: Array.isArray(item?.topics) ? item.topics.filter((t) => t !== TOPIC) : [],
    stars: item?.stargazers_count ?? 0,
    forks: item?.forks_count ?? 0,
    openIssues: item?.open_issues_count ?? 0,
    archived: Boolean(item?.archived),
    createdAt: item?.created_at ?? '',
    pushedAt: item?.pushed_at ?? '',
    defaultBranch: item?.default_branch || 'main',
    url: item?.html_url ?? `https://github.com/${fullName}`,
  }
}

/**
 * Every repository carrying the topic, across as many searches as it takes.
 *
 * Reports `truncated` when the ceiling genuinely could not be worked around — a
 * single star bucket holding more than a thousand repositories. Reported rather
 * than hidden: an index quietly missing entries is worse than one that says it
 * is incomplete, because from the outside the two look identical.
 */
export async function searchRepos(fetchJson) {
  const first = await fetchJson(searchUrl(buildQuery(), 1))
  const total = first?.total_count ?? 0

  if (total <= SEARCH_RESULT_CAP) {
    return { repos: await drainPages(fetchJson, buildQuery(), total, first), truncated: false }
  }

  const seen = new Map()
  let truncated = false
  for (const bucket of STAR_BUCKETS) {
    const query = buildQuery(bucket)
    const head = await fetchJson(searchUrl(query, 1))
    const count = head?.total_count ?? 0
    if (count > SEARCH_RESULT_CAP) truncated = true

    for (const repo of await drainPages(fetchJson, query, count, head)) {
      // Buckets are disjoint, so this only guards against a repository gaining
      // or losing a star between two requests — cheap insurance against a
      // duplicate or a gap that would otherwise be invisible.
      seen.set(repo.fullName, repo)
    }
  }
  return { repos: [...seen.values()], truncated }
}

/** Pages through one query, reusing the response that told us how many there are. */
async function drainPages(fetchJson, query, total, first) {
  const reachable = Math.min(total, SEARCH_RESULT_CAP)
  const pages = Math.ceil(reachable / SEARCH_PAGE_SIZE)
  const repos = (first?.items ?? []).map(toRepo)

  for (let page = 2; page <= pages; page += 1) {
    const body = await fetchJson(searchUrl(query, page))
    repos.push(...(body?.items ?? []).map(toRepo))
  }
  return repos.slice(0, reachable)
}

/**
 * How much each signal is worth. They sum to 100, so a score reads as a percent.
 *
 * The weights encode an opinion worth stating plainly: popularity is the loudest
 * signal but not the only one, because a brand-new connector with no stars would
 * otherwise be unrankable and unfindable, and the list would calcify into
 * whatever was published first. Freshness is the counterweight — a connector
 * still being maintained outranks a more popular one that was abandoned two
 * years ago, which is the judgement a person browsing would make themselves.
 */
export const WEIGHTS = { popularity: 60, freshness: 30, completeness: 10 }

/** Where the star curve flattens out. Beyond this, more stars barely move a rank. */
export const STAR_CEILING = 10_000

/** Days after which a repository that stopped being pushed to has lost half its freshness. */
export const FRESHNESS_HALF_LIFE_DAYS = 90

/**
 * Stars, on a log curve.
 *
 * Linear would make the whole list a footnote to whichever connector happened to
 * trend once: the distance from 0 to 50 stars is the meaningful one, and
 * linearly it is invisible next to the distance from 5,000 to 10,000.
 */
export function popularity(stars) {
  const safe = Math.max(0, Number(stars) || 0)
  return clamp01(Math.log10(safe + 1) / Math.log10(STAR_CEILING + 1))
}

/** Half-life decay on the last push. Unknown or future dates score as "today". */
export function freshness(pushedAt, now) {
  const at = Date.parse(pushedAt ?? '')
  if (Number.isNaN(at)) return 0
  const days = Math.max(0, (now - at) / 86_400_000)
  return clamp01(2 ** (-days / FRESHNESS_HALF_LIFE_DAYS))
}

/**
 * Whether the listing is worth reading, which is the part an author controls.
 *
 * Unlike the other two this is not a measure of the project, it is a measure of
 * how well it introduces itself — and it is the one signal a new connector can
 * max out on day one. That is the point: it gives an author something to do
 * about their rank other than wait for stars.
 */
export function completeness(entry) {
  const checks = [
    entry.ok,
    entry.warnings.length === 0,
    Boolean(entry.description),
    Boolean(entry.license || entry.manifest?.license),
    Boolean(entry.homepage || entry.manifest?.homepage),
    entry.topics.length > 0,
    !entry.archived,
  ]
  return checks.filter(Boolean).length / checks.length
}

/** The published score: 0–100, two decimals, so a tie is a real tie. */
export function scoreEntry(entry, now) {
  const raw =
    WEIGHTS.popularity * popularity(entry.stars) +
    WEIGHTS.freshness * freshness(entry.pushedAt, now) +
    WEIGHTS.completeness * completeness(entry)
  return Math.round(raw * 100) / 100
}

function clamp01(value) {
  return Math.min(1, Math.max(0, value))
}

/**
 * Orders the index and numbers it.
 *
 * Working connectors come before broken ones whatever their stars: a listing the
 * app cannot install is not a better result than one it can, and burying it is
 * the honest treatment of a repository that carries the topic but not a manifest.
 *
 * The last tiebreak is the full name, which is arbitrary but *stable* — without
 * it two equally-scored connectors could swap places on every nightly run and
 * every one of those runs would be a commit saying nothing happened.
 */
export function rank(entries, now) {
  return entries
    .map((entry) => ({ ...entry, score: scoreEntry(entry, now) }))
    .sort(
      (a, b) =>
        Number(b.ok) - Number(a.ok) ||
        b.score - a.score ||
        b.stars - a.stars ||
        a.fullName.localeCompare(b.fullName),
    )
    .map((entry, i) => ({ rank: i + 1, ...entry }))
}

/** Turns a failure into something worth putting in front of a person. */
export function describeFailure(error) {
  return String(error?.message ?? error ?? 'Unknown error')
}

/** The release a manifest pins its artifact to. */
export function releaseUrl(fullName, tag) {
  return `https://api.github.com/repos/${fullName}/releases/tags/${encodeURIComponent(tag)}`
}

/**
 * Whether the release a manifest pins actually exists, with the asset it names.
 *
 * This is the one check that leaves the document, and it is the reason the index
 * can claim any authority at all. Everything else says the manifest is
 * well-formed; this says the thing it promises is really there. A release tag
 * that was deleted, renamed, or never cut is otherwise invisible until somebody
 * tries to install — and at that point the failure looks like the app's fault.
 *
 * A *missing* release or asset is an error, because it is a fact, checked at the
 * same moment for everyone. A request that merely *failed* is not: a timeout
 * says nothing about the release, so it downgrades to a warning rather than
 * blanking a working connector out of the index on a bad network day.
 */
export async function verifyArtifact(repo, artifact, fetchJson) {
  if (!artifact) return { ok: true, warnings: [], asset: null }

  let release
  try {
    release = await fetchJson(releaseUrl(repo.fullName, artifact.release))
  } catch (error) {
    return {
      ok: true,
      warnings: [`Could not check release "${artifact.release}": ${describeFailure(error)}`],
      asset: null,
    }
  }

  if (release === null) {
    return { ok: false, reason: `"artifact.release" names "${artifact.release}", which is not a release of this repository.` }
  }
  const asset = (release.assets ?? []).find((candidate) => candidate?.name === artifact.asset)
  if (!asset) {
    return {
      ok: false,
      reason: `Release "${artifact.release}" has no asset named "${artifact.asset}".`,
    }
  }

  const warnings = []
  if (release.draft) warnings.push(`Release "${artifact.release}" is still a draft, so nobody else can download it.`)
  if (release.prerelease) warnings.push(`Release "${artifact.release}" is marked as a pre-release.`)

  return {
    ok: true,
    warnings,
    asset: {
      name: asset.name,
      size: asset.size ?? 0,
      downloadUrl: asset.browser_download_url ?? '',
      publishedAt: release.published_at ?? '',
    },
  }
}

/**
 * Warnings about the repository rather than the document inside it.
 *
 * A manifest can be perfect and the listing still be worth a caveat. These are
 * the ones a reader would want before clicking install, and that the author can
 * see and fix.
 */
export function repoWarnings(repo, manifest) {
  const warnings = []
  if (repo.archived) {
    warnings.push('This repository is archived, so it is not being maintained.')
  }
  if (manifest && repo.license && manifest.license !== repo.license) {
    // Two different answers to "what may I do with this" is exactly the kind of
    // thing nobody notices until it matters, and the index is the only place
    // both are visible at once.
    warnings.push(
      `The manifest says "${manifest.license}" but GitHub detects "${repo.license}" in the repository. ` +
        'One of the two is wrong, and a reader cannot tell which.',
    )
  }
  return warnings
}

/**
 * Flags connectors that claim the same name as another.
 *
 * `name` is an address: it is the install directory and the registry key, so two
 * connectors answering to one name is a collision, not a coincidence. It is
 * *not* grounds for dropping either — the index has no way to tell the original
 * from the impostor, and quietly hiding one would be the index taking a side it
 * cannot defend. Both are listed, both say who they collide with, and the person
 * reading decides.
 */
export function flagNameConflicts(entries) {
  const byName = new Map()
  for (const entry of entries) {
    if (!entry.ok) continue
    const claimants = byName.get(entry.manifest.name) ?? []
    claimants.push(entry.fullName)
    byName.set(entry.manifest.name, claimants)
  }

  return entries.map((entry) => {
    const claimants = entry.ok ? byName.get(entry.manifest.name) : undefined
    if (!claimants || claimants.length < 2) return entry

    const others = claimants.filter((fullName) => fullName !== entry.fullName)
    return {
      ...entry,
      nameConflict: others,
      warnings: [
        ...entry.warnings,
        `The name "${entry.manifest.name}" is also claimed by ${others.join(', ')}.`,
      ],
    }
  })
}

/** Runs `worker` over `items`, a few at a time, to stay under the burst limits. */
async function pooled(items, limit, worker) {
  const out = []
  for (let i = 0; i < items.length; i += limit) {
    out.push(...(await Promise.all(items.slice(i, i + limit).map(worker))))
  }
  return out
}

/**
 * One listing: a repository, and either its connector or why it has none.
 *
 * @param {(url: string) => Promise<any>} fetchJson
 * @param {(url: string) => Promise<string|null>} fetchText
 */
export async function toEntry(repo, { fetchJson, fetchText }) {
  let parsed
  try {
    parsed = parseManifest(await fetchText(manifestUrl(repo)))
  } catch (error) {
    // A manifest we could not fetch is not a manifest we know to be bad. Say
    // which happened, so an author chasing a listing is not sent to look at a
    // file that is perfectly fine.
    return { ...repo, ok: false, warnings: [], reason: `Could not read ${MANIFEST_FILE}: ${describeFailure(error)}` }
  }
  if (!parsed.ok) return { ...repo, ok: false, warnings: [], reason: parsed.reason }

  const release = await verifyArtifact(repo, parsed.manifest.artifact, fetchJson)
  if (!release.ok) {
    // The manifest parses; the thing it points at does not exist. That is still
    // a connector nobody can install, so it is listed as broken — with the
    // reason naming the release, which is where the author has to go and look.
    return { ...repo, ok: false, warnings: parsed.warnings, reason: release.reason, manifest: parsed.manifest }
  }

  return {
    ...repo,
    ok: true,
    manifest: parsed.manifest,
    asset: release.asset,
    warnings: [...parsed.warnings, ...release.warnings, ...repoWarnings(repo, parsed.manifest)],
  }
}

/**
 * The whole index: search, read and check every manifest, score, order.
 *
 * @param {object} deps
 * @param {(url: string) => Promise<any>} deps.fetchJson   Resolves null on 404.
 * @param {(url: string) => Promise<string|null>} deps.fetchText  Resolves null on 404.
 * @param {() => number} [deps.now]
 * @param {number} [deps.concurrency]
 */
export async function buildIndex({ fetchJson, fetchText, now = () => Date.now(), concurrency = 6 }) {
  const at = now()
  const found = await searchRepos(fetchJson)

  const entries = await pooled(found.repos, concurrency, (repo) => toEntry(repo, { fetchJson, fetchText }))
  const ranked = rank(flagNameConflicts(entries), at)

  return {
    schemaVersion: SCHEMA_VERSION,
    topic: TOPIC,
    manifest: MANIFEST_FILE,
    generatedAt: new Date(at).toISOString(),
    truncated: found.truncated,
    count: ranked.length,
    ok: ranked.filter((entry) => entry.ok).length,
    warned: ranked.filter((entry) => entry.warnings.length > 0).length,
    weights: WEIGHTS,
    connectors: ranked,
  }
}

/**
 * Whether two indexes differ in anything a reader would care about.
 *
 * `generatedAt` changes on every run by definition. Committing on that alone
 * would put a commit a day into the history forever, and the one day something
 * actually changed would be indistinguishable from the 364 that did not.
 */
export function isUnchanged(previous, next) {
  const strip = ({ generatedAt, ...rest }) => JSON.stringify(rest)
  return Boolean(previous) && strip(previous) === strip(next)
}

/** A trailing newline, so the file is a well-formed text file and diffs cleanly. */
export function serialize(index) {
  return `${JSON.stringify(index, null, 2)}\n`
}

/** GitHub's JSON API, with the token Actions already has. */
export function createFetchers(token = '', fetchImpl = fetch) {
  const headers = {
    Accept: 'application/vnd.github+json',
    'X-GitHub-Api-Version': '2022-11-28',
    'User-Agent': 'agentrq-os-connectors-indexer',
    ...(token ? { Authorization: `Bearer ${token}` } : {}),
  }

  async function get(url, accept) {
    const response = await fetchImpl(url, { headers: { ...headers, ...(accept ? { Accept: accept } : {}) } })
    if (response.status === 404) return null
    if (!response.ok) {
      throw new Error(`${response.status} ${response.statusText} for ${url}`)
    }
    return response
  }

  return {
    fetchJson: async (url) => (await get(url))?.json() ?? null,
    fetchText: async (url) => (await get(url, 'text/plain'))?.text() ?? null,
  }
}

/**
 * Writes the index, and says whether anything moved.
 *
 * Exits 0 either way. A day on which no connector changed is a successful run,
 * not a failure, and a red cross on the schedule for it would train everyone to
 * ignore the one that means something.
 */
export async function main({
  path = 'connectors.json',
  token = env.GITHUB_TOKEN ?? '',
  log = console.log,
  fetchers = createFetchers(token),
  readPrevious = () => readFile(path, 'utf8'),
  write = (contents) => writeFile(path, contents),
  setOutput = async (changed) => {
    if (env.GITHUB_OUTPUT) await writeFile(env.GITHUB_OUTPUT, `changed=${changed}\n`, { flag: 'a' })
  },
} = {}) {
  const index = await buildIndex(fetchers)

  let previous = null
  try {
    previous = JSON.parse(await readPrevious())
  } catch {
    // No index yet, or one we cannot read. Either way this run writes a new one.
  }

  if (index.truncated) {
    log('warning: a single star bucket holds more than 1000 repositories; the index is incomplete.')
  }

  const changed = !isUnchanged(previous, index)
  if (changed) await write(serialize(index))
  log(
    `${index.count} connectors (${index.ok} installable, ${index.warned} with warnings). ` +
      `${changed ? 'Index updated.' : 'No change.'}`,
  )
  await setOutput(changed)
  return { changed, index }
}

const invokedDirectly = argv[1] && import.meta.url === new URL(`file://${argv[1]}`).href

if (invokedDirectly) {
  main().catch((error) => {
    console.error(describeFailure(error))
    exit(1)
  })
}
