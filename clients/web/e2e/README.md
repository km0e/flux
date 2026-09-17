# e2e — headless browser UI check

Two runners share one skeleton (cargo flux-server + scripted fake provider +
headless Chrome over raw CDP — no extra dependencies):

- **`ui-check.mjs`** — the regression suite (wired into `npm run ui-check`),
  including the transcript-search block that only a real Chrome can assert
  (the CSS Custom Highlight API has no jsdom twin).
- **`shots.mjs`** — the screenshot tour (`npm run shots` → `.shots/*.png`,
  gitignored): empty state, a live tool round (prose sample + both verdict
  cards), the transcript-search bar and command palette (over the
  conversation), the dock's terminal tab (dark), the (lazy-loaded)
  Settings dialog's Providers + MCP sections, and the 390×844 mobile
  regime. Design-review aid —
  and, with the flags below, the driver of the visual-regression net. Its
  settings stop also smoke-tests the lazy dialog chunk against the real
  built bundle.

Drives the **real** Flux stack end to end in headless Chrome and asserts
regression-sensitive UI behavior that unit tests (jsdom has no layout) and
cargo tests (no browser) cannot see:

| Check | Pins |
|-------|------|
| Tool cards keep their height when the chat pane overflows | The pane is a flex column; `.tool` has `overflow: hidden` (automatic minimum size 0) — without `.chat-pane > * { flex-shrink: 0 }` long conversations squeeze every tool card into a 2px line. |
| A failing command paints the exit verdict | The scripted `exit 3` call reports "(exit code: 3)"; the card's status speaks "exit 3" (warn) while the success card stays "completed" — the kernel's string-marker verdicts, end to end. |
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

## Visual regression net

The tour doubles as a pixel guard. `.baseline/` is a LOCAL, gitignored
snapshot of the currently accepted appearance — deliberately NOT a
committed design contract: rendering is machine-coupled (Chrome version,
font rasterization, dsf) and nothing in this repo should pin how the UI
must look. The baseline belongs to a WORK BATCH, not to history:

```bash
cd clients/web
npm run shots:update   # BEFORE a visual batch — snapshot the look as-is
# …make the visual changes; re-run the tour as often as needed…
npm run shots:check    # diff vs the snapshot — everything that moved must
                       # be WHAT YOU MEANT; heatmaps land in .shots/
npm run shots:update   # accept the new look — re-snapshot, keep going
                       # (also after a Chrome major update)
```

A check failing against the snapshot is the whole point: it means
"something moved since you last accepted the look" — recognize each
movement as intended, then re-snapshot. The design is never frozen; only
the boundary of one change batch is.

Zero extra dependencies — the PNG decode and the per-pixel diff run inside
the already-launched Chrome (`createImageBitmap` + `OffscreenCanvas`);
node ships base64 in, stats + a diff heatmap out (`e2e/visual-diff.mjs`).

- **Acceptance is a budget, not zero**: `--max-diff=PCT` (default 0.2% of
  pixels). Content that legitimately varies between runs (the tool card's
  frozen elapsed chip, probe-list render timing) sits two orders of
  magnitude under it; real layout/color drift blows it instantly.
- **Noise floor**: `--tolerance=N` (default 16) — a pixel counts as
  changed only when a channel moves further; absorbs antialiasing noise.
- **Determinism**: captures wait for `document.fonts.ready` and phase-lock
  infinite animations (WAAPI seek) — same machine + same Chrome +
  pinned dsf reproducibly lands 0 px on static shots.
- The diff heatmap tints moved pixels toward red over a dimmed baseline —
  open `.shots/<name>.diff.png` to see WHAT moved before re-baselining.

## Requirements

- node ≥ 22 (global `WebSocket` for the raw CDP client — no extra deps)
- cargo (flux-server binary)
- Chrome/Chromium — auto-detected (`/opt/google/chrome/chrome`, `chromium`, …);
  override with `FLUX_CHROME=/path/to/chrome`

## Files

- `ui-check.mjs` — orchestrator + CDP driver + assertions
- `shots.mjs` — screenshot tour + visual-regression driver (modes above)
- `visual-diff.mjs` — the in-Chrome pixel comparison (routine serialized
  into the page; node-side arg parsing, baseline snapshot, compare loop)
- `fake-provider.mjs` — scripted OpenAI-compatible server: `/models` (a
  two-entry catalog, one with the non-standard `context_length` extension)
  feeds the Providers dialog's probe, and chat completions stream SSE
  (round 1: reasoning + a full prose sample — inline code, fenced block,
  quote, hr, list — then the pathological pair (unbreakable token + wide
  table) + a `bash` tool call; round 2: closing text) so the kernel
  executes a real tool without network access
