// Copyright 2026 Contextual, Inc. https://agentrq.com
// This notice may not be modified or removed.

/**
 * The connector manifest, and whether one is worth listing.
 *
 * A connector is a GitHub repository carrying the `agentrq-connector` topic and
 * an `agentrq-connector.json` at its root. The topic makes it findable; this
 * file is what makes it a connector. The index, and anyone reading the index,
 * trusts what comes out of here — so this is deliberately the most testable
 * thing in the repository: pure functions, no network, no filesystem, no clock.
 *
 * ## Errors and warnings are not the same judgement
 *
 * The index is built nightly by one machine. The app that installs a connector
 * is built separately and runs wherever someone put it, possibly months behind
 * or ahead. That gap is the whole design constraint here, and it splits every
 * check in two:
 *
 * - An **error** is something no version of anything could accept: a file that
 *   is not JSON, a name that is not a name, a `sha256` that is not a digest.
 *   These are facts about the document, so the index states them with authority
 *   and marks the connector `ok: false`.
 * - A **warning** is something that depends on a version, a judgement, or a
 *   fact outside the document — an unknown key, a missing release artifact, a
 *   license that contradicts the repository's own. The connector still lists as
 *   installable; the warning is published next to it.
 *
 * Getting this backwards in either direction is costly. Enforcing a
 * version-dependent rule as an error would have a nightly job declare a
 * connector broken for a reason no user's install agrees with. Demoting a real
 * structural fault to a warning would publish a listing the app cannot install
 * and leave the author to find out from a bug report.
 *
 * ## Every rejection carries a reason, and the reason is copy
 *
 * A repository with the topic and a bad manifest is *listed as broken with its
 * reason* rather than quietly dropped. The author needs to see why — a silently
 * missing connector is undebuggable for them — and anyone following a link to it
 * needs the index to explain itself rather than appear empty. So the strings
 * below are user-facing text, addressed to the person who wrote the manifest,
 * and are worth the same care as anything else a person reads.
 */

/** The file at the root of a connector repository that describes it. */
export const MANIFEST_FILE = 'agentrq-connector.json'

/**
 * Keys a manifest may carry.
 *
 * A key outside this set is a *warning*, not an error, and the difference is the
 * version gap: a connector written for a newer app than the one that built this
 * index would otherwise be declared broken by a nightly job that simply has not
 * heard of the field yet. It is still worth saying — nearly every one of these
 * is a typo, and nothing else would ever tell the author.
 */
export const KNOWN_KEYS = new Set([
  'name',
  'displayName',
  'version',
  'description',
  'license',
  'homepage',
  'keywords',
  'engines',
  'mcp',
  'hooks',
  'net',
  'config',
  'provides',
  'artifact',
])

/**
 * The name, which is an address rather than a label.
 *
 * It ends up in a route, in registry keys and in the install directory, so a
 * name with a space in it is not one.
 */
export const NAME_RE = /^[a-z0-9]+(-[a-z0-9]+)*$/

/** `1`, `1.4`, `1.4.2`, `v1.4.2`, `1.4.2-rc.1`. */
const VERSION_RE = /^v?\d+(\.\d+){0,2}(-[0-9A-Za-z.-]+)?$/

/** Lowercase hex, 64 characters. Anything else could not be a SHA-256. */
const SHA256_RE = /^[a-f0-9]{64}$/

