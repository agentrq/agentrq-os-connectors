// Copyright 2026 Contextual, Inc. https://agentrq.com
// This notice may not be modified or removed.
// SPDX-License-Identifier: AGPL-3.0-only

/**
 * Tests for the readers of a connector repository's documents.
 *
 * Every rule here exists to match one in `crates/domain/src/catalog/` of
 * `agentrq/connectors`, so several tests below are written as "the server would
 * refuse this too". The error/warning split is the module's point and is
 * asserted directly: a check being a warning and not an error is the thing that
 * keeps a nightly job from declaring a connector broken on behalf of an install
 * it has never seen.
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'

import {
  ACCEPTED_LICENSES,
  CONNECTOR_ID_RE,
  INDEX_FILE,
  INDEX_MAX_BUNDLES,
  INDEX_MAX_CONNECTORS,
  MANIFEST_FILE,
  MANIFEST_KEYS,
  REMOTE_FILE,
  isAcceptedLicense,
  licenseReason,
  parseIndex,
  parseManifest,
  parseManifestPath,
  parseRemoteFile,
} from '../scripts/manifest.mjs'

/** The smallest manifest the server accepts. */
function manifest(overrides = {}) {
  return JSON.stringify({
    manifestVersion: 1,
    mcpVersion: '2026-07-28',
    name: 'gmail',
    version: '0.1.0',
    description: 'Reads Gmail threads.',
    icons: [{ src: 'data:image/svg+xml;base64,AAA', mimeType: 'image/svg+xml', sizes: ['any'] }],
    tools: [{ name: 'listThreads' }],
    ...overrides,
  })
}

const parse = (overrides) => parseManifest(manifest(overrides))

const index = (overrides = {}) =>
  JSON.stringify({ manifestVersion: 1, connectors: [{ manifest: 'gmail/manifest.json' }], ...overrides })

test('the accepted licences are MIT, Apache-2.0 and AGPL-3.0, in both spellings', () => {
  assert.deepEqual([...ACCEPTED_LICENSES].sort(), ['AGPL-3.0', 'AGPL-3.0-only', 'AGPL-3.0-or-later', 'Apache-2.0', 'MIT'])

  for (const id of ['MIT', 'Apache-2.0', 'AGPL-3.0', 'AGPL-3.0-only', 'AGPL-3.0-or-later']) {
    assert.equal(isAcceptedLicense(id), true, id)
  }
  // GitHub's search returns the short form and a LICENSE file may carry the
  // long one; refusing over which arrived would be a rule about the API.
  assert.equal(isAcceptedLicense('AGPL-3.0'), isAcceptedLicense('AGPL-3.0-only'))
})

test('anything else, or nothing at all, is not accepted', () => {
  for (const id of ['GPL-3.0', 'BSD-3-Clause', 'UNLICENSED', 'NOASSERTION', '', null, undefined]) {
    assert.equal(isAcceptedLicense(id), false, String(id))
  }
})

test('licenseReason tells the author what would be accepted', () => {
  assert.match(licenseReason(''), /No licence GitHub can identify/)
  assert.match(licenseReason('NOASSERTION'), /No licence GitHub can identify/)
  assert.match(licenseReason('GPL-3.0'), /Licensed as "GPL-3.0"/)
  // The long AGPL spellings would be noise in a sentence offering a choice.
  assert.match(licenseReason('GPL-3.0'), /AGPL-3\.0, Apache-2\.0, MIT/)
})

test('a manifest path stays inside the repository', () => {
  assert.equal(parseManifestPath('gmail/manifest.json').path, 'gmail/manifest.json')
  assert.equal(parseManifestPath('manifest.json').path, 'manifest.json')

  for (const bad of ['../secrets.json', 'a//b.json', './a.json', 'a/../b.json', '']) {
    assert.equal(parseManifestPath(bad).ok, false, bad)
  }
})

