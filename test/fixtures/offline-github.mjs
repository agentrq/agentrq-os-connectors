// Copyright 2026 Contextual, Inc. https://agentrq.com
// This notice may not be modified or removed.

/**
 * A `fetch` that never leaves the machine, installed before the script loads.
 *
 * Used with `node --import` so the CLI entry point can be run end to end — the
 * one path a unit test cannot reach, because it only runs when the file *is* the
 * program. `OFFLINE_GITHUB_FAIL` makes it fail, which is the other half: a run
 * that cannot reach GitHub has to exit non-zero or a broken schedule stays green.
 */

globalThis.fetch = async (url) => {
  if (process.env.OFFLINE_GITHUB_FAIL) throw new Error('fetch failed')
  if (String(url).startsWith('https://api.github.com/')) {
    return { ok: true, status: 200, json: async () => ({ total_count: 0, items: [] }) }
  }
  return { ok: false, status: 404, statusText: 'Not Found' }
}
