# e2e — headless browser UI check

Drives the **real** Flux stack end to end in headless Chrome and asserts
regression-sensitive UI behavior that unit tests (jsdom has no layout) and
cargo tests (no browser) cannot see:

| Check | Pins |
|-------|------|
| Tool cards keep their height when the chat pane overflows | The pane is a flex column; `.tool` has `overflow: hidden` (automatic minimum size 0) — without `.chat-pane > * { flex-shrink: 0 }` long conversations squeeze every tool card into a 2px line. |
| Composer focus shows a single (outer) border | The global focus ring must not draw an outline inside the bordered `#input-row`; the row's own `focus-within` accent border is the indication. |
| Scroll-to-bottom button reveals on scroll-up | The pane scroll listener must publish through `useFlux.setState` — a direct field write bypasses subscriptions. |
| New-chat dialog height is stable across listings | The dialog frame is fixed-height; the directory list flexes inside and scrolls — entry-count changes never resize the dialog. |
| The preview dock pushes the conversation | The dock is a flex sibling of the chat column: opening it shrinks `#main`, dragging the boundary re-balances both, closing restores the width (overlay only on narrow viewports). |
| Explorer toolbar | The Files tab tree renders with the manual refresh button. |
| A full tool round renders | SSE (scripted provider) → flux kernel → WS → imperative DOM pipeline works against the built bundle. |

## Run

```bash
cd clients/web
npm run ui-check
```

The runner auto-builds what is missing (`cargo build -p flux-server`,
`npm run build` when `dist/` is stale/absent), picks free ports, and uses
CLI flags only (the server has no config file) — no repo state is touched.

## Requirements

- node ≥ 22 (global `WebSocket` for the raw CDP client — no extra deps)
- cargo (flux-server binary)
- Chrome/Chromium — auto-detected (`/opt/google/chrome/chrome`, `chromium`, …);
  override with `FLUX_CHROME=/path/to/chrome`

## Files

- `ui-check.mjs` — orchestrator + CDP driver + assertions
- `fake-provider.mjs` — scripted OpenAI-compatible server: `/models` (a
  two-entry catalog, one with the non-standard `context_length` extension)
  feeds the Providers dialog's probe, and chat completions stream SSE
  (round 1: reasoning + text + `bash` tool call; round 2: closing text) so
  the kernel executes a real tool without network access
