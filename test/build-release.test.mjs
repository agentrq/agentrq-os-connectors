// Copyright 2026 Contextual, Inc. https://agentrq.com
// This notice may not be modified or removed.
// SPDX-License-Identifier: AGPL-3.0-only

/**
 * Tests for the release builder.
 *
 * `buildRelease` takes its file system and cargo as arguments, so most of this
 * runs in memory. The last tests run it as a program, against a scratch
 * repository and a fake cargo that writes a component header, so the real side
 * effects are exercised too — still without a toolchain or the real SDK.
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { chmod, mkdir, mkdtemp, readFile, readdir, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { execPath } from 'node:process'
import { fileURLToPath } from 'node:url'

import {
  COMPONENT_HEADER,
  TARGET,
  artifactFile,
  buildRelease,
  crateName,
  isComponent,
  nodeIo,
  releaseTargets,
  useRealSdk,
} from '../scripts/build-release.mjs'

const SCRIPT_PATH = fileURLToPath(new URL('../scripts/build-release.mjs', import.meta.url))
const REPO_ROOT = fileURLToPath(new URL('..', import.meta.url))

const manifest = (name, extra = {}) =>
  JSON.stringify({
    manifestVersion: 1,
    mcpVersion: '2026-07-28',
    name,
    version: '0.1.0',
    description: `${name} things.`,
    instructions: `Use ${name}.`,
    icons: [{ src: 'data:image/svg+xml;base64,AA==' }],
    tools: [{ name: 'go', inputSchema: { type: 'object' } }],
    ...extra,
  })

const cargoToml = (crate) =>
  [
    '[package]',
    `name = "${crate}"`,
    'version = "0.1.0"',
    '',
    '[lib]',
    'crate-type = ["cdylib"]',
    '',
    '[dependencies]',
    'connectors-sdk = { path = "../sdk/connectors-sdk" }',
    'serde_json = "1"',
    '',
  ].join('\n')

const index = (...paths) => JSON.stringify({ manifestVersion: 1, connectors: paths.map((path) => ({ manifest: path })) })

const COMPONENT = Uint8Array.from([...COMPONENT_HEADER, 0x01, 0x02])
const CORE_MODULE = Uint8Array.from([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00])

/** A file system in a map, and cargo as a function that drops bytes where cargo would. */
function memoryIo(files, { bytesFor = () => COMPONENT } = {}) {
  const fs = new Map(Object.entries(files))
  const calls = []
  return {
    fs,
    calls,
    io: {
      readText: async (path) => fs.get(path) ?? null,
      readBytes: async (path) => {
        if (!fs.has(path)) throw new Error(`ENOENT ${path}`)
        return fs.get(path)
      },
      writeText: async (path, contents) => fs.set(path, contents),
      writeBytes: async (path, bytes) => fs.set(path, bytes),
      copyDir: async (from, to) => {
        for (const [path, contents] of [...fs]) {
          if (path.startsWith(`${from}/`)) fs.set(`${to}${path.slice(from.length)}`, contents)
        }
      },
      tempDir: async () => '/work',
      run: async (command, args) => {
        calls.push([command, ...args])
        const manifestPath = args[args.indexOf('--manifest-path') + 1]
        const targetDir = args[args.indexOf('--target-dir') + 1]
        const crate = crateName(fs.get(manifestPath), manifestPath)
        fs.set(join(targetDir, TARGET, 'release', artifactFile(crate)), bytesFor(crate))
      },
    },
  }
}

test('crateName reads the [package] name, not a dependency or a later table', () => {
  assert.equal(crateName(cargoToml('github-pat'), 'github-pat'), 'github-pat')
  assert.equal(crateName('[lib]\nname = "nope"\n[package]\nversion = "1"\nname = "yes"\n', 'x'), 'yes')
  assert.throws(() => crateName('[lib]\nname = "nope"\n', 'x'), /x\/Cargo.toml has no \[package\] name/)
})

test('artifactFile names the file cargo writes for a cdylib', () => {
  assert.equal(artifactFile('github-pat'), 'github_pat.wasm')
  assert.equal(artifactFile('gcal'), 'gcal.wasm')
})

