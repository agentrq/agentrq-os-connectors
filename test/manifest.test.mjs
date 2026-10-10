// Copyright 2026 Contextual, Inc. https://agentrq.com
// This notice may not be modified or removed.

/**
 * Tests for manifest validation.
 *
 * The split these exercise is the point of the module: an **error** is a fact
 * about the document that no version of anything could accept, and a **warning**
 * is a judgement that depends on a version or on something outside the file.
 * Several tests below assert specifically that a check is one and not the other.
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'

import {
  KNOWN_KEYS,
  MANIFEST_FILE,
  SPDX,
  parseDrawerEntry,
  parseManifest,
  validateArtifact,
  validateConfig,
  validateEngines,
  validateHooks,
  validateLicense,
  validateMcp,
  validateNet,
  validateProvides,
  validateToolList,
} from '../scripts/manifest.mjs'

/** The smallest manifest that is one. */
function manifest(overrides = {}) {
  return JSON.stringify({
    name: 'jira',
    version: '1.0.0',
    description: 'Jira issues, as agent tools.',
    license: 'MIT',
    engines: { agentrq: '^1.4' },
    ...overrides,
  })
}

const parse = (overrides) => parseManifest(manifest(overrides))

test('a minimal manifest parses, and is normalised', () => {
  const parsed = parse()
  assert.equal(parsed.ok, true)
  assert.equal(parsed.manifest.name, 'jira')
  assert.equal(parsed.manifest.displayName, 'jira', 'the name stands in when no display name is given')
  assert.deepEqual(parsed.manifest.engines, { agentrq: '^1.4' })
  assert.deepEqual(parsed.manifest.mcp, { tools: [], workspace: [] })
  assert.deepEqual(parsed.manifest.hooks, {})
  assert.deepEqual(parsed.manifest.net, [])
  assert.deepEqual(parsed.manifest.config, [])
  assert.deepEqual(parsed.manifest.provides, { drawers: [] })
  assert.equal(parsed.manifest.artifact, null)
})

test('a full manifest keeps everything it declares', () => {
  const parsed = parse({
    displayName: 'Jira',
    homepage: 'https://acme.test',
    keywords: ['jira', '', 'issues'],
    mcp: { tools: ['createTask'], workspace: ['loadMemory'] },
    hooks: { toolCall: 'deny' },
    net: ['API.Example.com', '*.atlassian.net'],
    config: [{ key: 'token', type: 'secret', label: 'API token' }, { key: 'site' }],
    provides: { drawers: [{ format: 'Gantt', entry: 'drawers/gantt.mjs' }] },
    artifact: { release: 'v1.0.0', asset: 'jira.tgz', sha256: 'a'.repeat(64) },
  })

  assert.equal(parsed.ok, true)
  assert.deepEqual(parsed.warnings, [])
  assert.equal(parsed.manifest.displayName, 'Jira')
  assert.deepEqual(parsed.manifest.keywords, ['jira', 'issues'])
  assert.deepEqual(parsed.manifest.mcp, { tools: ['createTask'], workspace: ['loadMemory'] })
  assert.deepEqual(parsed.manifest.net, ['api.example.com', '*.atlassian.net'], 'hostnames are compared lowercase')
  assert.deepEqual(parsed.manifest.config, [
    { key: 'token', type: 'secret', label: 'API token' },
    { key: 'site', type: 'string', label: 'site' },
  ])
  assert.deepEqual(parsed.manifest.provides.drawers, [{ format: 'gantt', entry: 'drawers/gantt.mjs' }])
  assert.equal(parsed.manifest.artifact.sha256, 'a'.repeat(64))
})

test('a missing file is reported as missing, by name', () => {
  for (const absent of [null, undefined]) {
    const parsed = parseManifest(absent)
    assert.equal(parsed.ok, false)
    assert.match(parsed.reason, new RegExp(MANIFEST_FILE))
  }
})

test('unparseable JSON is reported with the parser reason', () => {
  const parsed = parseManifest('{ nope')
  assert.equal(parsed.ok, false)
  assert.match(parsed.reason, /not valid JSON/)
})

test('a document that is not an object is refused', () => {
  for (const source of ['[]', '"text"', 'null', '7']) {
    assert.match(parseManifest(source).reason, /must be a JSON object/, source)
  }
})

test('a name must be an address, not a label', () => {
  for (const name of ['Jira Issues', 'jira_issues', '', 'jira--issues', '-jira', 'jira-']) {
    const parsed = parse({ name })
    assert.equal(parsed.ok, false, `"${name}" should not be a name`)
    assert.match(parsed.reason, /"name"/)
  }
  assert.equal(parse({ name: 'jira-issues-2' }).ok, true)
})

