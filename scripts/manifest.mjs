// Copyright 2026 Contextual, Inc. https://agentrq.com
// This notice may not be modified or removed.
// SPDX-License-Identifier: AGPL-3.0-only

/**
 * The files a connector repository publishes, and whether they are those files.
 *
 * A connector is an MCP server compiled to a wasm component, attached to a
 * GitHub release and described by a `manifest.json`. A repository that publishes
 * several lists them in a `connectors-manifest.json` at its root. Both formats
 * are defined by the server, in `crates/domain/src/catalog/` of
 * `agentrq/connectors` — this module is a second reader of the same documents,
 * and every rule below is there to match a rule in `manifest.rs`, `index.rs` or
 * `connector_id.rs`.
 *
 * ## Why a second reader is worth having
 *
 * The server reads these at browse and install time, on the person's machine,
 * and tells that one person what went wrong. Nobody else ever sees it — least of
 * all the author, who has no install of their connector that is failing. Reading
 * them here, once a day, in public, is what turns a private install failure into
 * something its author can find.
 *
 * It follows that this must never be *stricter* than the server, and the
 * error/warning split is how that is kept honest:
 *
 * - An **error** is something the server would refuse too: a document that is
 *   not JSON, a `manifestVersion` that is not 1, a name that is not a connector
 *   id, a path that leaves the repository.
 * - A **warning** is everything that depends on a version or on judgement — an
 *   unknown key (today's server refuses it, a newer one may not), a connector
 *   with no description, a repository whose licence the catalogue does not
 *   accept. The row still lists; the warning rides along with it.
 *
 * ## Every rejection carries a reason, and the reason is copy
 *
 * The strings below are addressed to the person who wrote the document, and are
 * worth the same care as anything else a person reads.
 */

/** The file a repository publishing several connectors must have at its root. */
export const INDEX_FILE = 'connectors-manifest.json'

/** The file describing one connector. At the root, unless the index moves it. */
export const MANIFEST_FILE = 'manifest.json'

/** Both files carry this, and it is always 1. */
export const MANIFEST_VERSION = 1

/** `index.rs`: the most connectors and bundles one index may list. */
export const INDEX_MAX_CONNECTORS = 100
export const INDEX_MAX_BUNDLES = 20
export const INDEX_BUNDLE_DESCRIPTION_MAX_LEN = 4096

/** `index.rs`: the longest manifest path an index may hold. */
const PATH_MAX_LEN = 256

/**
 * `connector_id.rs`: `^[a-z][a-z0-9-]{0,62}$`.
 *
 * It is the manifest `name`, the prefix of every command (`gmail.tools`) and the
 * name of the release asset, so it is an address rather than a label.
 */
export const CONNECTOR_ID_RE = /^[a-z][a-z0-9-]{0,62}$/

/** `manifest.rs`: the keys a connector manifest may carry. */
export const MANIFEST_KEYS = new Set([
  '$schema',
  'manifestVersion',
  'mcpVersion',
  'name',
  'title',
  'version',
  'description',
  'websiteUrl',
  'icons',
  'tools',
  'resources',
  'resourceTemplates',
  'prompts',
  'skills',
  'auth',
  'inputs',
  'permissions',
  'limits',
  'remote',
  'workflow',
])

/**
 * The licences the public catalogue accepts for a wasm connector.
 *
 * A connector is code that runs on the user's machine with whatever the manifest
 * asks for, so "may I read this, and what may I do with it" is part of the
 * decision to install, not metadata. Three copyleft-or-permissive families, and
 * nothing that leaves the question open.
 *
 * GitHub's search returns the short identifier (`AGPL-3.0`, not
 * `AGPL-3.0-only`), while a manifest or a `LICENSE` detected by Licensee may
 * carry the long one. Both spellings are the same licence, so both are here —
 * refusing a repository over which SPDX form GitHub happened to return would be
 * a rule about the API rather than about the licence.
 */
export const ACCEPTED_LICENSES = new Set([
  'AGPL-3.0',
  'AGPL-3.0-only',
  'AGPL-3.0-or-later',
  'Apache-2.0',
  'MIT',
])

