# gslides — Google Slides connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the Google Slides API v1.

Unlike a 1:1 mapping of the API, the tools are **outcome-shaped**: each one
finishes a job in a single call, even when the connector makes several HTTP
requests to get there. `addSlide` creates the slide, reads its title and body
placeholders back, and writes the text — one call. `setSlideText` finds the
placeholders and rewrites them. `reorderSlides` takes the full desired order
and moves each slide into place. A raw `batchUpdate` escape hatch stays
available for edits the shaped tools don't cover (shapes, tables, styling).

Inputs use `snake_case` (`presentation_id`, `slide_id`, `image_url`, ...);
the connector maps them onto the API's camelCase bodies. Sizes and positions
are given in **inches** and converted to EMU internally (914400 EMU per
inch).

![icon](icon.svg)

## Tools

| Tool | What it does in one call |
|---|---|
| `createPresentation` | Create a presentation; answers `{presentationId, title, url}` |
| `getPresentation` | Compact summary: title, slide count, page size, and per-slide `{objectId, layout, title, bodyPreview}` |
| `getSlide` | A slide's page elements with their types (`shape`, `image`, `table`, ...) and text previews |
| `addSlide` | Add a slide with a predefined layout (`BLANK` … `BIG_NUMBER`) and optional title/body text, at an optional position; answers `{slideId}` |
| `setSlideText` | Rewrite a slide's title and/or body placeholder text; answers `{slideId}` |
| `addImage` | Insert an image from a public URL onto a slide, with optional size and position in inches. The API keeps the image's aspect ratio inside the given size; with no size the image keeps its default size |
| `replaceText` | Replace all occurrences of a text string across the presentation or selected slides; answers `{occurrencesChanged}` |
| `deleteSlide` | Delete a slide (via `deleteObject`; the API has no separate delete-slide request); answers `{"deleted": true, "slideId"}` |
| `reorderSlides` | Set the full new order of the deck: `slide_ids` lists every slide ID in the desired order |
| `getSlideThumbnail` | Render a slide as a PNG thumbnail (`SMALL`/`MEDIUM`/`LARGE`, default `LARGE`); answers `{thumbnailUrl, width, height}`. The URL stays valid for about 30 minutes |
| `batchUpdate` | Raw Slides API request objects, e.g. `createShape`, `createTable`, `updateTextStyle` — the escape hatch |

`addSlide` and `setSlideText` fail with a clear error when the layout or
slide has no title/body placeholder to write into, instead of sending a
batch the API would reject.

## Auth

OAuth 2.0 (authorization code + PKCE), scope
`https://www.googleapis.com/auth/presentations`. The host runs the sign-in
and injects the Bearer token; the connector code never sees it. Configure
the `client_id` / `client_secret` inputs and the `api_base` input
(default `https://slides.googleapis.com`).

## Build & test

Built with the AgentRQ `wasmrs` connector toolchain (`oauth2` template):

```sh
cargo test          # unit tests (100% line and function coverage required)
```

`sdk/connectors-sdk` is a host-side test double of the private AgentRQ
`connectors-sdk`, providing the same API surface so tests run anywhere. The
real SDK validates `manifest.json` at compile time and performs HTTP inside
the `wasm32-wasip2` guest.

The multi-call tools stay unit-testable through an injectable sender: the
guest implementation sends requests over HTTP, while tests answer from
canned responses and assert on the requests the tools planned.

Icon: the official Google Slides logo
([Wikimedia Commons](https://commons.wikimedia.org/wiki/File:Google_Slides_icon_(2026).svg)),
used for identification.
