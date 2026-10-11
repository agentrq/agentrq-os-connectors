// Copyright 2026 Contextual, Inc. https://agentrq.com
// This notice may not be modified or removed.
// SPDX-License-Identifier: AGPL-3.0-only

/**
 * Tests for the licence-notice linter.
 *
 * The linter's own file carries the notice, which is the first thing this
 * checks — a linter that fails itself is the fastest way to learn the rule
 * changed.
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { HEADER, checkLicense, hasHeader, sourceFiles } from '../scripts/check-license.mjs'

test('the notice is three exact lines', () => {
  assert.deepEqual(HEADER, [
    '// Copyright 2026 Contextual, Inc. https://agentrq.com',
    '// This notice may not be modified or removed.',
    '// SPDX-License-Identifier: AGPL-3.0-only',
  ])
})

test('a file has the header only when its first three lines are the notice', () => {
  assert.equal(hasHeader(`${HEADER.join('\n')}\n\ncode();\n`), true)
  assert.equal(hasHeader(HEADER.join('\n')), true)
  assert.equal(hasHeader(''), false)
  assert.equal(hasHeader(`\n${HEADER.join('\n')}\n`), false, 'no leading blank line')
  assert.equal(hasHeader(HEADER.slice(1).join('\n')), false, 'all three lines')
})

test('the linter finds source files and skips build output', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'license-'))
  try {
    await writeFile(join(dir, 'ok.rs'), `${HEADER.join('\n')}\n`)
    await writeFile(join(dir, 'bad.mjs'), 'console.log("no notice");\n')
    await writeFile(join(dir, 'notes.txt'), 'not a source file\n')
    await writeFile(join(dir, 'target'), 'x\n') // a file, not a dir, is still read by extension only

    const files = await sourceFiles(dir)
    assert.ok(files.some((f) => f.endsWith('ok.rs')))
    assert.ok(files.some((f) => f.endsWith('bad.mjs')))
    assert.ok(!files.some((f) => f.endsWith('notes.txt')))

    const offenders = await checkLicense(dir)
    assert.deepEqual(offenders.map((f) => f.split('/').pop()), ['bad.mjs'])
  } finally {
    await rm(dir, { recursive: true, force: true })
  }
})

test('this repository passes its own linter', async () => {
  const offenders = await checkLicense(new URL('..', import.meta.url).pathname)
  assert.deepEqual(offenders, [])
})