const fail = (reason) => ({ ok: false, reason })
const str = (value) => (typeof value === 'string' ? value.trim() : '')
const isPlainObject = (value) => Boolean(value) && typeof value === 'object' && !Array.isArray(value)

/** Whether the catalogue lists a wasm connector under this licence. */
export function isAcceptedLicense(spdxId) {
  const id = str(spdxId)
  // GitHub says NOASSERTION for a licence it cannot identify, which is the same
  // answer as none at all: nobody can tell what they are allowed to do.
  return id !== '' && id !== 'NOASSERTION' && ACCEPTED_LICENSES.has(id)
}

/** The sentence an excluded repository is listed with. */
export function licenseReason(spdxId) {
  const accepted = [...ACCEPTED_LICENSES].filter((id) => !id.startsWith('AGPL-3.0-'))
  const id = str(spdxId)
  if (!id || id === 'NOASSERTION') {
    return (
      'No licence GitHub can identify. Add a LICENSE file — the catalogue lists ' +
      `wasm connectors under ${accepted.join(', ')}.`
    )
  }
  return `Licensed as "${id}". The catalogue lists wasm connectors under ${accepted.join(', ')}.`
}

/** A repository-relative path to a connector's manifest, or why it is not one. */
export function parseManifestPath(value) {
  const path = str(value)
  if (!path || path.length > PATH_MAX_LEN) {
    return fail(`"${path || '(empty)'}" is not a manifest path: 1 to ${PATH_MAX_LEN} characters.`)
  }
  if (!/^[A-Za-z0-9\-_./]+$/.test(path)) {
    return fail(`"${path}" is not a manifest path: letters, digits, '-', '_', '.' and '/' only.`)
  }
  if (!path.endsWith('.json')) return fail(`"${path}" is not a manifest path: it must end in ".json".`)

  const segments = path.split('/')
  if (segments.some((segment) => segment === '' || segment === '.' || segment === '..')) {
    // The whole of the traversal defence, and the reason the path can go into a
    // raw.githubusercontent.com URL exactly as it is written.
    return fail(`"${path}" is not a manifest path: no empty, "." or ".." segment.`)
  }
  return { ok: true, path }
}

function parseBundles(raw) {
  if (raw === undefined) return { ok: true, bundles: [] }
  if (!Array.isArray(raw)) return fail(`${INDEX_FILE}: "bundles" must be an array.`)
  if (raw.length > INDEX_MAX_BUNDLES) {
    return fail(`${INDEX_FILE}: at most ${INDEX_MAX_BUNDLES} bundles, not ${raw.length}.`)
  }

  const bundles = []
  const claimed = new Set()
  for (const item of raw) {
    if (!isPlainObject(item)) return fail(`${INDEX_FILE}: "bundles" holds an entry that is not a bundle.`)

    const name = str(item.name)
    if (!CONNECTOR_ID_RE.test(name)) {
      return fail(`${INDEX_FILE}: "${name || '(none)'}" is not a bundle name: a lowercase letter, then lowercase letters, digits or '-'.`)
    }
    if (claimed.has(name)) return fail(`${INDEX_FILE}: it declares the bundle "${name}" twice.`)

    const description = str(item.description)
    if (description.length > INDEX_BUNDLE_DESCRIPTION_MAX_LEN) {
      return fail(`${INDEX_FILE}: the bundle "${name}" has a description longer than ${INDEX_BUNDLE_DESCRIPTION_MAX_LEN} characters.`)
    }
    if (!Array.isArray(item.connectors) || item.connectors.length === 0) {
      return fail(`${INDEX_FILE}: the bundle "${name}" must list the connectors in it.`)
    }
    const connectors = item.connectors.map(str)
    const bad = connectors.find((id) => !CONNECTOR_ID_RE.test(id))
    if (bad !== undefined) {
      return fail(`${INDEX_FILE}: the bundle "${name}" lists "${bad || '(none)'}", which is not a connector name.`)
    }

    claimed.add(name)
    bundles.push({ name, title: str(item.title) || name, description, connectors })
  }
  return { ok: true, bundles }
}

/**
 * Reads a `connectors-manifest.json`.
 *
 * The file is optional: a repository publishing one connector keeps its
 * `manifest.json` at the root and needs no index. A repository publishing
 * several must have one, which is a rule only the release can confirm — so it is
 * checked in the builder, where the release is, and not here.
 */
