# AGENTS.md

Conventions for working in this repository (agentrq-os-connectors). Short and
durable: add an entry only when it's worth a future session knowing.

## Tool design principles (critical)
- Connectors are NOT 1:1 mappings of the underlying API. Design tools for the
  best agent experience: outcome-oriented tools that complete a task in a
  single call.
- One tool may — and should, when it serves the outcome — make multiple HTTP
  calls internally to achieve its result. Never push multi-call choreography
  onto the agent when the connector can do it.
- Prefer fewer, task-shaped tools over exhaustive endpoint coverage. Add a
  low-level tool only when it is a genuinely useful primitive for agents.

## Connector quality bar
- Every connector must apply Rust best practices, software design best
  practices, and sound data structures & algorithms: idiomatic error handling
  (`?`, no `unwrap`/`expect` in library paths), borrow instead of clone,
  small focused functions, shared private helpers instead of duplicated
  request/response plumbing, efficient URL and query-string building, and a
  clean `cargo clippy --all-targets`.
- The public tool contract is frozen: tool names, input schemas, manifest.json
  contents, and observable behavior must not change in a refactor. Fix genuine
  bugs explicitly and call them out in the PR description.
- 100% line and function coverage is mandatory for every connector, and 100%
  region coverage after exclusions (`node scripts/check-coverage.mjs` from the
  connector directory; wired as the CI coverage gate). A region that cannot be
  covered gets a `// coverage: exclude — <reason>` note on its line — the
  reason is mandatory, and the linter rejects notes gone stale. Prefer a test
  over a note; the note is for the genuinely uncoverable.
  `cargo fmt --all -- --check` must be clean.
- Every published connector's `manifest.json` must carry a non-empty `icons`
  array and a non-empty `instructions` string (guidance for the model using
  the connector). The catalogue does not list a connector without them —
  enforced by `scripts/manifest.mjs`, the `agentrq-os-connectors-template`
  linter, and the nightly build. `connectors-manifest.json` must list
  connectors in alphanumeric order by manifest path (code-point comparison, so
  `github-pat/…` sorts before `github/…`) — same enforcement.
- The connector `manifest.json` files have mixed formatting (some pretty-printed
  2-space, some compact single-line). When editing one, preserve its existing
  format — insert keys as text rather than re-serializing the whole file.

## Learning resources
- Rust: read https://github.com/leonardomso/rust-skills/blob/master/SKILL.md
  before writing or refactoring Rust. It holds 265 rules across 26 categories
  (ownership, error handling, API design, serde, testing, anti-patterns) —
  learn from it instead of guessing.
