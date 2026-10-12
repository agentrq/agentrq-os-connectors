// Copyright 2026 Contextual, Inc. https://agentrq.com
// This notice may not be modified or removed.
// SPDX-License-Identifier: AGPL-3.0-only

/**
 * The coverage bar for a connector, and the one way to be exempt from it.
 *
 * Two numbers matter:
 *
 * - **Soft coverage** is line coverage. Every line must be executed by the
 *   test suite: 100%, no exceptions. A line that never runs is code nobody
 *   has watched work.
 * - **Hard coverage** is region coverage. Every region — each branch arm,
 *   each `?` error path, each closure body — must be taken: 100%, except
 *   where an exclusion note says otherwise. A region that never runs is a
 *   decision nobody has watched get made.
 *
 * The exemption is a source note, on the same line as the uncovered code:
 *
 * ```rust
 * let input: GetValues = request.json()?; // coverage: exclude — the test harness builds requests from valid JSON, so the Err arm is unreachable in tests
 * ```
 *
 * The note must carry a reason. A note without one is rejected, and a note
 * on a line whose regions are all covered is rejected as stale — the moment
 * a test starts covering the line, the note has to go. That keeps every
 * exclusion an honest, reviewable claim instead of a quiet hole in the bar.
 *
 * Run it from a connector directory (or pass one): it runs
 * `cargo llvm-cov --json`, reads the report, and fails on any line,
 * function, or un-noted region below the bar.
 */

import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

/**
 * A `// coverage: exclude — <reason>` note. The separator may be an em
 * dash, an en dash, a hyphen, or a colon; the reason must be non-empty.
 */
const NOTE_RE = /\/\/\s*coverage:\s*exclude\b\s*(?:[—–\-:]\s*)?(.*?)\s*$/

/**
 * Reads the exclusion notes out of source text.
 *
 * @returns {Map<number, string>} 1-based line number to the note's reason.
 *   A note with no reason maps to the empty string; the checker rejects it.
 */
export function parseExclusionNotes(sourceText) {
  const notes = new Map()
  sourceText.split('\n').forEach((text, index) => {
    const match = NOTE_RE.exec(text)
    if (match) notes.set(index + 1, (match[1] ?? '').trim())
  })
  return notes
}

/**
 * The 1-based lines holding at least one region the test suite never
 * entered. A segment is `[line, col, count, hasCount, isRegionEntry,
 * isGapRegion]`; only region entries count, and gap regions are layout,
 * not decisions.
 *
 * @returns {Set<number>}
 */
export function uncoveredRegionLines(fileCoverage) {
  const lines = new Set()
  for (const segment of fileCoverage.segments ?? []) {
    const [line, , count, hasCount, isRegionEntry, isGapRegion] = segment
    if (hasCount && isRegionEntry && !isGapRegion && count === 0) {
      lines.add(line)
    }
  }
  return lines
}

/**
 * Checks one covered file against its source.
 *
 * @returns {Array<{ type: string, file: string, line: number, detail: string }>}
 *   `uncovered` (a region with no note), `empty-reason` (a note that says
 *   nothing), or `stale-note` (a note on a fully covered line).
 */
export function checkFile(filename, fileCoverage, sourceText) {
  const failures = []
  const uncovered = uncoveredRegionLines(fileCoverage)
  const notes = parseExclusionNotes(sourceText)
  const lines = sourceText.split('\n')

  for (const line of [...uncovered].sort((a, b) => a - b)) {
    if (!notes.has(line)) {
      failures.push({
        type: 'uncovered',
        file: filename,
        line,
        detail: (lines[line - 1] ?? '').trim(),
      })
    }
  }

  for (const [line, reason] of [...notes].sort((a, b) => a[0] - b[0])) {
    if (!reason) {
      failures.push({
        type: 'empty-reason',
        file: filename,
        line,
        detail: 'exclusion note carries no reason',
      })
    } else if (!uncovered.has(line)) {
      failures.push({
        type: 'stale-note',
        file: filename,
        line,
        detail: 'every region on this line is covered; the note has to go',
      })
    }
  }

  return failures
}

/**
 * Checks a whole llvm-cov JSON report.
 *
 * @param report the parsed `cargo llvm-cov --json` output.
 * @param readSource { (path: string) => string } reads a covered file.
 * @returns {{ failures: Array, summary: Array }} per-file line/region
 *   numbers for the report, and every failure across all files.
 */
export function checkReport(report, readSource) {
  const failures = []
  const summary = []
  const files = report?.data?.[0]?.files ?? []
  const totals = report?.data?.[0]?.totals ?? {}

  if (files.length === 0) {
    failures.push({
      type: 'no-data',
      file: '',
      line: 0,
      detail: 'the coverage report holds no files; refusing to pass vacuously',
    })
    return { failures, summary }
  }

  for (const kind of ['lines', 'functions']) {
    const stat = totals[kind]
    if (stat && stat.percent < 100) {
      failures.push({
        type: 'below-100',
        file: '',
        line: 0,
        detail: `${kind} coverage is ${stat.percent.toFixed(2)}% (${stat.covered}/${stat.count})`,
      })
    }
  }

  for (const file of files) {
    const filename = file.filename ?? ''
    let source = null
    try {
      source = readSource(filename)
    } catch {
      failures.push({
        type: 'unreadable',
        file: filename,
        line: 0,
        detail: 'covered file could not be read; exclusion notes cannot be checked',
      })
      continue
    }
    const uncovered = uncoveredRegionLines(file)
    const notes = parseExclusionNotes(source)
    const excluded = [...uncovered].filter((line) => notes.has(line) && notes.get(line)).length
    const regions = file.summary?.regions ?? {}
    const lines = file.summary?.lines ?? {}
    summary.push({
      file: filename,
      lines: `${lines.covered ?? '?'}/${lines.count ?? '?'}`,
      regions: `${regions.covered ?? '?'}/${regions.count ?? '?'} (+${excluded} excluded)`,
    })
    failures.push(...checkFile(filename, file, source))
  }

  return { failures, summary }
}

function main() {
  const dir = process.argv[2] ?? process.cwd()
  const scratch = mkdtempSync(join(tmpdir(), 'check-coverage-'))
  const jsonPath = join(scratch, 'coverage.json')

  try {
    execFileSync('cargo', ['llvm-cov', '--json', '--output-path', jsonPath], {
      cwd: dir,
      stdio: 'inherit',
    })
  } catch (error) {
    console.error('check-coverage: `cargo llvm-cov` failed to run.')
    console.error('Is cargo-llvm-cov installed? `cargo install cargo-llvm-cov`')
    process.exit(2)
  }

  let report
  try {
    report = JSON.parse(readFileSync(jsonPath, 'utf8'))
  } catch (error) {
    console.error(`check-coverage: could not parse the coverage report: ${error.message}`)
    process.exit(2)
  } finally {
    rmSync(scratch, { recursive: true, force: true })
  }

  const { failures, summary } = checkReport(report, (path) => readFileSync(path, 'utf8'))

  for (const row of summary) {
    console.log(`${row.file}\n  lines ${row.lines} · regions ${row.regions}`)
  }

  if (failures.length === 0) {
    console.log('\ncheck-coverage: 100% lines, 100% functions, 100% regions after exclusions.')
    return
  }

  console.error('\ncheck-coverage: failing.')
  for (const failure of failures) {
    const where = failure.file ? `${failure.file}:${failure.line}` : ''
    console.error(`  [${failure.type}] ${where} ${failure.detail}`)
  }
  process.exit(1)
}

// Only run when invoked directly; the test suite imports the pure functions.
if (import.meta.url === `file://${process.argv[1]}`) main()
