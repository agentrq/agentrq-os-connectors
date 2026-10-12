// Copyright 2026 Contextual, Inc. https://agentrq.com
// This notice may not be modified or removed.
// SPDX-License-Identifier: AGPL-3.0-only

/**
 * The release: every connector this repository lists, built as a component.
 *
 * The catalogue lists a repository only when its latest release carries a
 * `<connector name>.wasm` for each connector in `connectors-manifest.json`, and
 * the server installs exactly that asset. This script makes those assets.
 *
 * ## Why it swaps the SDK
 *
 * `sdk/connectors-sdk` in this repository is a host-side test double: its
 * `export_connector!` exports nothing, so a component built against it would
 * install and then do nothing. The real SDK lives in `agentrq/connectors`. Each
 * connector is therefore copied to a staging directory, its `connectors-sdk`
 * dependency is pointed at the real one, and it is built there, leaving the
 * working tree (and the shim the tests use) untouched.
 *
 * ## Why it refuses a remote manifest
 *
 * A manifest with a `remote` block has no code to build, and the server installs
 * a listed connector only from its release asset. Listing one in the index would
 * leave a connector that cannot be installed, so this fails rather than publish
 * a release the catalogue then rejects. A remote MCP server is published from
 * its own repository, under the `agentrq-remote-connector` topic.
 *
 * Every side effect is an argument, so the whole of it is testable without
 * cargo, a network or the real SDK.
 */

import { spawn } from 'node:child_process'
import { cp, mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { basename, dirname, join, resolve } from 'node:path'
import { argv, env, exit } from 'node:process'
import { parseArgs } from 'node:util'

import { INDEX_FILE, MANIFEST_FILE, parseIndex, parseManifest } from './manifest.mjs'

/** The target the server runs: a component, with no `cargo component` step. */
export const TARGET = 'wasm32-wasip2'

/** `\0asm`, then version 0x0d and layer 1: a component, not a core module. */
export const COMPONENT_HEADER = Uint8Array.from([0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00])

/** The one dependency line that names the SDK, whatever it points at. */
const SDK_DEPENDENCY = /^connectors-sdk\s*=\s*\{[^}\n]*\}[ \t]*$/m

const PACKAGE_NAME = /^\[package\][^[]*?^name\s*=\s*"([^"]+)"/m

/**
 * The connectors to build, in the index's order, each with its directory and crate.
 *
 * A repository without an index publishes the one connector at its root.
 */
export async function releaseTargets(readText) {
  const index = parseIndex(await readText(INDEX_FILE))
  if (!index.ok) throw new Error(index.reason)

  const targets = []
  for (const path of index.index ? index.index.manifests : [MANIFEST_FILE]) {
    const parsed = parseManifest(await readText(path), { path })
    if (!parsed.ok) throw new Error(parsed.reason)
    const { name, remote } = parsed.manifest
    if (remote) {
      throw new Error(
        `${path} describes a remote MCP server, which has no component to release. ` +
          `Remove it from ${INDEX_FILE} and publish it under the agentrq-remote-connector topic instead.`,
      )
    }

    const dir = dirname(path)
    const cargoToml = await readText(join(dir, 'Cargo.toml'))
    if (cargoToml === null) throw new Error(`${path} has no Cargo.toml beside it to build.`)
    targets.push({ name, dir, crate: crateName(cargoToml, dir) })
  }
  return targets
}

/** The `[package]` name in a `Cargo.toml`. */
export function crateName(cargoToml, dir) {
  const match = PACKAGE_NAME.exec(cargoToml)
  if (!match) throw new Error(`${dir}/Cargo.toml has no [package] name.`)
  return match[1]
}

/** The file cargo writes for a cdylib crate: hyphens become underscores. */
export function artifactFile(crate) {
  return `${crate.replaceAll('-', '_')}.wasm`
}

/** A `Cargo.toml` whose `connectors-sdk` is the real SDK at `sdkPath`. */
export function useRealSdk(cargoToml, sdkPath, dir) {
  if (!SDK_DEPENDENCY.test(cargoToml)) throw new Error(`${dir}/Cargo.toml has no connectors-sdk dependency to replace.`)
  return cargoToml.replace(SDK_DEPENDENCY, `connectors-sdk = { path = ${JSON.stringify(sdkPath)} }`)
}

/** Whether `bytes` starts like a component rather than a core module. */
export function isComponent(bytes) {
  return COMPONENT_HEADER.every((byte, i) => bytes[i] === byte)
}

/** The real side effects, for the CLI. */
export const nodeIo = {
  readText: async (path) => {
    try {
      return await readFile(path, 'utf8')
    } catch (error) {
      if (error.code === 'ENOENT') return null
      throw error
    }
  },
  readBytes: (path) => readFile(path),
  writeText: (path, contents) => writeFile(path, contents),
  writeBytes: async (path, bytes) => {
    await mkdir(dirname(path), { recursive: true })
    await writeFile(path, bytes)
  },
  // Build output is skipped: it is large, and it belongs to the shim build.
  copyDir: (from, to) => cp(from, to, { recursive: true, filter: (source) => basename(source) !== 'target' }),
  tempDir: () => mkdtemp(join(tmpdir(), 'agentrq-release-')),
  run: (command, args) =>
    new Promise((done, failed) => {
      const child = spawn(command, args, { stdio: 'inherit' })
      child.on('error', failed)
      child.on('close', (code) => (code === 0 ? done() : failed(new Error(`${command} ${args.join(' ')} exited with ${code}.`))))
    }),
}

/**
 * Builds every listed connector against the real SDK into `out/<name>.wasm`.
 *
 * All of them share one target directory, so the SDK and its dependencies are
 * compiled once rather than once per connector.
 */
export async function buildRelease({ root = '.', sdk, out = 'dist', cargo = env.CARGO ?? 'cargo', io = nodeIo, log = console.log }) {
  if (!sdk) throw new Error('Pass --sdk with the path to the real connectors-sdk (sdk/connectors-sdk of agentrq/connectors).')
  const sdkPath = resolve(sdk)
  const targets = await releaseTargets((path) => io.readText(join(root, path)))
  const work = await io.tempDir()
  const targetDir = join(work, 'target')

  const built = []
  for (const { name, dir, crate } of targets) {
    const staged = join(work, 'src', dir)
    await io.copyDir(join(root, dir), staged)
    const manifestPath = join(staged, 'Cargo.toml')
    await io.writeText(manifestPath, useRealSdk(await io.readText(manifestPath), sdkPath, dir))

    log(`Building ${name} (${dir}) for ${TARGET}…`)
    await io.run(cargo, ['build', '--release', '--target', TARGET, '--manifest-path', manifestPath, '--target-dir', targetDir])

    const bytes = await io.readBytes(join(targetDir, TARGET, 'release', artifactFile(crate)))
    if (!isComponent(bytes)) throw new Error(`${dir} built a core wasm module, not a component.`)
    const asset = join(out, `${name}.wasm`)
    await io.writeBytes(asset, bytes)
    built.push({ name, asset, size: bytes.length })
  }

  log(`Built ${built.length} components into ${out}.`)
  return built
}

const invokedDirectly = argv[1] && import.meta.url === new URL(`file://${resolve(argv[1])}`).href

if (invokedDirectly) {
  const run = async () => {
    const { values } = parseArgs({ options: { sdk: { type: 'string' }, out: { type: 'string', default: 'dist' } } })
    await buildRelease(values)
  }
  run().catch((error) => {
    console.error(error.message)
    exit(1)
  })
}