export function parseIndex(source) {
  if (source === null || source === undefined) return { ok: true, index: null }

  let doc
  try {
    doc = JSON.parse(source)
  } catch (error) {
    return fail(`${INDEX_FILE} is not valid JSON: ${error.message}`)
  }
  if (!isPlainObject(doc)) return fail(`${INDEX_FILE} must be a JSON object.`)
  if (doc.manifestVersion !== MANIFEST_VERSION) {
    return fail(`${INDEX_FILE}: "manifestVersion" must be ${MANIFEST_VERSION}, not ${JSON.stringify(doc.manifestVersion ?? null)}.`)
  }
  if (!Array.isArray(doc.connectors) || doc.connectors.length === 0) {
    return fail(`${INDEX_FILE}: "connectors" must list at least one connector.`)
  }
  if (doc.connectors.length > INDEX_MAX_CONNECTORS) {
    return fail(`${INDEX_FILE}: at most ${INDEX_MAX_CONNECTORS} connectors, not ${doc.connectors.length}.`)
  }

  const manifests = []
  for (const item of doc.connectors) {
    if (!isPlainObject(item)) return fail(`${INDEX_FILE}: "connectors" holds an entry that is not an object.`)
    const parsed = parseManifestPath(item.manifest)
    if (!parsed.ok) return fail(`${INDEX_FILE}: ${parsed.reason}`)
    if (manifests.includes(parsed.path)) return fail(`${INDEX_FILE}: it lists "${parsed.path}" twice.`)
    manifests.push(parsed.path)
  }

  const bundles = parseBundles(doc.bundles)
  if (!bundles.ok) return bundles

  const warnings = []
  const unknown = Object.keys(doc).filter((key) => !['$schema', 'manifestVersion', 'connectors', 'bundles'].includes(key))
  if (unknown.length > 0) {
    warnings.push(`${INDEX_FILE} has ${unknown.length > 1 ? 'keys' : 'a key'} the server does not know: ${unknown.map((k) => `"${k}"`).join(', ')}.`)
  }

  return { ok: true, warnings, index: { manifests, bundles: bundles.bundles } }
}

/**
 * Reads one connector's `manifest.json`.
 *
 * Four fields are required because the server's `Manifest` makes them so;
 * everything else it carries — tools, auth, inputs, permissions, limits — is
 * read for the listing but not judged here. Those are the fields that say what a
 * connector may *do*, and the screen that asks the user to agree to them is the
 * right place to check them, against the version of the host doing the asking.
 */
export function parseManifest(source, { path = MANIFEST_FILE } = {}) {
  if (source === null || source === undefined) {
    return fail(`No ${path} in this repository.`)
  }

  let doc
  try {
    doc = JSON.parse(source)
  } catch (error) {
    return fail(`${path} is not valid JSON: ${error.message}`)
  }
  if (!isPlainObject(doc)) return fail(`${path} must be a JSON object.`)

  if (doc.manifestVersion !== MANIFEST_VERSION) {
    return fail(`${path}: "manifestVersion" must be ${MANIFEST_VERSION}, not ${JSON.stringify(doc.manifestVersion ?? null)}.`)
  }

  const name = str(doc.name)
  if (!CONNECTOR_ID_RE.test(name)) {
    return fail(`${path}: "${name || '(none)'}" is not a connector name — a lowercase letter, then up to 62 lowercase letters, digits or '-'.`)
  }

  const version = str(doc.version)
  if (!version) return fail(`${path}: "version" is required, for example "0.1.0".`)

  const mcpVersion = str(doc.mcpVersion)
  if (!mcpVersion) {
    return fail(`${path}: "mcpVersion" is required — the MCP revision this was written against, such as "2026-07-28".`)
  }

  const warnings = []
  const unknown = Object.keys(doc).filter((key) => !MANIFEST_KEYS.has(key))
  if (unknown.length > 0) {
    // The server refuses an unknown key outright. Said as a warning rather than
    // an error anyway, because "the server I know about refuses this" is a fact
    // with a date on it, and a nightly job is the wrong thing to be making that
    // call for an install it has never seen.
    warnings.push(
      `${path} has ${unknown.length > 1 ? 'keys' : 'a key'} the server does not know: ` +
        `${unknown.map((k) => `"${k}"`).join(', ')}. Today's server refuses a manifest with an unknown key.`,
    )
  }

  const description = str(doc.description)
  if (!description) {
    warnings.push(`${path} has no "description", so the catalogue has nothing to say about ${name}.`)
  }
  if (!Array.isArray(doc.icons) || doc.icons.length === 0) {
    warnings.push(`${path} has no "icons", so ${name} shows without one.`)
  }

  const tools = Array.isArray(doc.tools) ? doc.tools : []
  const isRemote = isPlainObject(doc.remote)
  const isWorkflow = isPlainObject(doc.workflow)
  if (tools.length === 0 && !isRemote && !isWorkflow) {
    warnings.push(`${path} declares no tools, and names neither a remote server nor a workflow.`)
  }

  return {
    ok: true,
    warnings,
    manifest: {
      name,
      title: str(doc.title) || name,
      version,
      mcpVersion,
      description,
      websiteUrl: str(doc.websiteUrl),
      icons: Array.isArray(doc.icons) ? doc.icons.length : 0,
      tools: tools.map((tool) => str(tool?.name)).filter(Boolean),
      skills: doc.skills === true,
      auth: isPlainObject(doc.auth) ? str(doc.auth.type) || 'yes' : '',
      inputs: Array.isArray(doc.inputs) ? doc.inputs.length : 0,
      remote: isRemote,
      workflow: isWorkflow,
    },
  }
}

