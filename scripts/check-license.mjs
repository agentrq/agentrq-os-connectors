// Copyright 2026 Contextual, Inc. https://agentrq.com
// This notice may not be modified or removed.
// SPDX-License-Identifier: AGPL-3.0-only

/**
 * Every source file carries the licence notice.
 *
 * The notice names the copyright holder, forbids its own removal, and states
 * the SPDX identifier, so the terms a file is offered under travel with the
 * file instead of living only in the LICENSE at the root. This script is the
 * check that keeps it so: it walks the repository, reads every source file,
 * and fails listing the ones whose first three lines are not the notice.
 *
 * Run it directly (`node scripts/check-license.mjs`); CI runs it on every
 * pull request. It checks itself.
 */

import { readdir, readFile } from 'node:fs/promises'
import { extname, join } from 'node:path'
import { argv, exit } from 'node:process'

/** The first three lines of every source file, exactly. */
export const HEADER = [
  '// Copyright 2026 Contextual, Inc. https://agentrq.com',
  '// This notice may not be modified or removed.',
  '// SPDX-License-Identifier: AGPL-3.0-only',
]

/** The files this repository writes code in. */
const SOURCE_EXTENSIONS = new Set(['.rs', '.mjs'])

/** Directories that hold build output or vendored code, never source. */
const SKIP_DIRS = new Set(['.git', 'target', 'node_modules'])

/** Every source file under `dir`, recursively. */
export async function sourceFiles(dir) {
  const files = []
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      if (!SKIP_DIRS.has(entry.name)) files.push(...(await sourceFiles(join(dir, entry.name))))
    } else if (SOURCE_EXTENSIONS.has(extname(entry.name))) {
      files.push(join(dir, entry.name))
    }
  }
  return files.sort()
}

/** Whether `source` opens with the licence notice. */
export function hasHeader(source) {
  const lines = source.split('\n')
  return HEADER.every((line, i) => lines[i] === line)
}

/** The source files under `dir` whose first three lines are not the notice. */
export async function checkLicense(dir = '.') {
  const offenders = []
  for (const file of await sourceFiles(dir)) {
    if (!hasHeader(await readFile(file, 'utf8'))) offenders.push(file)
  }
  return offenders
}

const invokedDirectly = argv[1] && import.meta.url === new URL(`file://${argv[1]}`).href

if (invokedDirectly) {
  const offenders = await checkLicense()
  if (offenders.length > 0) {
    console.error(
      `${offenders.length} source file${offenders.length > 1 ? 's' : ''} missing the licence notice:\n` +
        offenders.map((file) => `  ${file}`).join('\n'),
    )
    exit(1)
  }
  console.log('Every source file carries the licence notice.')
}