test('the version shapes people actually publish are accepted', () => {
  for (const version of ['1', '1.4', '1.4.2', 'v1.4.2', '1.4.2-rc.1']) {
    assert.equal(parse({ version }).ok, true, version)
  }
  assert.match(parse({ version: 'latest' }).reason, /"version"/)
  assert.match(parse({ version: '' }).reason, /\(none\)/)
})

test('a description is required, because the listing is mostly description', () => {
  assert.match(parse({ description: '   ' }).reason, /"description"/)
})

test('validateLicense takes the SPDX list and nothing else', () => {
  assert.deepEqual(validateLicense('MIT'), { ok: true, license: 'MIT' })
  assert.equal(SPDX.has('UNLICENSED'), true, 'all rights reserved is an answer; silence is not')
  assert.match(validateLicense('').reason, /required/)
  assert.match(validateLicense('Free to use').reason, /not a recognised SPDX identifier/)
})

test('validateLicense suggests the spelling when the intent is obvious', () => {
  assert.match(validateLicense('mit').reason, /"MIT", not "mit"/)
  assert.match(validateLicense('apache-2.0').reason, /"Apache-2.0"/)
})

test('engines are required, and their form is checked but never satisfied', () => {
  for (const spec of ['1.4.0', '^1.4', '~1.4', '>=1.4', 'v2']) {
    assert.equal(parse({ engines: { agentrq: spec } }).ok, true, spec)
  }
  assert.match(validateEngines(undefined).reason, /"engines" is required/)
  assert.match(validateEngines([]).reason, /must be an object/)
  assert.match(validateEngines({}).reason, /"engines.agentrq" is required/)
  assert.match(validateEngines({ agentrq: '>1.4' }).reason, /not a range this understands/)
  assert.match(validateEngines({ agentrq: 'latest' }).reason, /Supported forms/)
})

test('validateToolList refuses a list that is not one, and a tool named twice', () => {
  assert.deepEqual(validateToolList(undefined, 'tools'), { ok: true, tools: [] })
  assert.deepEqual(validateToolList(['a', 'b'], 'tools'), { ok: true, tools: ['a', 'b'] })
  assert.match(validateToolList('a', 'tools').reason, /must be a list of tool names/)
  assert.match(validateToolList([''], 'tools').reason, /not a tool name/)
  assert.match(validateToolList(['a', 'a'], 'workspace').reason, /"mcp.workspace" lists "a" twice/)
})

test('validateMcp reports whichever surface is wrong', () => {
  assert.deepEqual(validateMcp(undefined).mcp, { tools: [], workspace: [] })
  assert.match(validateMcp('tools').reason, /"mcp" must be an object/)
  assert.match(validateMcp({ tools: 'a' }).reason, /"mcp.tools"/)
  assert.match(validateMcp({ workspace: 'a' }).reason, /"mcp.workspace"/)
})

test('a hook name nobody recognises is an error, not a warning', () => {
  // Deliberately stricter than an unknown top-level key: a hook is a say over
  // the user's own answer, so an unrecognised one must never be read as a
  // smaller version of one that is recognised.
  assert.match(validateHooks({ onThing: 'deny' }).reason, /Unknown hook: "onThing"/)
  assert.match(validateHooks({ a: 1, b: 2 }).reason, /Unknown hooks: "a", "b"/)
})

test('a hook level must be one of the two the app offers', () => {
  assert.deepEqual(validateHooks(undefined), { ok: true, hooks: {} })
  assert.deepEqual(validateHooks({}), { ok: true, hooks: {} })
  assert.deepEqual(validateHooks({ toolCall: '' }), { ok: true, hooks: {} })
  assert.deepEqual(validateHooks({ toolCall: 'deny' }).hooks, { toolCall: 'deny' })
  assert.match(validateHooks([]).reason, /must be an object/)
  assert.match(validateHooks({ toolCall: 'allow' }).reason, /must be "deny".*or "decide"/s)
})

test('declaring the power to approve tool calls is published as a warning', () => {
  const parsed = parse({ hooks: { toolCall: 'decide' } })
  assert.equal(parsed.ok, true)
  assert.match(parsed.warnings.join('\n'), /approve tool calls on your behalf/)
})

test('net takes hostnames, not URLs', () => {
  assert.deepEqual(validateNet(undefined), { ok: true, net: [] })
  assert.deepEqual(validateNet(['a.example.com']).net, ['a.example.com'])
  assert.match(validateNet('a.example.com').reason, /must be a list of hostnames/)
  assert.match(validateNet(['https://a.example.com/x']).reason, /which is not a hostname/)
  assert.match(validateNet(['a.example.com:443']).reason, /no scheme or path/)
  assert.match(validateNet(['a.test', 'a.test']).reason, /lists "a.test" twice/)
})

