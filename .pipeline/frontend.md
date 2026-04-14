I now have enough context to write accurate frontend standards. Here's the content:

# Frontend Standards

## Framework and Key UI Patterns

The dashboard is a single-page application built with **Alpine.js** (CDN), served as two static HTML fragments:
- `crates/openfang-api/static/index_head.html` — `<head>` with fonts and meta
- `crates/openfang-api/static/index_body.html` — full SPA body (Alpine.js component tree)

Typography: **Inter** (UI) + **Geist Mono** (code/monospace). Accent color: `#6366f1` / `var(--accent)`. Theme tokens use CSS custom properties (`--bg-card`, `--border`, `--text`, `--text-dim`, etc.) with light/dark/system modes toggled via `data-theme`.

Global state lives in `$store.app` (Alpine store). Each page section is a separate `x-data="pageName"` component scoped to its `<template x-if="page === '...'">` block.

**JavaScript SDK** (`sdk/javascript/`) is a zero-dependency CommonJS client targeting Node ≥ 18. Resource classes (`AgentResource`, `SessionResource`, etc.) are instantiated on the `OpenFang` client. All API calls funnel through `_request()` (JSON) or `_stream()` (SSE/async-generator).

## File Organization

```
crates/openfang-api/static/
  index_head.html   # <head> only
  index_body.html   # full SPA — sidebar + all page templates
sdk/javascript/
  index.js          # CJS SDK implementation
  index.d.ts        # TypeScript declarations (kept in sync manually)
  package.json
  examples/         # runnable Node.js examples
```

## Running Frontend Tests

There is no dedicated frontend test runner. Validate the dashboard by running the live integration test flow from `CLAUDE.md`:

```bash
cargo build --release -p openfang-cli
GROQ_API_KEY=<key> ./target/release/openfang start &
sleep 6
curl -s http://127.0.0.1:4200/api/health
# Then verify new UI components exist:
curl -s http://127.0.0.1:4200/ | grep -c "yourComponentName"
```

For the JS SDK, run examples directly:
```bash
node sdk/javascript/examples/basic.js
node sdk/javascript/examples/streaming.js
```

## Quality Rules

- **Page components**: every new page is `x-data="pageName"` inside `<template x-if="page === 'pageName'">`. Call `x-init` for data loading; emit `page-leave` on `window` for cleanup.
- **State**: page-local state in `x-data`, cross-page state in `$store.app`. Never reach into sibling component scope.
- **Loading/error states**: every data-loading section must show a skeleton loader (`x-show="loading"`) and an error state (`x-show="!loading && loadError"`) before the content.
- **SDK**: `index.d.ts` must stay in sync with `index.js`. All resource methods are `async`; streaming methods are `async *` generators. Use `[key: string]: unknown` for extensible options objects, not `any`.

## Common Pitfalls

- Adding a nav item without wiring the `page === 'newPage'` template block — the nav link will render but clicking it shows a blank main area.
- Forgetting `@page-leave.window` cleanup in `x-init` components causes polling/timers to run on other pages.
- CSS variables (`--accent`, `--border`, etc.) must be used for all colors — hardcoded hex values break theme switching.
- Uploading files via the SDK bypasses `_request()` — do not set `Content-Type: application/json` on `FormData` POSTs.
- `index.d.ts` uses `unknown[]` / `Record<string, unknown>` intentionally; do not weaken to `any`.

## Push Contract

Before merging any dashboard change:
1. `curl -s http://127.0.0.1:4200/ | grep -c "yourNewElement"` must return `> 0`
2. New nav items must have both a sidebar `<a>` entry and a corresponding `<template x-if="page === '...'">` block
3. SDK additions require matching declaration in `index.d.ts`