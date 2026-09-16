/**
 * ui-check.mjs — headless browser smoke check for the Flux web UI.
 *
 * Drives the REAL stack end to end: a cargo-built flux-server + a scripted
 * SSE provider (fake-provider.mjs) + headless Chrome over the raw CDP
 * (node:24 global WebSocket — no extra dependencies, no test runner).
 *
 * Regression coverage (each pinned by an assertion below):
 *   1. Tool-card geometry under overflow — the chat pane is a flex column;
 *      .tool carries overflow:hidden (automatic minimum size 0), so without
 *      `flex-shrink: 0` a long conversation squeezes every tool card into a
 *      2px line. Cards must keep their height when the pane overflows.
 *   2. Composer focus — the global focus ring must not draw an outline
 *      INSIDE the bordered #input-row (the row's own focus-within accent
 *      border is the indication); the focused textarea's outline-width is 0.
 *   3. Scroll-to-bottom button — the pane scroll listener must publish
 *      scrollBtnVisible through setState (a direct field write bypasses
 *      subscriptions); scrolling up reveals the button reactively.
 *   4. The streaming pipeline itself — a tool round (SSE → kernel → WS →
 *      imperative DOM) produces a completed tool card with its result.
 *   5. Fork semantics — the copy EXCLUDES the fork point (the redo turn):
 *      the forked pane carries no copied messages and its composer opens
 *      prefilled with the forked message's content.
 *   6. Mobile drawer — the drawer keeps its authored width (`flex: none`
 *      against the Tabs root's flex-1, else the grow makes it a full-screen
 *      sheet), starts below the top bar (toggle/X + backdrop stay reachable
 *      as the way back), and the row ⋯ menu opens WITHOUT closing the
 *      drawer (the tap's click must not bubble into the row's select).
 *
 * Usage:
 *   npm run ui-check          (from clients/web)
 * Requires: node ≥ 22 (global WebSocket), cargo, a Chrome/Chromium binary
 * (auto-detected; override with FLUX_CHROME=/path/to/chrome).
 */
import { spawn, execSync } from 'node:child_process';
import { mkdtempSync, writeFileSync, existsSync, rmSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { createServer } from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const WEB_DIR = path.resolve(HERE, '..');
const REPO_ROOT = path.resolve(WEB_DIR, '..', '..');
const SERVER_BIN = path.join(REPO_ROOT, 'target', 'debug', 'flux-server');
const DIST = path.join(WEB_DIR, 'dist');

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Pick a free TCP port by binding port 0 and releasing. */
function freePort() {
  return new Promise((resolve, reject) => {
    const srv = createServer();
    srv.unref();
    srv.on('error', reject);
    srv.listen(0, '127.0.0.1', () => {
      const { port } = srv.address();
      srv.close(() => resolve(port));
    });
  });
}

// ── Process management ──────────────────────────────────────────────────

const children = [];
function startDetached(cmd, args, logFile, opts = {}) {
  const out = { 'ignore': 'ignore', 'inherit': 'inherit' }[logFile] ?? logFile;
  const child = spawn(cmd, args, { stdio: ['ignore', out, out], detached: false, ...opts });
  children.push(child);
  return child;
}

function cleanup() {
  for (const c of children) {
    try {
      c.kill('SIGKILL');
    } catch {
      /* already gone */
    }
  }
}
process.on('exit', cleanup);
process.on('SIGINT', () => process.exit(130));
process.on('SIGTERM', () => process.exit(143));

// ── Assertions ──────────────────────────────────────────────────────────

const results = [];

// ── minimal gRPC-Web + protobuf helpers (harness-local, no imports) ────────

/** One uncompressed length-prefixed data frame. */
function lpFrame(msg) {
  const out = new Uint8Array(5 + msg.length);
  out[0] = 0;
  new DataView(out.buffer).setUint32(1, msg.length);
  out.set(msg, 5);
  return out;
}

/** Split a gRPC-Web body: (data frames, trailer bytes). */
function parseGrpcWebBody(buf) {
  const data = [];
  let trailer = null;
  let i = 0;
  while (i + 5 <= buf.length) {
    const flag = buf[i];
    const len = new DataView(buf.buffer, buf.byteOffset + i + 1).getUint32(0);
    const frame = buf.slice(i + 5, i + 5 + len);
    i += 5 + len;
    if (flag & 0x80) trailer = frame;
    else data.push(frame);
  }
  return { data, trailer };
}

function trailerGrpcStatus(trailer) {
  if (!trailer) return null;
  const text = new TextDecoder().decode(trailer);
  for (const line of text.split('\n')) {
    if (line.startsWith('grpc-status:')) return Number(line.slice(12).trim());
  }
  return null;
}

/** AddProviderRequest {id(1), url(3), api_key(4)} — the three fields the
 * harness sets. Hand-rolled protobuf (the .proto is the contract). */
function encodeAddProviderRequest(req) {
  const field = (num, bytes) => {
    const out = [];
    const tag = (num << 3) | 2; // length-delimited wire type
    while (tag > 0x7f) { out.push((tag & 0x7f) | 0x80); tag >>>= 7; }
    out.push(tag);
    let len = bytes.length;
    while (len > 0x7f) { out.push((len & 0x7f) | 0x80); len >>>= 7; }
    out.push(len);
    return new Uint8Array([...out, ...bytes]);
  };
  const str = (s) => new TextEncoder().encode(s);
  const parts = [field(1, str(req.id)), field(2, str('openai'))];
  if (req.url) parts.push(field(3, str(req.url)));
  if (req.api_key) parts.push(field(4, str(req.api_key)));
  const total = parts.reduce((n, p) => n + p.length, 0);
  const out = new Uint8Array(total);
  let off = 0;
  for (const p of parts) { out.set(p, off); off += p.length; }
  return out;
}

/** AddProviderResponse {id(1), error(2)}. */
function decodeAddProviderResponse(buf) {
  const resp = { id: '', error: null };
  let i = 0;
  while (i < buf.length) {
    let tag = 0, shift = 0;
    do { tag |= (buf[i++] & 0x7f) << shift; shift += 7; } while (buf[i - 1] & 0x80);
    const len = buf[i++];
    const val = new TextDecoder().decode(buf.slice(i, i + len));
    i += len;
    if ((tag >>> 3) === 1) resp.id = val;
    if ((tag >>> 3) === 2) resp.error = val;
  }
  return resp;
}
function check(name, ok, detail = '') {
  results.push({ name, ok, detail });
  console.log(`${ok ? '  ✓' : '  ✗'} ${name}${detail ? ` — ${detail}` : ''}`);
}

// ── CDP driver ──────────────────────────────────────────────────────────

async function connectCdp(port, urlFilter) {
  let page = null;
  for (let i = 0; i < 40 && !page; i++) {
    try {
      const targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
      page = targets.filter((t) => t.type === 'page' && t.url.includes(urlFilter)).pop();
    } catch {
      /* chrome not up yet */
    }
    if (!page) await sleep(250);
  }
  if (!page) throw new Error('no browser page target found');

  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((r, j) => {
    ws.onopen = r;
    ws.onerror = j;
  });
  let nextId = 0;
  const pending = new Map();
  ws.onmessage = (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result);
    }
  };
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = ++nextId;
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, method, params }));
    });
  /** Evaluate an expression in the page; throws on page exceptions. */
  const evalJs = async (expression) => {
    const r = await send('Runtime.evaluate', {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (r.exceptionDetails) {
      throw new Error(r.exceptionDetails.exception?.description ?? 'page exception');
    }
    return r.result.value;
  };
  /** Poll until the expression is truthy (or timeout). */
  const waitFor = async (expression, label, timeoutMs = 20_000) => {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const v = await evalJs(expression);
      if (v) return v;
      if (Date.now() > deadline) throw new Error(`timeout waiting for ${label}`);
      await sleep(250);
    }
  };
  return { ws, send, evalJs, waitFor };
}