test('a manifest path is a path, and ends in .json', () => {
  assert.match(parseManifestPath('gmail/manifest.yaml').reason, /must end in "\.json"/)
  assert.match(parseManifestPath('gm ail/manifest.json').reason, /letters, digits/)
  assert.match(parseManifestPath(`${'a'.repeat(260)}.json`).reason, /1 to 256 characters/)
})

test('a repository with one connector needs no index', () => {
  const parsed = parseIndex(null)
  assert.equal(parsed.ok, true)
  assert.equal(parsed.index, null)
})

test('an index lists the manifests, in the order it gives them', () => {
  const parsed = parseIndex(index({ connectors: [{ manifest: 'gmail/manifest.json' }, { manifest: 'drive/manifest.json' }] }))
  assert.deepEqual(parsed.index.manifests, ['gmail/manifest.json', 'drive/manifest.json'])
  assert.deepEqual(parsed.warnings, [])
})

test('an index must be version 1, and list at least one connector', () => {
  assert.match(parseIndex('{').reason, /not valid JSON/)
  assert.match(parseIndex('[]').reason, /must be a JSON object/)
  assert.match(parseIndex(index({ manifestVersion: 2 })).reason, /"manifestVersion" must be 1, not 2/)
  assert.match(parseIndex(JSON.stringify({ connectors: [] })).reason, /"manifestVersion" must be 1, not null/)
  assert.match(parseIndex(index({ connectors: [] })).reason, /at least one connector/)
  assert.match(parseIndex(index({ connectors: 'gmail' })).reason, /at least one connector/)
})

test('an index is capped at a hundred connectors', () => {
  const connectors = Array.from({ length: INDEX_MAX_CONNECTORS + 1 }, (_, i) => ({ manifest: `c${i}/manifest.json` }))
  assert.match(parseIndex(index({ connectors })).reason, /at most 100 connectors, not 101/)
})

test('an index refuses a bad entry, a bad path, or the same path twice', () => {
  assert.match(parseIndex(index({ connectors: ['gmail/manifest.json'] })).reason, /not an object/)
  assert.match(parseIndex(index({ connectors: [{ manifest: '../x.json' }] })).reason, /no empty, "\." or "\.\." segment/)
  assert.match(
    parseIndex(index({ connectors: [{ manifest: 'a/manifest.json' }, { manifest: 'a/manifest.json' }] })).reason,
    /lists "a\/manifest.json" twice/,
  )
})

test('bundles are named like connectors and list the connectors in them', () => {
  const parsed = parseIndex(index({ bundles: [{ name: 'google', title: 'Google', connectors: ['gmail', 'drive'] }] }))
  assert.deepEqual(parsed.index.bundles, [{ name: 'google', title: 'Google', description: '', connectors: ['gmail', 'drive'] }])

  const untitled = parseIndex(index({ bundles: [{ name: 'google', connectors: ['gmail'] }] }))
  assert.equal(untitled.index.bundles[0].title, 'google', 'the name stands in when there is no title')
})

test('a bundle is refused with a reason rather than dropped', () => {
  const bad = (bundles) => parseIndex(index({ bundles })).reason
  assert.match(bad('google'), /"bundles" must be an array/)
  assert.match(bad(['google']), /not a bundle/)
  assert.match(bad([{ name: 'Google', connectors: ['gmail'] }]), /not a bundle name/)
  assert.match(bad([{ connectors: ['gmail'] }]), /"\(none\)" is not a bundle name/)
  assert.match(bad([{ name: 'g', connectors: ['gmail'] }, { name: 'g', connectors: ['drive'] }]), /declares the bundle "g" twice/)
  assert.match(bad([{ name: 'g', connectors: [] }]), /must list the connectors in it/)
  assert.match(bad([{ name: 'g', connectors: ['Gmail'] }]), /lists "Gmail", which is not a connector name/)
  assert.match(bad([{ name: 'g', connectors: ['gmail'], description: 'x'.repeat(4097) }]), /longer than 4096/)
  assert.match(bad(Array.from({ length: INDEX_MAX_BUNDLES + 1 }, (_, i) => ({ name: `b${i}`, connectors: ['gmail'] }))), /at most 20 bundles/)
})