test('useRealSdk points connectors-sdk at the real SDK and touches nothing else', () => {
  const swapped = useRealSdk(cargoToml('gcal'), '/sdk/connectors-sdk', 'gcal')
  assert.match(swapped, /^connectors-sdk = \{ path = "\/sdk\/connectors-sdk" \}$/m)
  assert.equal(swapped.replace(/^connectors-sdk = .*$/m, ''), cargoToml('gcal').replace(/^connectors-sdk = .*$/m, ''))
  assert.throws(() => useRealSdk('[package]\nname = "x"\n', '/sdk', 'x'), /x\/Cargo.toml has no connectors-sdk dependency/)
})

test('isComponent tells a component from a core module', () => {
  assert.equal(isComponent(COMPONENT), true)
  assert.equal(isComponent(CORE_MODULE), false)
  assert.equal(isComponent(new Uint8Array()), false)
})

test('releaseTargets reads the index, in order, with each directory and crate', async () => {
  const { io } = memoryIo({
    'connectors-manifest.json': index('gcal/manifest.json', 'github-pat/manifest.json'),
    'gcal/manifest.json': manifest('gcal'),
    'gcal/Cargo.toml': cargoToml('gcal'),
    'github-pat/manifest.json': manifest('github-pat'),
    'github-pat/Cargo.toml': cargoToml('github-pat'),
  })
  assert.deepEqual(await releaseTargets(io.readText), [
    { name: 'gcal', dir: 'gcal', crate: 'gcal' },
    { name: 'github-pat', dir: 'github-pat', crate: 'github-pat' },
  ])
})

test('releaseTargets falls back to the root manifest when there is no index', async () => {
  const { io } = memoryIo({ 'manifest.json': manifest('solo'), 'Cargo.toml': cargoToml('solo-connector') })
  assert.deepEqual(await releaseTargets(io.readText), [{ name: 'solo', dir: '.', crate: 'solo-connector' }])
})

test('releaseTargets refuses what the catalogue would refuse', async () => {
  const cases = [
    [{ 'connectors-manifest.json': '{' }, /connectors-manifest.json is not valid JSON/],
    [{ 'connectors-manifest.json': index('a/manifest.json'), 'a/manifest.json': manifest('a', { manifestVersion: '1.0.0' }) }, /"manifestVersion" must be 1/],
    [{ 'connectors-manifest.json': index('a/manifest.json'), 'a/manifest.json': manifest('a') }, /a\/manifest.json has no Cargo.toml beside it/],
  ]
  for (const [files, reason] of cases) {
    await assert.rejects(() => releaseTargets(memoryIo(files).io.readText), reason)
  }
})

test('releaseTargets refuses a remote manifest, which has nothing to build', async () => {
  const { io } = memoryIo({
    'connectors-manifest.json': index('notion/manifest.json'),
    'notion/manifest.json': manifest('notion', { tools: [], remote: { url: 'https://mcp.notion.com/mcp' } }),
  })
  await assert.rejects(() => releaseTargets(io.readText), /notion\/manifest.json describes a remote MCP server.*agentrq-remote-connector/)
})

test('buildRelease builds each connector against the real SDK into <name>.wasm', async () => {
  const { fs, calls, io } = memoryIo({
    'repo/connectors-manifest.json': index('gcal/manifest.json', 'github-pat/manifest.json'),
    'repo/gcal/manifest.json': manifest('gcal'),
    'repo/gcal/Cargo.toml': cargoToml('gcal'),
    'repo/gcal/src/lib.rs': '// gcal',
    'repo/github-pat/manifest.json': manifest('github-pat'),
    'repo/github-pat/Cargo.toml': cargoToml('github-pat'),
  })
  const logs = []

  const built = await buildRelease({ root: 'repo', sdk: '/sdk/connectors-sdk', out: 'dist', cargo: 'cargo', io, log: (line) => logs.push(line) })

  assert.deepEqual(built, [
    { name: 'gcal', asset: 'dist/gcal.wasm', size: COMPONENT.length },
    { name: 'github-pat', asset: 'dist/github-pat.wasm', size: COMPONENT.length },
  ])
  assert.deepEqual(fs.get('dist/github-pat.wasm'), COMPONENT, 'the asset is named after the connector, not the crate file')
  assert.equal(fs.get('/work/src/gcal/src/lib.rs'), '// gcal', 'the connector is built from a staged copy')
  assert.match(fs.get('/work/src/gcal/Cargo.toml'), /connectors-sdk = \{ path = "\/sdk\/connectors-sdk" \}/)
  assert.equal(fs.get('repo/gcal/Cargo.toml'), cargoToml('gcal'), 'the working tree keeps the shim')
  assert.deepEqual(calls[0], [
    'cargo', 'build', '--release', '--target', TARGET,
    '--manifest-path', '/work/src/gcal/Cargo.toml', '--target-dir', '/work/target',
  ])
  assert.equal(calls[1].at(-1), '/work/target', 'one target directory is shared by every build')
  assert.match(logs.at(-1), /Built 2 components into dist/)
})