// ── Chrome discovery ────────────────────────────────────────────────────

function findChrome() {
  if (process.env.FLUX_CHROME) return process.env.FLUX_CHROME;
  const candidates = [
    '/opt/google/chrome/chrome',
    '/usr/bin/google-chrome',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
  ];
  for (const c of candidates) if (existsSync(c)) return c;
  try {
    execSync('command -v chrome', { stdio: 'ignore' });
    return 'chrome';
  } catch {
    /* fall through */
  }
  throw new Error(
    'no Chrome/Chromium found — install one or set FLUX_CHROME=/path/to/chrome',
  );
}

// ── Main scenario ───────────────────────────────────────────────────────

async function main() {
  if (typeof WebSocket !== 'function') {
    console.error('ui-check needs node ≥ 22 (global WebSocket).');
    process.exit(2);
  }

  // Build what the check serves/drives.
  if (!existsSync(SERVER_BIN)) {
    console.log('building flux-server (cargo build -p flux-server)…');
    execSync('cargo build -p flux-server', { cwd: REPO_ROOT, stdio: 'inherit' });
  }
  if (!existsSync(path.join(DIST, 'index.html'))) {
    console.log('building web UI (npm run build)…');
    execSync('npm run build', { cwd: WEB_DIR, stdio: 'inherit' });
  }

  // One port hosts everything now: the Connect surface + the static UI.
  const serverPort = await freePort();
  const providerPort = await freePort();

  // Marker workdir: the chat's Files tab has a file to click for the
  // docked-preview checks (the dialog's default is $HOME — navigate to here).
  const workdir = path.join(tmpdir(), 'flux-ui-check-wd');
  rmSync(workdir, { recursive: true, force: true });
  mkdirSync(workdir, { recursive: true });
  writeFileSync(path.join(workdir, 'preview-me.md'), '# docked preview check\n');

  const tmp = mkdtempSync(path.join(tmpdir(), 'flux-ui-check-'));

  console.log('starting fake provider + flux-server…');
  startDetached(process.execPath, [path.join(HERE, 'fake-provider.mjs'), String(providerPort)], 'inherit');
  startDetached(
    SERVER_BIN,
    [
      // NO config file — everything rides CLI flags; the provider registry
      // lives in the DB and is registered over the WS below.
      '--db-path', path.join(tmp, 'ui-check.db'),
      '--web-assets-dir', DIST,
      '--host', '127.0.0.1', '--port', String(serverPort),
    ],
    'inherit',
    // The server's reqwest honors env-var proxies (system-proxy feature) —
    // on a proxied dev machine the upstream POST to the loopback fake
    // provider would be routed through the proxy and answered with a 502.
    // Scope no_proxy to THIS child only; the user's shell is untouched.
    { env: { ...process.env, no_proxy: '127.0.0.1,localhost', NO_PROXY: '127.0.0.1,localhost' } },
  );
  for (let i = 0; i < 60; i++) {
    try {
      if ((await fetch(`http://127.0.0.1:${serverPort}/`)).ok) break;
    } catch {
      /* not up yet */
    }
    await sleep(250);
  }

  const chrome = findChrome();
  const cdpPort = await freePort();
  console.log(`launching headless chrome (${chrome})…`);
  startDetached(
    chrome,
    [
      '--headless=new', '--no-sandbox', '--disable-gpu', '--hide-scrollbars',
      '--window-size=1280,860',
      `--remote-debugging-port=${cdpPort}`,
      `--user-data-dir=${path.join(tmp, 'profile')}`,
      `http://127.0.0.1:${serverPort}/`,
    ],
    'ignore',
  );

  const { send, evalJs, waitFor } = await connectCdp(cdpPort, String(serverPort));

  // Force :focus-visible matching so the composer focus check is meaningful
  // without synthesizing trusted key events.
  await send('Emulation.setFocusEmulationEnabled', { enabled: true });

  // 1. App mounts + the shell layout survived the @layer base move (the
  // #sidebar rules now live in the base layer; utilities must not disturb
  // the authored width/overflow).
  await waitFor(`!!document.getElementById('new-chat-btn')`, 'app mount');

  // The provider registry lives in the server DB (UI-managed). Register the
  // fake provider over a raw gRPC-Web call BEFORE opening the new-chat
  // dialog; the `providers` broadcast updates the app's store live (the
  // page's own Subscribe stream). Hand-rolled framing — the browser's
  // exact wire, no client libraries in the harness.
  {
    const body = lpFrame(
      encodeAddProviderRequest({
        id: 'default',
        url: `http://127.0.0.1:${providerPort}/v1`,
        api_key: 'sk-ui-check',
      }),
    );
    const resp = await fetch(
      `http://127.0.0.1:${serverPort}/flux.v1.ProviderService/AddProvider`,
      { method: 'POST', headers: { 'content-type': 'application/grpc-web+proto' }, body },
    );
    if (!resp.ok) throw new Error(`provider_add transport failed: ${resp.status}`);
    const { data, trailer } = parseGrpcWebBody(new Uint8Array(await resp.arrayBuffer()));
    if (trailerGrpcStatus(trailer) !== 0) throw new Error(`provider_add refused`);
    const ack = decodeAddProviderResponse(data[0]);
    if (ack.error) throw new Error(`provider_add failed: ${ack.error}`);
  }
  const shell = await evalJs(`(() => {
    const sb = document.getElementById('sidebar');
    const cs = getComputedStyle(sb);
    return JSON.stringify({
      w: sb.getBoundingClientRect().width,
      display: cs.display,
      overflow: cs.overflow,
      layerDisplay: getComputedStyle(document.getElementById('sidebar-layer')).display,
    });
  })()`);
  const sh = JSON.parse(shell);
  check(
    'shell layout intact (sidebar authored width + overflow)',
    Math.abs(sh.w - 240) < 1 && sh.display === 'flex' && sh.overflow === 'hidden' && sh.layerDisplay === 'flex',
    `width=${sh.w}, display=${sh.display}, overflow=${sh.overflow}`,
  );

  // 2. Create a chat in the marker workdir through the real dialog:
  // Home → pick provider + type model (REQUIRED pin) → / Root → tmp →
  // flux-ui-check-wd → Create here.
  await evalJs(`document.getElementById('new-chat-btn').click()`);
  await waitFor(
    `!!document.querySelector('select')`,
    'new-chat dialog',
  );
  // Stable dialog frame: the listing's entry count must not resize the
  // dialog — the list area flexes inside a fixed-height box. Measure now
  // (home listing) and again after the deeper tmp listing below.
  const dialogH1 = await evalJs(`document.querySelector('[role="dialog"]').offsetHeight`);
  // The dialog itself must NEVER scroll — the file list inside is the only
  // scrollable region (the reported extra scrollbar).
  const dlgScroll = JSON.parse(
    await evalJs(`(() => {
      const dlg = document.querySelector('[role="dialog"]');
      return JSON.stringify({ sh: dlg.scrollHeight, ch: dlg.clientHeight });
    })()`),
  );
  check(
    'new-chat dialog body has no scrollbar (list scrolls, not the dialog)',
    dlgScroll.sh <= dlgScroll.ch + 1,
    `scrollHeight ${dlgScroll.sh} vs clientHeight ${dlgScroll.ch}`,
  );
  // The pin (provider + model) is required and Create is gated on both —
  // drive the controlled fields via the native value setters (bypasses
  // React's value tracker).
  await evalJs(
    `(() => {
      const s = document.querySelector('select');
      const setter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set;
      setter.call(s, 'default');
      s.dispatchEvent(new Event('change', { bubbles: true }));
    })()`,
  );
  await evalJs(
    `(() => {
      const m = document.querySelector('input[list="provider-models-default"]');
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
      setter.call(m, 'ui-check-model');
      m.dispatchEvent(new Event('input', { bubbles: true }));
    })()`,
  );
  await waitFor(
    `[...document.querySelectorAll('button')].some((b) => b.textContent.includes('Create here') && !b.disabled)`,
    'provider + model pinned',
  );
  await evalJs(
    `[...document.querySelectorAll('button')].find((b) => b.textContent.trim() === 'Root').click()`,
  );
  await waitFor(`!!document.querySelector('button[title="tmp"]')`, 'root listing');
  await evalJs(`document.querySelector('button[title="tmp"]').click()`);
  await waitFor(`!!document.querySelector('button[title="flux-ui-check-wd"]')`, 'tmp listing');
  const dialogH2 = await evalJs(`document.querySelector('[role="dialog"]').offsetHeight`);
  check(
    'dialog height is stable across listings (list scrolls, frame does not)',
    Math.abs(dialogH2 - dialogH1) <= 1,
    `home ${dialogH1}px vs tmp ${dialogH2}px`,
  );
  await evalJs(`document.querySelector('button[title="flux-ui-check-wd"]').click()`);
  await waitFor(
    `[...document.querySelectorAll('button')].some((b) => b.textContent.includes('Create here') && !b.disabled)`,
    'marker workdir listing',
  );
  await evalJs(
    `[...document.querySelectorAll('button')].find((b) => b.textContent.includes('Create here')).click()`,
  );
  await waitFor(`!!document.querySelector('.chat-pane')`, 'chat pane');

  // 3. Composer focus: outer border is THE indication — the row picks up
  // its focus-within accent border and the textarea draws no inner outline.
  // (Chrome quirk: with outline-style none, computed outline-width still
  // reports the specified medium — assert on the STYLE, the visible signal.
  // Runs AFTER chat creation: the composer is disabled until a chat is
  // active, and a disabled textarea can neither focus nor match focus-within.)
  const focusProbe = await evalJs(`(() => {
    const ta = document.getElementById('input');
    const row = document.getElementById('input-row');
    ta.blur(); // the composer autofocused on mount — sample the RESTING border
    return JSON.stringify({
      disabled: ta.disabled,
    });
  })()`);
  await sleep(300); // let the border-color transition back to rest settle
  const restBorder = await evalJs(
    `getComputedStyle(document.getElementById('input-row')).borderTopColor`,
  );
  await evalJs(`document.getElementById('input').focus()`);
  await sleep(300); // let the border-color transition settle
  const focus = await evalJs(`(() => {
    const ta = document.getElementById('input');
    const row = document.getElementById('input-row');
    return JSON.stringify({
      outlineStyle: getComputedStyle(ta).outlineStyle,
      focusedBorder: getComputedStyle(row).borderTopColor,
    });
  })()`);
  const rest = JSON.parse(focusProbe);
  const f = JSON.parse(focus);
  check(
    'composer focus keeps a single (outer) border — no inner outline',
    rest.disabled === false &&
      f.outlineStyle === 'none' &&
      f.focusedBorder !== restBorder,
    `outline-style: ${f.outlineStyle}, border ${restBorder} → ${f.focusedBorder}`,
  );

  // 4. Send a message; the fake provider answers with a bash tool call.
  await evalJs(`(() => {
    const ta = document.getElementById('input');
    ta.value = 'run the demo tool please';
    ta.dispatchEvent(new Event('input', { bubbles: true }));
    document.getElementById('send').click();
  })()`);
  await waitFor(`document.querySelectorAll('.tool.done').length >= 1`, 'tool round wrap-up');
  // The result <pre> is LAZILY materialized on first expansion (dom.ts) —
  // a collapsed card carries no result text. Expand, then assert.
  await evalJs(`document.querySelector('.tool.done .tool-header').click()`);
  await waitFor(
    `(document.querySelector('.chat-pane')?.textContent ?? '').includes('ran fine')`,
    'tool result text',
  );
  check('tool round completes (SSE → kernel → WS → DOM)', true);

  // 4.5 Fork, LIVE path: the user message persisted announcement
  // (`message_persisted`) must attach the fork affordance to the sender's
  // own bubble WITHOUT a history reload — clicking it forks a NEW chat
  // from that message (no confirm: the source is untouched).
  await waitFor(
    `!!document.querySelector('.chat-pane .message.user .msg-fork')`,
    'live fork affordance',
  );
  check('live user bubble gains the fork affordance without a reload', true);
  await evalJs(
    `document.querySelector('.chat-pane .message.user .msg-fork').click()`,
  );
  // The fork ack's chat_created auto-selects the new conversation; its
  // name carries the fork lineage.
  await waitFor(
    `(document.getElementById('chat-header')?.textContent ?? '').includes('(fork)')`,
    'forked chat header',
  );
  check('forking opens the new conversation (source untouched)', true);
  // The copy EXCLUDES the fork point (it is the redo turn): the forked
  // pane carries no copied messages and its composer opens prefilled
  // with the forked message's content.
  await sleep(300); // the claim snapshot + the composer's draft consume
  const forkState = JSON.parse(
    await evalJs(`(() => {
      const pane = [...document.querySelectorAll('.chat-pane')]
        .find((p) => p.style.display !== 'none');
      const ta = document.getElementById('input');
      return JSON.stringify({
        empty: !(pane?.textContent ?? '').includes('run the demo tool please'),
        draft: ta?.value === 'run the demo tool please',
      });
    })()`),
  );
  check(
    'fork copy excludes the fork point; composer prefills the redo turn',
    forkState.empty && forkState.draft,
    JSON.stringify(forkState),
  );
  // The source lease handed over with the navigation: the fork's chats
  // broadcast already carries the source free — no row flashes In-use.
  const noInUse = JSON.parse(
    await evalJs(
      `JSON.stringify([...document.querySelectorAll('#conversation-list [role="button"]')]
         .every((row) => !row.textContent.includes('In use')))`,
    ),
  );
  check('no In-use badge after forking (the source lease handed over)', noInUse === true);
  // Switch BACK to the source conversation — the following overflow checks
  // exercise the pane that carries the demo tool cards.
  await evalJs(
    `[...document.querySelectorAll('#conversation-list [role="button"]')]
       .find((row) => !row.textContent.includes('(fork)'))
       ?.click()`,
  );
  await waitFor(
    `!(document.getElementById('chat-header')?.textContent ?? '').includes('(fork)')`,
    'back on the source chat',
  );

  // 5. THE BUG: overflow the pane — tool cards must not collapse into lines.
  await evalJs(`(() => {
    const pane = document.querySelector('.chat-pane');
    for (let i = 0; i < 40; i++) {
      const d = document.createElement('div');
      d.className = 'message user';
      d.innerHTML = '<div class="message-body">filler ' + i +
        ' — enough rows to push the flex column past its definite height.</div>';
      pane.insertBefore(d, pane.firstChild);
    }
    pane.scrollTop = 0;
  })()`);
  await sleep(200);
  const overflow = await evalJs(`(() => {
    const pane = document.querySelector('.chat-pane');
    const tools = [...document.querySelectorAll('.tool')].map((t) => t.getBoundingClientRect().height);
    return JSON.stringify({
      overflowing: pane.scrollHeight > pane.clientHeight,
      toolHeights: tools,
      shrink: getComputedStyle(document.querySelector('.tool')).flexShrink,
    });
  })()`);
  const o = JSON.parse(overflow);
  check(
    'pane actually overflows (test is meaningful)',
    o.overflowing,
    `scrollHeight > clientHeight`,
  );
  check(
    'tool cards keep their height under overflow (flex-shrink: 0)',
    o.shrink === '0' && o.toolHeights.every((h) => h > 20),
    `shrink=${o.shrink}, heights=[${o.toolHeights.map((h) => Math.round(h)).join(', ')}]`,
  );

  // 6. Scroll-to-bottom button reacts to user scroll (setState path).
  await evalJs(`(() => {
    const pane = document.querySelector('.chat-pane');
    pane.scrollTop = pane.scrollHeight; // bottom → hidden
  })()`);
  await sleep(150);
  await evalJs(`(() => {
    const pane = document.querySelector('.chat-pane');
    pane.scrollTop = 0; // away from bottom → visible
  })()`);
  await waitFor(
    `document.getElementById('scroll-bottom-btn')?.className.includes('opacity-95')`,
    'scroll button reveal',
  );
  check('scroll-to-bottom button reveals on scroll-up (reactive store write)', true);

  // 6.5 EXPANSION FOLLOW: expanding the last tool card while attached must
  // keep the pane pinned to the bottom — the card sits near the pane end,
  // its detail grows through a 180ms grid-rows transition, and without the
  // follow the growth lands below the fold (the scrollbar-thumb-rises bug).
  await evalJs(`(() => {
    const pane = document.querySelector('.chat-pane');
    pane.scrollTop = pane.scrollHeight; // attach the stick state
  })()`);
  await sleep(150);
  await evalJs(`(() => {
    const tools = document.querySelectorAll('.chat-pane .tool .tool-header');
    tools[tools.length - 1].click();
  })()`);
  // ≥ the 180ms transition + the bounded follow loop.
  await sleep(500);
  const expand = await evalJs(`(() => {
    const pane = document.querySelector('.chat-pane');
    const tools = document.querySelectorAll('.chat-pane .tool');
    const card = tools[tools.length - 1];
    return JSON.stringify({
      expanded: card.classList.contains('expanded'),
      // Bottom-gap: 0 = pinned. Before the fix this equals the growth.
      gap: pane.scrollHeight - pane.scrollTop - pane.clientHeight,
    });
  })()`);
  const ex = JSON.parse(expand);
  check(
    'expanding the bottom tool card keeps the pane pinned (expansion follow)',
    ex.expanded && ex.gap <= 8,
    `expanded=${ex.expanded}, bottom-gap=${Math.round(ex.gap)}px`,
  );
  // The anchoring contract: native scroll anchoring must be OFF on the
  // pane (the stick-state follow owns repositioning). Tailwind generates
  // no overflow-anchor utility — a class-based attempt was silently dead
  // once, so this asserts the COMPUTED style, not the class list.
  const anchor = await evalJs(
    `getComputedStyle(document.querySelector('.chat-pane')).overflowAnchor`,
  );
  check('pane disables native scroll anchoring (computed)', anchor === 'none', `overflowAnchor=${anchor}`);
  // Collapse must NOT scroll — shrink the content back.
  await evalJs(`(() => {
    const tools = document.querySelectorAll('.chat-pane .tool .tool-header');
    tools[tools.length - 1].click();
  })()`);
  await sleep(300);
  // STRESS: a transition SLOWER than any fixed frame budget — the exact
  // shape of the 120Hz bug (the follow loop ended before the growth did).
  // The follow window derives from the element's computed transition
  // duration, so even a 600ms transition must stay pinned to the bottom.
  await evalJs(`(() => {
    const pane = document.querySelector('.chat-pane');
    pane.scrollTop = pane.scrollHeight; // re-attach after the collapse clamp
  })()`);
  await sleep(150);
  await evalJs(`document.documentElement.style.setProperty('--fx-duration-base', '600ms')`);
  await evalJs(`(() => {
    const tools = document.querySelectorAll('.chat-pane .tool .tool-header');
    tools[tools.length - 1].click();
  })()`);
  // ≥ the 600ms transition + margin.
  await sleep(1100);
  const stress = await evalJs(`(() => {
    const pane = document.querySelector('.chat-pane');
    return JSON.stringify({
      gap: pane.scrollHeight - pane.scrollTop - pane.clientHeight,
    });
  })()`);
  check(
    'follow outlives a slow transition (stress: 600ms growth stays pinned)',
    JSON.parse(stress).gap <= 8,
    `bottom-gap=${Math.round(JSON.parse(stress).gap)}px`,
  );
  await evalJs(`document.documentElement.style.setProperty('--fx-duration-base', '')`);

  // 7. Explorer toolbar + docked preview: the file click opens the dock and
  // widening it PUSHES the conversation left (never covers it).
  // Radix TabsTrigger needs a trusted pointer sequence — element.click()
  // alone does not switch the tab.
  const tabRect = JSON.parse(
    await evalJs(
      `(() => { const b = [...document.querySelectorAll('#sidebar [role=tab]')].find((x) => x.textContent.includes('Files')); const r = b.getBoundingClientRect(); return JSON.stringify({ x: r.x + r.width / 2, y: r.y + r.height / 2 }); })()`,
    ),
  );
  await send('Input.dispatchMouseEvent', { type: 'mousePressed', x: tabRect.x, y: tabRect.y, button: 'left', buttons: 1, clickCount: 1 });
  await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: tabRect.x, y: tabRect.y, button: 'left', buttons: 1, clickCount: 1 });
  try {
    await waitFor(
      `!!document.querySelector('#explorer div[title$="/preview-me.md"]')`,
      'explorer file row (auto-expanded root)',
    );
  } catch (e) {
    // Diagnose: what did the tree actually render, and which workdir is active?
    const dump = await evalJs(
      `JSON.stringify({
        workdir: document.querySelector('#sidebar [title]:not([title=""])')?.getAttribute('title'),
        explorer: document.getElementById('explorer')?.textContent.slice(0, 300) ?? 'NO #explorer',
        tabs: [...document.querySelectorAll('#sidebar [role=tab]')].map((b) => b.getAttribute('data-state')),
      })`,
    );
    console.log('explorer debug:', dump);
    throw e;
  }
  check('explorer toolbar renders (manual refresh)',
    await evalJs(`!!document.querySelector('button[aria-label="Refresh files"]')`));
  const mainBefore = await evalJs(`document.getElementById('main').getBoundingClientRect().width`);
  await evalJs(`document.querySelector('#explorer div[title$="/preview-me.md"]').click()`);
  await waitFor(`!!document.getElementById('right-dock')`, 'right dock open');
  await sleep(400); // fs_read round-trip
  const docked = JSON.parse(
    await evalJs(`(() => {
      const dock = document.getElementById('right-dock');
      return JSON.stringify({
        position: getComputedStyle(dock).position,
        dockW: dock.getBoundingClientRect().width,
        mainW: document.getElementById('main').getBoundingClientRect().width,
        content: dock.textContent.includes('docked preview check'),
      });
    })()`),
  );
  check(
    'preview dock takes layout space and pushes the conversation left',
    docked.position === 'relative' && docked.content && mainBefore - docked.mainW > 400,
    `position=${docked.position}, main ${Math.round(mainBefore)} → ${Math.round(docked.mainW)}px (dock ${Math.round(docked.dockW)}px)`,
  );

  // 8. Dragging the dock's left edge moves the boundary (chat shrinks further).
  const r = JSON.parse(
    await evalJs(
      `(() => { const b = document.getElementById('dock-resizer').getBoundingClientRect(); return JSON.stringify({ x: b.x + b.width / 2, y: b.y + b.height / 2 }); })()`,
    ),
  );
  await send('Input.dispatchMouseEvent', { type: 'mousePressed', x: r.x, y: r.y, button: 'left', buttons: 1, clickCount: 1 });
  await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: r.x - 140, y: r.y, button: 'left', buttons: 1 });
  await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: r.x - 140, y: r.y, button: 'left', buttons: 1, clickCount: 1 });
  await sleep(250);
  const afterDrag = JSON.parse(
    await evalJs(`(() => {
      const dock = document.getElementById('right-dock');
      return JSON.stringify({
        dockW: dock.getBoundingClientRect().width,
        mainW: document.getElementById('main').getBoundingClientRect().width,
      });
    })()`),
  );
  check(
    'dragging the dock boundary re-balances dock vs conversation',
    afterDrag.dockW > docked.dockW + 100 && afterDrag.mainW < docked.mainW - 100,
    `dock ${Math.round(docked.dockW)} → ${Math.round(afterDrag.dockW)}px, main ${Math.round(docked.mainW)} → ${Math.round(afterDrag.mainW)}px`,
  );

  // 8.5 Terminal: the tab strip's "+" adds a terminal tab (explicit ask —
  // never auto-spawned; the dock itself is already open from check 7);
  // the full loop (WS /ws/term → e4pty → bash → output → xterm) runs live.
  await evalJs(`document.querySelector('#right-dock button[aria-label="New terminal"]').click()`);
  await waitFor(`!!document.querySelector('#right-dock [role=tab][title^="Interactive shell"]')`, 'terminal tab created');
  await waitFor(`!!document.querySelector('#right-dock .xterm')`, 'xterm mounted');
  await sleep(600); // shell prompt lands
  // Focus persistence: a TRUSTED click focuses xterm; the status-tick
  // repaints (every 300ms) must never steal it — the mount effect runs on
  // tabId only. Regression guard for the reported focus-steal.
  const sc = JSON.parse(
    await evalJs(`(() => { const r = document.querySelector('#right-dock .xterm-screen').getBoundingClientRect(); return JSON.stringify({ x: Math.round(r.x + 60), y: Math.round(r.y + 60) }); })()`),
  );
  await send('Input.dispatchMouseEvent', { type: 'mousePressed', x: sc.x, y: sc.y, button: 'left', buttons: 1, clickCount: 1 });
  await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: sc.x, y: sc.y, button: 'left', clickCount: 1 });
  await sleep(800); // > 2 status ticks — the old bug stole focus right here
  const focused = await evalJs(
    `document.activeElement?.className?.includes?.('xterm-helper-textarea') ?? false`,
  );
  check('terminal keeps focus across status repaints (no focus steal)', !!focused);
  await evalJs(`document.querySelector('#right-dock .xterm-helper-textarea').focus()`);
  for (const ch of 'echo ui-check-ok\r') {
    await send('Input.dispatchKeyEvent', { type: 'keyDown', text: ch, unmodifiedText: ch, key: ch });
    await send('Input.dispatchKeyEvent', { type: 'keyUp', key: ch });
  }
  await waitFor(
    `document.getElementById('right-dock').textContent.includes('ui-check-ok')`,
    'terminal echo round-trip',
  );
  check('terminal round-trip works (WS → PTY → shell → xterm)', true);
  // Geometry: the screen fills the dock body — no truncated-height render.
  const geom = JSON.parse(
    await evalJs(`(() => {
      const screen = document.querySelector('#right-dock .xterm-screen');
      const host = document.querySelector('[data-testid=terminal-host]');
      const r = screen.getBoundingClientRect();
      const h = host.getBoundingClientRect();
      return JSON.stringify({ screenH: Math.round(r.height), screenW: Math.round(r.width), hostH: Math.round(h.height) });
    })()`),
  );
  check(
    'terminal renders at a real size (fills the host, no truncation)',
    geom.screenH > 200 && geom.screenW > 300 && Math.abs(geom.screenH - geom.hostH) < 40,
    `screen ${geom.screenW}x${geom.screenH} in host ${geom.hostH}px`,
  );
  // The bundled web font actually loaded (not a system fallback).
  const fontDbg = await evalJs(
    `JSON.stringify({
      check: document.fonts?.check?.('13px "JetBrains Mono"') ?? null,
      faces: [...document.fonts].filter((f) => f.family.includes('JetBrains')).map((f) => f.family + '/' + f.weight + '/' + f.status),
    })`,
  );
  console.log('font debug:', fontDbg);
  const fontOk = JSON.parse(fontDbg).faces.some((f) => f.endsWith('/loaded'));
  check('bundled terminal font loaded (JetBrains Mono)', !!fontOk);

  // 9. Closing releases the space.
  await evalJs(
    `[...document.querySelectorAll('#right-dock button')].find((b) => b.getAttribute('aria-label') === 'Close dock').click()`,
  );
  await waitFor(`!document.getElementById('right-dock')`, 'right dock closed');
  const mainAfterClose = await evalJs(`document.getElementById('main').getBoundingClientRect().width`);
  check(
    'closing the dock restores the conversation width',
    Math.abs(mainAfterClose - mainBefore) < 2,
    `main back to ${Math.round(mainAfterClose)}px`,
  );

  // 9.5 The dock's DIRECT toggle (ChatHeader): the dock must be reachable
  // without first opening a file or spawning a terminal. Open → visible →
  // close again (the mobile section below needs the dock closed).
  await evalJs(`document.getElementById('dock-toggle').click()`);
  await waitFor(`!!document.getElementById('right-dock')`, 'dock reopened via toggle');
  const reopenedEmpty = await evalJs(
    `(() => {
      const d = document.getElementById('right-dock');
      return JSON.stringify({ pressed: document.getElementById('dock-toggle').getAttribute('aria-pressed'), empty: d.textContent.includes('Nothing open') });
    })()`,
  );
  await evalJs(`document.getElementById('dock-toggle').click()`);
  await waitFor(`!document.getElementById('right-dock')`, 'dock closed via toggle');
  check(
    'the header dock toggle opens/closes the dock directly',
    JSON.parse(reopenedEmpty).pressed === 'true',
    `aria-pressed=${JSON.parse(reopenedEmpty).pressed}`,
  );

  // 10. MOBILE regime — device-metrics + touch emulation at 390×844: the
  // responsive shell must hold the geometry floors (no horizontal
  // overflow, touch-visible hover affordances, 16px composer input
  // against iOS auto-zoom, full-screen dock sheet) and the drawer must
  // stay a drawer (authored width below the bar, ⋯ menu contained).
  // Restores the desktop metrics after.
  {
    await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 3, mobile: true });
    await send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 });
    await sleep(600);

    const overflow = await evalJs(
      `document.documentElement.scrollWidth - document.documentElement.clientWidth`,
    );
    check('mobile: no horizontal overflow at 390px', overflow === 0, `overflow=${overflow}px`);
    const inputFont = await evalJs(
      `document.getElementById('input') ? getComputedStyle(document.getElementById('input')).fontSize : null`,
    );
    check('mobile: composer input is 16px (iOS auto-zoom guard)', inputFont === '16px', `font=${inputFont}`);

    // The desktop flow may have left the Files tab active — the chat rows
    // live in the Chats tab. Radix TabsTrigger needs a trusted pointer
    // sequence (same as check 7).
    {
      const rect = JSON.parse(
        await evalJs(
          `(() => { const t = [...document.querySelectorAll('[role="tab"]')].find((t) => t.textContent === 'Chats'); const r = t.getBoundingClientRect(); return JSON.stringify({ x: r.x + r.width / 2, y: r.y + r.height / 2 }); })()`,
        ),
      );
      await send('Input.dispatchMouseEvent', { type: 'mousePressed', x: rect.x, y: rect.y, button: 'left', buttons: 1, clickCount: 1 });
      await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: rect.x, y: rect.y, button: 'left', clickCount: 1 });
    }
    await sleep(500);
    const menuVisible = await evalJs(`(() => {
      const menu = document.querySelector('#conversation-list [aria-label^="Chat actions"]');
      return menu ? getComputedStyle(menu).opacity !== '0' : 'no-rows';
    })()`);
    check('mobile: chat-row menu visible without hover (touch)', menuVisible === true, `visible=${menuVisible}`);

    // Drawer: the toggle FLIPS it (the desktop flow may have left it open —
    // on mobile that renders as the overlay drawer being out).
    const before = await evalJs(`document.getElementById('sidebar-layer').classList.contains('open')`);
    await evalJs(`document.getElementById('sidebar-toggle').click()`);
    await sleep(300);
    const after = await evalJs(`document.getElementById('sidebar-layer').classList.contains('open')`);
    await evalJs(`document.getElementById('sidebar-toggle').click()`);
    await sleep(300);
    await evalJs(`document.getElementById('sidebar-toggle').click()`);
    await sleep(300);
    // Leave it closed (the chat must be visible for the dock check below).
    check('mobile: drawer toggles from the header button', before !== after, `open ${before} → ${after}`);

    // Drawer geometry + the way-back affordances (regression-pinned):
    // the Tabs root carries `flex-1`, and this layer is a definite-width
    // fixed box — without `flex: none` on #sidebar the grow silently turns
    // the drawer into a full-screen sheet (backdrop + every close path
    // dead except selecting a chat). With it: authored width, below the
    // top bar, the toggle (X) reachable, and a dimmed backdrop that
    // closes on tap.
    {
      const drawerOpen = await evalJs(`document.getElementById('sidebar-layer').classList.contains('open')`);
      if (!drawerOpen) {
        await evalJs(`document.getElementById('sidebar-toggle').click()`);
        await sleep(400);
      }
      const geom = JSON.parse(
        await evalJs(`(() => {
          const s = document.getElementById('sidebar').getBoundingClientRect();
          const bar = document.getElementById('top-bar').getBoundingClientRect();
          return JSON.stringify({ w: s.width, left: s.left, top: s.top, barBottom: bar.bottom });
        })()`),
      );
      check(
        'mobile: drawer keeps its authored width, below the top bar',
        geom.left === 0 && Math.abs(geom.top - geom.barBottom) < 1 && geom.w <= 301,
        `w=${Math.round(geom.w)} top=${Math.round(geom.top)} bar=${Math.round(geom.barBottom)}`,
      );
      const toggle = JSON.parse(
        await evalJs(`(() => {
          const t = document.getElementById('sidebar-toggle').getBoundingClientRect();
          const el = document.elementFromPoint(t.x + t.width / 2, t.y + t.height / 2);
          return JSON.stringify({ reachable: !!el && !!el.closest('#sidebar-toggle') });
        })()`),
      );
      check('mobile: toggle reachable while the drawer is open (visible close control)', toggle.reachable);
      const dimmed = JSON.parse(
        await evalJs(`(() => {
          const x = innerWidth - 20;
          const y = Math.round(innerHeight / 2);
          const el = document.elementFromPoint(x, y);
          return JSON.stringify({ x, y, hit: el ? el.id : 'none' });
        })()`),
      );
      if (dimmed.hit !== 'sidebar-backdrop') {
        check('mobile: dimmed backdrop exposed right of the drawer', false, `hit=${dimmed.hit}`);
      } else {
        await send('Input.dispatchMouseEvent', { type: 'mousePressed', x: dimmed.x, y: dimmed.y, button: 'left', buttons: 1, clickCount: 1 });
        await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: dimmed.x, y: dimmed.y, button: 'left', clickCount: 1 });
        await sleep(400);
        const closed = await evalJs(`document.getElementById('sidebar-layer').classList.contains('closed')`);
        check('mobile: backdrop tap closes the drawer without selecting', closed);
      }
      // Reopen, then the ⋯ tap must open the row menu WITHOUT closing the
      // drawer (the tap's click used to bubble into the row = select =
      // drawer close; rename/delete became unreachable mid-action).
      await evalJs(`document.getElementById('sidebar-toggle').click()`);
      await sleep(400);
      const menuRect = JSON.parse(
        await evalJs(
          `(() => { const b = document.querySelector('#conversation-list [aria-label^="Chat actions"]'); const r = b.getBoundingClientRect(); return JSON.stringify({ x: r.x + r.width / 2, y: r.y + r.height / 2 }); })()`,
        ),
      );
      await send('Input.dispatchMouseEvent', { type: 'mousePressed', x: menuRect.x, y: menuRect.y, button: 'left', buttons: 1, clickCount: 1 });
      await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: menuRect.x, y: menuRect.y, button: 'left', clickCount: 1 });
      await sleep(400);
      const menuAndDrawer = JSON.parse(
        await evalJs(`JSON.stringify({
          menu: !!document.querySelector('[role="menu"]'),
          drawerOpen: document.getElementById('sidebar-layer').classList.contains('open'),
        })`),
      );
      check(
        'mobile: ⋯ tap opens the row menu and keeps the drawer open',
        menuAndDrawer.menu && menuAndDrawer.drawerOpen,
        JSON.stringify(menuAndDrawer),
      );
      await evalJs(
        `[...document.querySelectorAll('[role="menuitem"]')].find((i) => i.textContent.includes('Rename'))?.click()`,
      );
      await waitFor(`!!document.querySelector('#conversation-list input[aria-label="Chat name"]')`, 'mobile rename input');
      const renameInDrawer = await evalJs(
        `document.getElementById('sidebar-layer').classList.contains('open') && !!document.querySelector('#conversation-list input[aria-label="Chat name"]')`,
      );
      check('mobile: Rename keeps the drawer open with the inline input', renameInDrawer === true);
      await send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 });
      await send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 });
      await sleep(300);
    }

    // The dock is a full-screen sheet on mobile. The drawer must be OPEN —
    // a closed drawer sits at translateX(-105%), putting the tab's rect
    // off-screen where a trusted pointer event hits nothing.
    const drawerOpen = await evalJs(`document.getElementById('sidebar-layer').classList.contains('open')`);
    if (!drawerOpen) {
      await evalJs(`document.getElementById('sidebar-toggle').click()`);
      await sleep(400);
    }
    {
      const rect = JSON.parse(
        await evalJs(
          `(() => { const t = [...document.querySelectorAll('[role="tab"]')].find((t) => t.textContent === 'Files'); const r = t.getBoundingClientRect(); return JSON.stringify({ x: r.x + r.width / 2, y: r.y + r.height / 2 }); })()`,
        ),
      );
      await send('Input.dispatchMouseEvent', { type: 'mousePressed', x: rect.x, y: rect.y, button: 'left', buttons: 1, clickCount: 1 });
      await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: rect.x, y: rect.y, button: 'left', clickCount: 1 });
    }
    await sleep(700);
    await evalJs(`document.querySelector('#explorer div[title$="/preview-me.md"]').click()`);
    await waitFor(`!!document.getElementById('right-dock')`, 'dock sheet open');
    await sleep(400);
    const sheet = JSON.parse(
      await evalJs(`(() => {
        const d = document.getElementById('right-dock');
        const r = d.getBoundingClientRect();
        return JSON.stringify({ w: r.width, full: Math.abs(r.width - innerWidth) < 2, close: !!d.querySelector('[aria-label="Close dock"]') });
      })()`),
    );
    check('mobile: dock opens as a full-screen sheet', sheet.full && sheet.close, `w=${Math.round(sheet.w)}`);
    await evalJs(
      `[...document.querySelectorAll('#right-dock button')].find((b) => b.getAttribute('aria-label') === 'Close dock').click()`,
    );
    await waitFor(`!document.getElementById('right-dock')`, 'dock sheet closed');

    await send('Emulation.clearDeviceMetricsOverride');
    await send('Emulation.setTouchEmulationEnabled', { enabled: false });
    await sleep(400);
  }

  // Done.
  const failed = results.filter((r) => !r.ok);
  console.log(
    failed.length === 0
      ? `\nui-check: ${results.length}/${results.length} passed`
      : `\nui-check: ${failed.length} FAILED of ${results.length}`,
  );
  process.exit(failed.length === 0 ? 0 : 1);
}

main()
  .catch((err) => {
    console.error('ui-check failed:', err.message);
    process.exit(1);
  });