/** `remote_file.rs`: the file at the root of a repository with the remote topic. */
export const REMOTE_FILE = 'remote-connector.json'

const REMOTE_TITLE_MAX_CHARS = 128
const REMOTE_DESCRIPTION_MAX_CHARS = 4096

/**
 * Reads a `remote-connector.json`: where a remote MCP server is.
 *
 * A remote connector is not a wasm component and has no release — the repository
 * is a pointer to somebody's hosted server. Only `url` is required, and it must
 * be `https:`, because the alternative is publishing a catalogue row that sends
 * a user's credentials over plaintext.
 */
export function parseRemoteFile(source) {
  if (source === null || source === undefined) return fail(`No ${REMOTE_FILE} in this repository.`)

  let doc
  try {
    doc = JSON.parse(source)
  } catch (error) {
    return fail(`${REMOTE_FILE} is not valid JSON: ${error.message}`)
  }
  if (!isPlainObject(doc)) return fail(`${REMOTE_FILE} must be a JSON object.`)

  const url = str(doc.url)
  let parsed
  try {
    parsed = new URL(url)
  } catch {
    return fail(`${REMOTE_FILE}: "url" is required — the server's Streamable HTTP endpoint.`)
  }
  if (parsed.protocol !== 'https:') {
    return fail(`${REMOTE_FILE}: "url" must be https, not "${parsed.protocol.replace(':', '')}".`)
  }

  const name = str(doc.name)
  if (name && !CONNECTOR_ID_RE.test(name)) {
    return fail(`${REMOTE_FILE}: "${name}" is not a connector name — a lowercase letter, then lowercase letters, digits or '-'.`)
  }

  const title = str(doc.title)
  if (title.length > REMOTE_TITLE_MAX_CHARS) {
    return fail(`${REMOTE_FILE}: "title" is longer than ${REMOTE_TITLE_MAX_CHARS} characters.`)
  }
  const description = str(doc.description)
  if (description.length > REMOTE_DESCRIPTION_MAX_CHARS) {
    return fail(`${REMOTE_FILE}: "description" is longer than ${REMOTE_DESCRIPTION_MAX_CHARS} characters.`)
  }

  const warnings = []
  const unknown = Object.keys(doc).filter((key) => !['url', 'name', 'title', 'description'].includes(key))
  if (unknown.length > 0) {
    warnings.push(`${REMOTE_FILE} has ${unknown.length > 1 ? 'keys' : 'a key'} the server refuses: ${unknown.map((k) => `"${k}"`).join(', ')}.`)
  }
  if (!description) warnings.push(`${REMOTE_FILE} has no "description", so the catalogue has nothing to say about it.`)

  return { ok: true, warnings, remote: { url, name, title, description } }
}