test('config fields need a key and a type the app can render', () => {
  assert.deepEqual(validateConfig(undefined), { ok: true, config: [] })
  assert.match(validateConfig('x').reason, /must be a list of fields/)
  assert.match(validateConfig(['x']).reason, /not a field/)
  assert.match(validateConfig([{}]).reason, /needs a "key"/)
  assert.match(validateConfig([{ key: 'a' }, { key: 'a' }]).reason, /declares "a" twice/)
  assert.match(validateConfig([{ key: 'a', type: 'json' }]).reason, /unknown type "json"/)
})

test('parseDrawerEntry refuses anything that could name a file outside the package', () => {
  assert.equal(parseDrawerEntry('drawers/gantt.js'), 'drawers/gantt.js')
  assert.equal(parseDrawerEntry('gantt.mjs'), 'gantt.mjs')
  for (const bad of ['../secrets.js', '/etc/passwd.js', 'drawers\\gantt.js', '.hidden.js', 'drawers/../x.js', '', 'gantt.ts', `${'a'.repeat(201)}.js`]) {
    assert.equal(parseDrawerEntry(bad), '', bad)
  }
})

test('drawers are refused with a reason rather than silently dropped', () => {
  assert.deepEqual(validateProvides(undefined).provides, { drawers: [] })
  assert.deepEqual(validateProvides({}).provides, { drawers: [] })
  assert.match(validateProvides('x').reason, /"provides" must be an object/)
  assert.match(validateProvides({ drawers: 'x' }).reason, /must be an array/)
  assert.match(validateProvides({ drawers: [{ entry: 'a.js' }] }).reason, /is not a diagram format/)
  assert.match(
    validateProvides({ drawers: [{ format: 'gantt', entry: 'gantt.js' }, { format: 'GANTT', entry: 'b.js' }] }).reason,
    /declares "gantt" twice/,
  )
  assert.match(validateProvides({ drawers: [{ format: 'gantt', entry: '../x.js' }] }).reason, /needs an "entry"/)
})

test('an artifact is optional, but a half-written one is an error', () => {
  assert.deepEqual(validateArtifact(undefined), { ok: true, artifact: null })
  assert.match(validateArtifact('v1').reason, /must be an object/)
  assert.match(validateArtifact({}).reason, /"artifact.release" is required/)
  assert.match(validateArtifact({ release: 'v1' }).reason, /"artifact.asset" is required/)
  assert.match(validateArtifact({ release: 'v1', asset: 'a.tgz' }).reason, /64-character hex SHA-256/)
  assert.match(validateArtifact({ release: 'v1', asset: 'a.tgz', sha256: 'zz' }).reason, /SHA-256/)
  assert.equal(validateArtifact({ release: 'v1', asset: 'a.tgz', sha256: 'A'.repeat(64) }).artifact.sha256, 'a'.repeat(64))
})

test('a connector with nothing to install yet is listed, with a warning', () => {
  const parsed = parse()
  assert.equal(parsed.ok, true)
  assert.match(parsed.warnings.join('\n'), /nothing for the app to install yet/)
})

test('an unknown top-level key is a warning, because the app may be newer than this index', () => {
  const parsed = parse({ surfaces: ['panel'], artifact: { release: 'v1', asset: 'a.tgz', sha256: 'a'.repeat(64) } })
  assert.equal(parsed.ok, true, 'a nightly job must not break a connector written for a newer app')
  assert.match(parsed.warnings.join('\n'), /Unknown key in .*: "surfaces"/)

  const two = parseManifest(manifest({ a: 1, b: 2 }))
  assert.match(two.warnings.join('\n'), /Unknown keys .*"a", "b"/)
})

test('UNLICENSED is accepted and said out loud', () => {
  const parsed = parse({ license: 'UNLICENSED' })
  assert.equal(parsed.ok, true)
  assert.match(parsed.warnings.join('\n'), /no permission to use it is granted/)
})

test('keywords are capped rather than republished whole', () => {
  const keywords = Array.from({ length: 40 }, (_, i) => `k${i}`)
  assert.equal(parse({ keywords }).manifest.keywords.length, 16)
  assert.deepEqual(parse({ keywords: 'jira' }).manifest.keywords, [], 'a string is not a keyword list')
})

test('the known-key set is exactly what the manifest documents', () => {
  assert.deepEqual(
    [...KNOWN_KEYS].sort(),
    ['artifact', 'config', 'description', 'displayName', 'engines', 'homepage', 'hooks', 'keywords', 'license', 'mcp', 'name', 'net', 'provides', 'version'],
  )
})

test('validation stops at the first error, so the reason names one thing to fix', () => {
  const parsed = parseManifest(JSON.stringify({ name: 'jira', version: '1.0.0', description: 'x', license: 'nope', net: 'also wrong' }))
  assert.equal(parsed.ok, false)
  assert.match(parsed.reason, /SPDX/)
  assert.doesNotMatch(parsed.reason, /net/)
})
