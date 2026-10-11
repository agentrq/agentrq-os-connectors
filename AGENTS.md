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
- 100% line and function coverage is mandatory for every connector
  (`cargo llvm-cov --fail-under-lines 100 --fail-under-functions 100`);
  `cargo fmt --all -- --check` must be clean.