test('a key the server does not know in an index is a warning', () => {
  const parsed = parseIndex(index({ extra: 1 }))
  assert.equal(parsed.ok, true)
  assert.match(parsed.warnings[0], /a key the server does not know: "extra"/)
  assert.equal(parseIndex(index({ $schema: 'https://x' })).warnings.length, 0, 'the schema link is a known key')
})

test('a minimal manifest parses, and is reduced to what the catalogue shows', () => {
  const parsed = parse()
  assert.equal(parsed.ok, true)
  assert.deepEqual(parsed.warnings, [])
  assert.equal(parsed.manifest.name, 'gmail')
  assert.equal(parsed.manifest.title, 'gmail', 'the name stands in when there is no title')
  assert.deepEqual(parsed.manifest.tools, ['listThreads'])
  assert.equal(parsed.manifest.icons, 1)
  assert.equal(parsed.manifest.skills, false)
  assert.equal(parsed.manifest.auth, '')
})

test('a manifest carries the fields a row shows', () => {
  const parsed = parse({
    title: 'Gmail',
    websiteUrl: 'https://mail.google.com',
    skills: true,
    auth: { type: 'oauth2' },
    inputs: [{ name: 'label' }],
  })
  assert.equal(parsed.manifest.title, 'Gmail')
  assert.equal(parsed.manifest.websiteUrl, 'https://mail.google.com')
  assert.equal(parsed.manifest.skills, true)
  assert.equal(parsed.manifest.auth, 'oauth2')
  assert.equal(parsed.manifest.inputs, 1)
})

test('an auth block with no type still says the connector authenticates', () => {
  assert.equal(parse({ auth: {} }).manifest.auth, 'yes')
})

test('a missing manifest is reported by the path it was looked for at', () => {
  assert.match(parseManifest(null).reason, new RegExp(`No ${MANIFEST_FILE}`))
  assert.match(parseManifest(undefined, { path: 'gmail/manifest.json' }).reason, /No gmail\/manifest.json/)
})

test('the four fields the server requires are errors when missing', () => {
  assert.match(parseManifest('{').reason, /not valid JSON/)
  assert.match(parseManifest('7').reason, /must be a JSON object/)
  assert.match(parse({ manifestVersion: 2 }).reason, /"manifestVersion" must be 1, not 2/)
  assert.match(parse({ name: 'Gmail' }).reason, /is not a connector name/)
  assert.match(parse({ name: undefined }).reason, /"\(none\)" is not a connector name/)
  assert.match(parse({ version: '' }).reason, /"version" is required/)
  assert.match(parse({ mcpVersion: undefined }).reason, /"mcpVersion" is required/)
})

test('a connector name is an address, as connector_id.rs defines it', () => {
  for (const name of ['g', 'gmail', 'gmail-2', `a${'b'.repeat(62)}`]) {
    assert.equal(CONNECTOR_ID_RE.test(name), true, name)
  }
  for (const name of ['', '2gmail', '-gmail', 'Gmail', 'gmail_2', `a${'b'.repeat(63)}`]) {
    assert.equal(CONNECTOR_ID_RE.test(name), false, name)
  }
})

