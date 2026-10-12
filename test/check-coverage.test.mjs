// Copyright 2026 Contextual, Inc. https://agentrq.com
// This notice may not be modified or removed.
// SPDX-License-Identifier: AGPL-3.0-only

/**
 * Tests for the coverage bar.
 *
 * The bar is two numbers — 100% of lines, 100% of regions — and one
 * exemption: a `// coverage: exclude — <reason>` note on the uncovered
 * line. The tests pin the exemption's honesty rules: a note must say why,
 * and a note on a covered line is stale and fails.
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'

import {
  checkFile,
  checkReport,
  parseExclusionNotes,
  uncoveredRegionLines,
} from '../scripts/check-coverage.mjs'

/** A minimal llvm-cov file entry with the given segments. */
function fileEntry(segments, regions = { covered: 1, count: 1 }) {
  return { filename: 'src/lib.rs', segments, summary: { regions } }
}

/** A segment tuple: [line, col, count, hasCount, isRegionEntry, isGapRegion]. */
const entry = (line, col, count) => [line, col, count, true, true, false]

test('parseExclusionNotes finds notes with reasons', () => {
  const notes = parseExclusionNotes(
    [
      'let a = 1;',
      'let b = foo()?; // coverage: exclude — infallible here',
      'let c = 2; // coverage: exclude: no reason is still a reason',
      'let d = 3; // coverage: exclude - hyphen works too',
    ].join('\n'),
  )
  assert.equal(notes.get(2), 'infallible here')
  assert.equal(notes.get(3), 'no reason is still a reason')
  assert.equal(notes.get(4), 'hyphen works too')
  assert.ok(!notes.has(1))
})

test('parseExclusionNotes keeps a note with no reason, for the checker to reject', () => {
  const notes = parseExclusionNotes('let b = foo()?; // coverage: exclude\n')
  assert.ok(notes.has(1))
  assert.equal(notes.get(1), '')
})

test('uncoveredRegionLines collects zero-count region entries', () => {
  const lines = uncoveredRegionLines(
    fileEntry([
      entry(10, 5, 0), // uncovered region
      entry(11, 5, 3), // covered region
      [12, 5, 0, true, false, false], // not a region entry
      [13, 5, 0, true, true, true], // a gap region, not a decision
      [14, 5, 0, false, true, false], // no count recorded
    ]),
  )
  assert.deepEqual([...lines], [10])
})

test('checkFile passes a fully covered file', () => {
  const failures = checkFile('src/lib.rs', fileEntry([entry(10, 5, 2)]), 'let a = 1;\n')
  assert.deepEqual(failures, [])
})

test('checkFile fails an uncovered line with no note', () => {
  const failures = checkFile(
    'src/lib.rs',
    fileEntry([entry(3, 8, 0)]),
    'let a = 1;\nlet b = 2;\nlet c = foo()?;\n',
  )
  assert.equal(failures.length, 1)
  assert.equal(failures[0].type, 'uncovered')
  assert.equal(failures[0].line, 3)
  assert.match(failures[0].detail, /foo\(\)\?/)
})

test('checkFile accepts an uncovered line with a reasoned note', () => {
  const failures = checkFile(
    'src/lib.rs',
    fileEntry([entry(1, 8, 0)]),
    'let c = foo()?; // coverage: exclude — infallible here\n',
  )
  assert.deepEqual(failures, [])
})

test('checkFile rejects a note with no reason', () => {
  const failures = checkFile(
    'src/lib.rs',
    fileEntry([entry(1, 8, 0)]),
    'let c = foo()?; // coverage: exclude\n',
  )
  assert.equal(failures.length, 1)
  assert.equal(failures[0].type, 'empty-reason')
})

test('checkFile rejects a stale note on a covered line', () => {
  const failures = checkFile(
    'src/lib.rs',
    fileEntry([entry(1, 8, 4)]),
    'let c = foo()?; // coverage: exclude — used to be uncovered\n',
  )
  assert.equal(failures.length, 1)
  assert.equal(failures[0].type, 'stale-note')
})

test('checkReport fails lines and functions below 100%', () => {
  const report = {
    data: [
      {
        totals: {
          lines: { covered: 9, count: 10, percent: 90 },
          functions: { covered: 5, count: 5, percent: 100 },
        },
        files: [fileEntry([entry(1, 8, 2)])],
      },
    ],
  }
  const { failures } = checkReport(report, () => 'let a = 1;\n')
  assert.equal(failures.length, 1)
  assert.equal(failures[0].type, 'below-100')
  assert.match(failures[0].detail, /lines/)
})

test('checkReport passes a clean report and summarizes it', () => {
  const report = {
    data: [
      {
        totals: {
          lines: { covered: 10, count: 10, percent: 100 },
          functions: { covered: 5, count: 5, percent: 100 },
        },
        files: [fileEntry([entry(1, 8, 0)], { covered: 0, count: 1 })],
      },
    ],
  }
  const source = 'let c = foo()?; // coverage: exclude — infallible here\n'
  const { failures, summary } = checkReport(report, () => source)
  assert.deepEqual(failures, [])
  assert.equal(summary.length, 1)
  assert.match(summary[0].regions, /\+1 excluded/)
})

test('checkReport fails a report with no files', () => {
  const { failures } = checkReport({ data: [{ totals: {}, files: [] }] }, () => '')
  assert.equal(failures.length, 1)
  assert.equal(failures[0].type, 'no-data')
})

test('checkReport fails a file it cannot read', () => {
  const report = {
    data: [{ totals: {}, files: [fileEntry([entry(1, 8, 0)])] }],
  }
  const { failures } = checkReport(report, () => {
    throw new Error('no such file')
  })
  assert.equal(failures.length, 1)
  assert.equal(failures[0].type, 'unreadable')
})