/** Hostnames only: no scheme, no path, no port. A leading `*.` is allowed. */
const HOST_RE = /^(\*\.)?[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$/

/**
 * One path segment inside a connector's package.
 *
 * **It may not begin with a dot**, which is the whole of the traversal defence:
 * that single rule makes `..` and `.` both unmatchable, so no entry can name a
 * file outside the directory it was installed into.
 */
const SEGMENT_RE = /^[a-zA-Z0-9_-][a-zA-Z0-9._-]*$/

/** A diagram format, which is a lookup key and a visible label both. */
const FORMAT_RE = /^[a-z][a-z0-9-]{0,31}$/i

/** The two levels a hook may be declared at. See `validateHooks`. */
const HOOK_LEVELS = new Set(['deny', 'decide'])

/** What a config field may ask for. `secret` is stored differently, hence separate. */
const CONFIG_TYPES = ['string', 'secret', 'boolean', 'number']

/** The range forms the app's own resolver understands, in match order. */
const RANGE_OPERATORS = ['>=', '^', '~', '']

/**
 * SPDX identifiers we recognise, plus the honest refusal.
 *
 * A connector runs with real access, so the terms it is offered under are part
 * of the decision to install it rather than metadata — which is why this is
 * required rather than optional.
 *
 * It is a fixed list and case-sensitive on purpose. Accepting free text would
 * make `MIT`, `mit` and `MIT License` three different licenses and none of them
 * comparable; accepting any SPDX-shaped string would take `MITT` without
 * comment. An identifier missing from the list is a one-line addition, and the
 * rejection says so rather than pretending the license is invalid.
 *
 * `UNLICENSED` is deliberately here. "All rights reserved, no permission
 * granted" is an answer a user can act on. Silence is not an answer; it just
 * leaves the question unasked.
 */
export const SPDX = new Set([
  'AGPL-3.0-only',
  'AGPL-3.0-or-later',
  'Apache-2.0',
  'BSD-2-Clause',
  'BSD-3-Clause',
  'BSL-1.0',
  'CC0-1.0',
  'CC-BY-4.0',
  'CC-BY-SA-4.0',
  'EPL-2.0',
  'GPL-2.0-only',
  'GPL-2.0-or-later',
  'GPL-3.0-only',
  'GPL-3.0-or-later',
  'ISC',
  'LGPL-2.1-only',
  'LGPL-3.0-only',
  'LGPL-3.0-or-later',
  'MIT',
  'MPL-2.0',
  'Unlicense',
  'Zlib',
  'UNLICENSED',
])

const fail = (reason) => ({ ok: false, reason })

/** A trimmed string, or '' for anything that is not one. */
const str = (value) => (typeof value === 'string' ? value.trim() : '')

const isPlainObject = (value) => Boolean(value) && typeof value === 'object' && !Array.isArray(value)

/**
 * Validates the license, suggesting the right spelling where it can.
 *
 * The case-insensitive near-match is the whole reason this is worth a function:
 * `mit` is not a slip to reject flatly, it is somebody who meant `MIT`, and
 * saying so costs nothing and saves a round trip through a maintainer.
 */
export function validateLicense(value) {
  const license = str(value)
  if (!license) {
    return fail('"license" is required. Use an SPDX identifier such as "MIT", or "UNLICENSED".')
  }
  if (SPDX.has(license)) return { ok: true, license }

  const near = [...SPDX].find((id) => id.toLowerCase() === license.toLowerCase())
  if (near) {
    return fail(`"license" must be spelled exactly as the SPDX identifier: "${near}", not "${license}".`)
  }
  return fail(
    `"license" is not a recognised SPDX identifier: "${license}". ` +
      'Use one of the standard identifiers, or "UNLICENSED" if no permission is granted.',
  )
}

/**
 * The app versions a connector says it runs on.
 *
 * The *form* is checked, never satisfaction — this index has no app version to
 * satisfy it against, and inventing one would mean a nightly job deciding on
 * behalf of installs it knows nothing about. The app re-asks the question at
 * install time, against the version of itself that is actually running.
 *
 * A range outside the supported subset is **refused rather than guessed at**. A
 * range nobody parsed correctly is how a connector ends up installed on a
 * version it cannot run on, and the failure then surfaces a long way from the
 * manifest that caused it.
 */
export function validateEngines(value) {
  if (value === undefined) {
    return fail('"engines" is required, naming the app versions this runs on, for example { "agentrq": "^1.4" }.')
  }
  if (!isPlainObject(value)) return fail('"engines" must be an object, for example { "agentrq": "^1.4" }.')

  const spec = str(value.agentrq)
  if (!spec) return fail('"engines.agentrq" is required, for example "^1.4".')

  const operator = RANGE_OPERATORS.find((op) => op === '' || spec.startsWith(op))
  if (!VERSION_RE.test(spec.slice(operator.length))) {
    return fail(
      `"engines.agentrq" is not a range this understands: "${spec}". ` +
        'Supported forms are "1.4.0", "^1.4", "~1.4" and ">=1.4".',
    )
  }
  return { ok: true, engines: { agentrq: spec } }
}

/** The tools a connector asks for, on one of the two MCP surfaces. */
export function validateToolList(value, surface) {
  if (value === undefined) return { ok: true, tools: [] }
  if (!Array.isArray(value)) return fail(`"mcp.${surface}" must be a list of tool names.`)

  const tools = []
  for (const entry of value) {
    const tool = str(entry)
    if (!tool) return fail(`"mcp.${surface}" contains an entry that is not a tool name.`)
    if (tools.includes(tool)) return fail(`"mcp.${surface}" lists "${tool}" twice.`)
    tools.push(tool)
  }
  return { ok: true, tools }
}

export function validateMcp(value) {
  if (value === undefined) return { ok: true, mcp: { tools: [], workspace: [] } }
  if (!isPlainObject(value)) return fail('"mcp" must be an object naming the tools this connector uses.')

  const tools = validateToolList(value.tools, 'tools')
  if (!tools.ok) return tools
  const workspace = validateToolList(value.workspace, 'workspace')
  if (!workspace.ok) return workspace

  return { ok: true, mcp: { tools: tools.tools, workspace: workspace.tools } }
}

/**
 * What a connector may be *asked* on, rather than what it may call.
 *
 * Everything else in a manifest describes what a connector reaches for. A hook
 * is the other direction: the app stops and asks the connector something, and
 * the connector's answer stands in for the user's.
 *
 * Two levels, because refusing and approving are not the same ask. A connector
 * that only ever *refuses* is a guardrail — it can cost you a tool call you
 * wanted, and nothing worse. One that can *approve* is answering yes on your
 * behalf, to a question the app exists to put in front of you. Collapsing those
 * into one "may review tool calls" would charge the same price for both and
 * quietly sell the second under the name of the first.
 */
export function validateHooks(value) {
  if (value === undefined) return { ok: true, hooks: {} }
  if (!isPlainObject(value)) return fail('"hooks" must be an object, for example { "toolCall": "deny" }.')

  const unknown = Object.keys(value).filter((key) => key !== 'toolCall')
  if (unknown.length > 0) {
    // An error rather than a warning, unlike an unknown top-level key: a hook
    // grants the connector a say over the user's answer, and a name nobody
    // recognises must never be read as a smaller version of one that is.
    return fail(`Unknown hook${unknown.length > 1 ? 's' : ''}: ${unknown.map((k) => `"${k}"`).join(', ')}.`)
  }

  const toolCall = str(value.toolCall)
  if (!toolCall) return { ok: true, hooks: {} }
  if (!HOOK_LEVELS.has(toolCall)) {
    return fail(
      '"hooks.toolCall" must be "deny" (it may only refuse a tool call) or "decide" ' +
        `(it may approve or refuse one). Got "${toolCall}".`,
    )
  }
  return { ok: true, hooks: { toolCall } }
}

/**
 * Hosts the author says the connector will contact.
 *
 * **This is a declaration, not a restriction, and nothing anywhere enforces it.**
 * A connector is trusted code — it can open any socket it likes, and this field
 * changes nothing about that. Saying otherwise would be worse than saying
 * nothing, because a list of hosts under a heading that reads like permissions
 * is read as a boundary.
 *
 * It is here because it is still worth knowing. A connector that says it talks
 * to `hooks.slack.com` has told the reader something true and useful about what
 * it is for, and one that says nothing and clearly does reach the network is a
 * mismatch a reviewer can see.
 */
export function validateNet(value) {
  if (value === undefined) return { ok: true, net: [] }
  if (!Array.isArray(value)) return fail('"net" must be a list of hostnames.')

  const net = []
  for (const entry of value) {
    const host = str(entry).toLowerCase()
    if (!HOST_RE.test(host)) {
      // Named, because the mistake is nearly always a URL pasted whole and the
      // fix is to delete everything but the host.
      return fail(`"net" contains "${str(entry)}", which is not a hostname. Use "api.example.com", with no scheme or path.`)
    }
    if (net.includes(host)) return fail(`"net" lists "${host}" twice.`)
    net.push(host)
  }
  return { ok: true, net }
}

/** Config fields a connector asks the user to fill in. */
export function validateConfig(value) {
  if (value === undefined) return { ok: true, config: [] }
  if (!Array.isArray(value)) return fail('"config" must be a list of fields.')

  const config = []
  const seen = new Set()
  for (const field of value) {
    if (!isPlainObject(field)) return fail('"config" contains an entry that is not a field.')
    const key = str(field.key)
    const type = str(field.type) || 'string'
    if (!key) return fail('Every "config" field needs a "key".')
    if (seen.has(key)) return fail(`"config" declares "${key}" twice.`)
    if (!CONFIG_TYPES.includes(type)) {
      return fail(`"config.${key}" has an unknown type "${type}". Use ${CONFIG_TYPES.join(', ')}.`)
    }
    seen.add(key)
    config.push({ key, type, label: str(field.label) || key })
  }
  return { ok: true, config }
}

/** A drawer's file, relative to the package, or '' when it is not one. */
export function parseDrawerEntry(value) {
  const path = str(value)
  if (!path || path.length > 200) return ''
  // Backslashes first: a Windows-shaped path would otherwise slip past a check
  // that only knows about forward slashes.
  if (path.includes('\\') || path.startsWith('/')) return ''

  const segments = path.split('/')
  if (!segments.every((segment) => SEGMENT_RE.test(segment))) return ''
  return /\.m?js$/.test(segments[segments.length - 1]) ? path : ''
}

/**
 * The drawers a manifest declares, or the reason it declares none properly.
 *
 * Refused rather than dropped. An author who typed a path wrong should be told
 * here, not left wondering why one format never draws.
 */
export function validateProvides(value) {
  if (value === undefined) return { ok: true, provides: { drawers: [] } }
  if (!isPlainObject(value)) return fail('"provides" must be an object.')

  const raw = value.drawers
  if (raw === undefined) return { ok: true, provides: { drawers: [] } }
  if (!Array.isArray(raw)) return fail('"provides.drawers" must be an array.')

  const drawers = []
  const claimed = new Set()
  for (const item of raw) {
    const format = str(item?.format).toLowerCase()
    if (!FORMAT_RE.test(format)) {
      return fail(`"${str(item?.format) || '(none)'}" is not a diagram format.`)
    }
    if (claimed.has(format)) return fail(`This manifest declares "${format}" twice.`)

    const entry = parseDrawerEntry(item?.entry)
    if (!entry) {
      return fail(`The drawer for "${format}" needs an "entry": a .js file inside the package, named relative to it.`)
    }
    claimed.add(format)
    drawers.push({ format, entry })
  }
  return { ok: true, provides: { drawers } }
}

/**
 * The release asset an install downloads, and the digest that pins it.
 *
 * Optional here, and its absence is a warning rather than an error, because a
 * repository can carry the topic while its first release is still being cut —
 * and a connector that nobody can install yet is still worth being findable.
 * Declared wrongly, though, is an error: every field is checkable from the
 * document alone, and a half-written `artifact` is the one failure that only
 * shows up at the moment somebody tries to install.
 */
export function validateArtifact(value) {
  if (value === undefined) return { ok: true, artifact: null }
  if (!isPlainObject(value)) {
    return fail('"artifact" must be an object naming the release, the asset and its sha256.')
  }

  const release = str(value.release)
  const asset = str(value.asset)
  const sha256 = str(value.sha256).toLowerCase()

  if (!release) return fail('"artifact.release" is required, for example "v1.2.0".')
  if (!asset) return fail('"artifact.asset" is required — the file name of the release asset.')
  if (!SHA256_RE.test(sha256)) {
    // Worth its own message: this is the field that makes an install honest, and
    // a git tag can be moved under one that is already in place.
    return fail('"artifact.sha256" must be a 64-character hex SHA-256 of the asset.')
  }
  return { ok: true, artifact: { release, asset, sha256 } }
}

/**
 * Reads a manifest and says whether it is one.
 *
 * Returns the normalised manifest and any warnings, or the single reason it is
 * not a manifest at all. Checks stop at the first error on purpose: a list of
 * twelve complaints, eleven of them caused by the first, is harder to act on
 * than one sentence naming the thing to fix.
 */
export function parseManifest(source) {
  if (source === null || source === undefined) {
    return fail(`No ${MANIFEST_FILE} at the root of this repository's default branch.`)
  }

  let doc
  try {
    doc = JSON.parse(source)
  } catch (error) {
    return fail(`${MANIFEST_FILE} is not valid JSON: ${error.message}`)
  }
  if (!isPlainObject(doc)) return fail(`${MANIFEST_FILE} must be a JSON object.`)

  const name = str(doc.name)
  if (!NAME_RE.test(name)) {
    return fail(
      `"name" must be lowercase words joined by single hyphens, such as "jira-issues". Got "${name || '(none)'}".`,
    )
  }

  const version = str(doc.version)
  if (!VERSION_RE.test(version)) {
    return fail(`"version" must look like "1.4.0". Got "${version || '(none)'}".`)
  }

  const description = str(doc.description)
  if (!description) {
    return fail('"description" is required — one sentence saying what this connector does.')
  }

  const steps = [
    validateLicense(doc.license),
    validateEngines(doc.engines),
    validateMcp(doc.mcp),
    validateHooks(doc.hooks),
    validateNet(doc.net),
    validateConfig(doc.config),
    validateProvides(doc.provides),
    validateArtifact(doc.artifact),
  ]
  const broken = steps.find((step) => !step.ok)
  if (broken) return broken

  const [{ license }, { engines }, { mcp }, { hooks }, { net }, { config }, { provides }, { artifact }] = steps

  const warnings = []
  const unknown = Object.keys(doc).filter((key) => !KNOWN_KEYS.has(key))
  if (unknown.length > 0) {
    warnings.push(
      `Unknown key${unknown.length > 1 ? 's' : ''} in ${MANIFEST_FILE}: ${unknown.map((k) => `"${k}"`).join(', ')}. ` +
        'Either a typo, or a field a newer app understands and this index does not.',
    )
  }
  if (!artifact) {
    warnings.push('No "artifact" declared, so there is nothing for the app to install yet.')
  }
  if (license === 'UNLICENSED') {
    warnings.push('Licensed as "UNLICENSED": all rights reserved, and no permission to use it is granted.')
  }
  if (hooks.toolCall === 'decide') {
    // Not a fault, and not hidden either. This is the one declaration that lets
    // a connector answer, on the user's behalf, the question the app exists to
    // ask them — a reader deciding whether to install should meet it in the
    // listing rather than on the grant screen.
    warnings.push('Declares "hooks.toolCall": "decide" — it can approve tool calls on your behalf, not only refuse them.')
  }

  return {
    ok: true,
    warnings,
    manifest: {
      name,
      displayName: str(doc.displayName) || name,
      version,
      description,
      license,
      homepage: str(doc.homepage),
      keywords: Array.isArray(doc.keywords) ? doc.keywords.map(str).filter(Boolean).slice(0, 16) : [],
      engines,
      mcp,
      hooks,
      net,
      config,
      provides,
      artifact,
    },
  }
}