test('an unknown manifest key is a warning, although the server refuses it today', () => {
  const parsed = parse({ surfaces: ['panel'] })
  assert.equal(parsed.ok, true, 'a nightly job is the wrong thing to be making that call for an install it has never seen')
  assert.match(parsed.warnings[0], /a key the server does not know: "surfaces"/)
  assert.match(parsed.warnings[0], /Today's server refuses/)
})

test('the known manifest keys are the ones manifest.rs declares', () => {
  for (const key of ['$schema', 'manifestVersion', 'mcpVersion', 'name', 'title', 'version', 'description', 'websiteUrl', 'icons', 'tools', 'resources', 'resourceTemplates', 'prompts', 'skills', 'auth', 'inputs', 'permissions', 'limits', 'remote', 'workflow']) {
    assert.equal(MANIFEST_KEYS.has(key), true, key)
  }
  assert.equal(MANIFEST_KEYS.size, 20)
})

test('a thin listing is warned about, not refused', () => {
  const parsed = parseManifest(JSON.stringify({ manifestVersion: 1, mcpVersion: '2026-07-28', name: 'gmail', version: '0.1.0' }))
  assert.equal(parsed.ok, true)
  const text = parsed.warnings.join('\n')
  assert.match(text, /no "description"/)
  assert.match(text, /no "icons"/)
  assert.match(text, /declares no tools/)
})

test('a connector with no tools is fine when it is a remote or a workflow', () => {
  for (const extra of [{ remote: { url: 'https://x.test/mcp' } }, { workflow: { steps: [] } }]) {
    const parsed = parse({ tools: [], ...extra })
    assert.doesNotMatch(parsed.warnings.join('\n'), /declares no tools/)
  }
  assert.equal(parse({ remote: { url: 'https://x.test/mcp' } }).manifest.remote, true)
  assert.equal(parse({ workflow: { steps: [] } }).manifest.workflow, true)
})

test('a remote file needs an https url and nothing else', () => {
  const parsed = parseRemoteFile(JSON.stringify({ url: 'https://mcp.example.com/mcp', name: 'example', title: 'Example', description: 'Does things.' }))
  assert.equal(parsed.ok, true)
  assert.deepEqual(parsed.warnings, [])
  assert.equal(parsed.remote.url, 'https://mcp.example.com/mcp')
  assert.equal(parsed.remote.name, 'example')

  const bare = parseRemoteFile(JSON.stringify({ url: 'https://mcp.example.com/mcp' }))
  assert.equal(bare.ok, true)
  assert.match(bare.warnings.join('\n'), /no "description"/)
})

test('a remote file over plaintext is refused', () => {
  // Listing it would be publishing a row that sends a user's credentials in the
  // clear.
  assert.match(parseRemoteFile(JSON.stringify({ url: 'http://mcp.example.com/mcp' })).reason, /must be https, not "http"/)
})

test('a remote file is refused with a reason rather than dropped', () => {
  assert.match(parseRemoteFile(null).reason, new RegExp(`No ${REMOTE_FILE}`))
  assert.match(parseRemoteFile('{').reason, /not valid JSON/)
  assert.match(parseRemoteFile('[]').reason, /must be a JSON object/)
  assert.match(parseRemoteFile('{}').reason, /"url" is required/)
  assert.match(parseRemoteFile(JSON.stringify({ url: 'not a url' })).reason, /"url" is required/)
  assert.match(parseRemoteFile(JSON.stringify({ url: 'https://x.test', name: 'Example' })).reason, /not a connector name/)
  assert.match(parseRemoteFile(JSON.stringify({ url: 'https://x.test', title: 'x'.repeat(129) })).reason, /longer than 128/)
  assert.match(parseRemoteFile(JSON.stringify({ url: 'https://x.test', description: 'x'.repeat(4097) })).reason, /longer than 4096/)
})

test('an unknown key in a remote file is a warning', () => {
  const parsed = parseRemoteFile(JSON.stringify({ url: 'https://x.test/mcp', description: 'x', extra: 1 }))
  assert.equal(parsed.ok, true)
  assert.match(parsed.warnings[0], /a key the server refuses: "extra"/)
})

test('the index file and the manifest file are named as the server names them', () => {
  assert.equal(INDEX_FILE, 'connectors-manifest.json')
  assert.equal(MANIFEST_FILE, 'manifest.json')
  assert.equal(REMOTE_FILE, 'remote-connector.json')
})