test('buildRelease refuses a build that is not a component', async () => {
  const { io } = memoryIo(
    { 'connectors-manifest.json': index('a/manifest.json'), 'a/manifest.json': manifest('a'), 'a/Cargo.toml': cargoToml('a') },
    { bytesFor: () => CORE_MODULE },
  )
  await assert.rejects(() => buildRelease({ sdk: '/sdk', io, log: () => {} }), /a built a core wasm module, not a component/)
})

test('buildRelease needs to be told where the real SDK is', async () => {
  await assert.rejects(() => buildRelease({ io: memoryIo({}).io }), /Pass --sdk/)
})

test('nodeIo reads a missing file as absence, and rethrows anything else', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'release-io-'))
  assert.equal(await nodeIo.readText(join(dir, 'missing')), null)
  await assert.rejects(() => nodeIo.readText(dir), { code: 'EISDIR' })
})

test('nodeIo.run rejects a command that fails or cannot start', async () => {
  await nodeIo.run(execPath, ['-e', ''])
  await assert.rejects(() => nodeIo.run(execPath, ['-e', 'process.exit(3)']), /exited with 3/)
  await assert.rejects(() => nodeIo.run(join(tmpdir(), 'no-such-command-anywhere'), []), { code: 'ENOENT' })
})

/** A cargo that writes a component where the real one would write the build. */
const FAKE_CARGO = `#!${execPath}
const { mkdirSync, readFileSync, writeFileSync } = require('node:fs')
const { join } = require('node:path')
const args = process.argv.slice(2)
const manifest = args[args.indexOf('--manifest-path') + 1]
const targetDir = args[args.indexOf('--target-dir') + 1]
const crate = /name = "([^"]+)"/.exec(readFileSync(manifest, 'utf8'))[1].replaceAll('-', '_')
const out = join(targetDir, '${TARGET}', 'release')
mkdirSync(out, { recursive: true })
writeFileSync(join(out, crate + '.wasm'), Buffer.from([${[...COMPONENT].join(', ')}]))
`

function runCli(cwd, args, env = {}) {
  return new Promise((done) => {
    const child = spawn(execPath, [SCRIPT_PATH, ...args], { cwd, env: { ...process.env, ...env } })
    let out = ''
    child.stdout.on('data', (chunk) => (out += chunk))
    child.stderr.on('data', (chunk) => (out += chunk))
    child.on('close', (code) => done({ code, out }))
  })
}

test('run as a program, it writes a <name>.wasm per connector', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'release-cli-'))
  await mkdir(join(cwd, 'gcal', 'target'), { recursive: true })
  await writeFile(join(cwd, 'connectors-manifest.json'), index('gcal/manifest.json'))
  await writeFile(join(cwd, 'gcal', 'manifest.json'), manifest('gcal'))
  await writeFile(join(cwd, 'gcal', 'Cargo.toml'), cargoToml('gcal'))
  await writeFile(join(cwd, 'gcal', 'target', 'stale.wasm'), 'shim build output')
  const cargo = join(cwd, 'fake-cargo')
  await writeFile(cargo, FAKE_CARGO)
  await chmod(cargo, 0o755)

  const { code, out } = await runCli(cwd, ['--sdk', 'sdk/connectors-sdk', '--out', 'release'], { CARGO: cargo })

  assert.equal(code, 0, out)
  assert.match(out, /Built 1 components into release/)
  assert.deepEqual(await readdir(join(cwd, 'release')), ['gcal.wasm'])
  assert.deepEqual(new Uint8Array(await readFile(join(cwd, 'release', 'gcal.wasm'))), COMPONENT)
})

test('run as a program, it fails with the reason and a non-zero exit', async () => {
  const { code, out } = await runCli(await mkdtemp(join(tmpdir(), 'release-cli-')), [])
  assert.equal(code, 1)
  assert.match(out, /Pass --sdk/)
})

test('this repository lists only connectors it can release', async () => {
  const targets = await releaseTargets((path) => nodeIo.readText(resolve(REPO_ROOT, path)))
  assert.ok(targets.length > 0)
  for (const { name, crate } of targets) assert.equal(crate, name, `${name} builds a crate of the same name`)
})
