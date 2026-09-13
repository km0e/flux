/**
 * shots.mjs — screenshot tour of the Flux web UI (design review aid).
 *
 * Drives the SAME real stack as ui-check.mjs (cargo flux-server +
 * fake-provider.mjs + headless Chrome over raw CDP — node ≥ 22 global
 * WebSocket, zero extra dependencies) but asserts nothing: it walks the
 * surface a designer/reviewer cares about and saves PNGs.
 *
 * Shots (light + dark where it matters):
 *   1. empty-state      — fresh chat, prompt cards
 *   2. conversation     — user bubble + assistant reply + completed tool card
 *   3. settings         — the (lazy-loaded) Settings dialog, Providers section
 *   4. mobile           — 390×844: conversation with the drawer out
 *
 * Usage:  npm run shots          (from clients/web) → e2e/.shots/*.png
 * Requires the same as ui-check: node ≥ 22, cargo, Chrome/Chromium.
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
const OUT_DIR = path.join(HERE, '.shots');

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

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

const children = [];
function startDetached(cmd, args, logFile) {
  const out = { 'ignore': 'ignore', 'inherit': 'inherit' }[logFile] ?? logFile;
  children.push(spawn(cmd, args, { stdio: ['ignore', out, out] }));
}
function cleanup() {
  for (const c of children) {
    try { c.kill('SIGKILL'); } catch { /* gone */ }
  }
}
process.on('exit', cleanup);
process.on('SIGINT', () => process.exit(130));
process.on('SIGTERM', () => process.exit(143));

// ── minimal gRPC-Web + protobuf helpers (same wire the page speaks) ──────

function lpFrame(msg) {
  const out = new Uint8Array(5 + msg.length);
  out[0] = 0;
  new DataView(out.buffer).setUint32(1, msg.length);
  out.set(msg, 5);
  return out;
}
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
  for (const line of new TextDecoder().decode(trailer).split('\n')) {
    if (line.startsWith('grpc-status:')) return Number(line.slice(12).trim());
  }
  return null;
}
/** AddProviderRequest {id(1), kind(2), url(3), api_key(4)} — hand-rolled. */
function encodeAddProviderRequest(req) {
  const field = (num, bytes) => {
    const tag = (num << 3) | 2;
    const head = [];
    let t = tag;
    while (t > 0x7f) { head.push((t & 0x7f) | 0x80); t >>>= 7; }
    head.push(t);
    let len = bytes.length;
    const lens = [];
    while (len > 0x7f) { lens.push((len & 0x7f) | 0x80); len >>>= 7; }
    lens.push(len);
    return new Uint8Array([...head, ...lens, ...bytes]);
  };
  const str = (s) => new TextEncoder().encode(s);
  const parts = [field(1, str(req.id)), field(2, str('openai'))];
  if (req.url) parts.push(field(3, str(req.url)));
  if (req.api_key) parts.push(field(4, str(req.api_key)));
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let off = 0;
  for (const p of parts) { out.set(p, off); off += p.length; }
  return out;
}

// ── CDP driver (same shape as ui-check) ─────────────────────────────────

async function connectCdp(port, urlFilter) {
  let page = null;
  for (let i = 0; i < 40 && !page; i++) {
    try {
      const targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
      page = targets.filter((t) => t.type === 'page' && t.url.includes(urlFilter)).pop();
    } catch { /* chrome not up yet */ }
    if (!page) await sleep(250);
  }
  if (!page) throw new Error('no browser page target found');

  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
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
  const evalJs = async (expression) => {
    const r = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) {
      throw new Error(r.exceptionDetails.exception?.description ?? 'page exception');
    }
    return r.result.value;
  };
  const waitFor = async (expression, label, timeoutMs = 20_000) => {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const v = await evalJs(expression);
      if (v) return v;
      if (Date.now() > deadline) throw new Error(`timeout waiting for ${label}`);
      await sleep(250);
    }
  };
  /** Screenshot → OUT_DIR/<name>.png (fonts flushed first). */
  const shot = async (name) => {
    await evalJs(`document.fonts.ready.then(() => {})`);
    await sleep(150);
    const { data } = await send('Page.captureScreenshot', {
      format: 'png',
      captureBeyondViewport: false,
    });
    const file = path.join(OUT_DIR, `${name}.png`);
    writeFileSync(file, Buffer.from(data, 'base64'));
    console.log(`  📸 ${file}`);
  };
  return { ws, send, evalJs, waitFor, shot };
}

function findChrome() {
  if (process.env.FLUX_CHROME) return process.env.FLUX_CHROME;
  for (const c of ['/opt/google/chrome/chrome', '/usr/bin/google-chrome', '/usr/bin/chromium', '/usr/bin/chromium-browser']) {
    if (existsSync(c)) return c;
  }
  try {
    execSync('command -v chrome', { stdio: 'ignore' });
    return 'chrome';
  } catch { /* fall through */ }
  throw new Error('no Chrome/Chromium found — install one or set FLUX_CHROME=/path/to/chrome');
}

// ── Tour ────────────────────────────────────────────────────────────────

async function main() {
  if (typeof WebSocket !== 'function') {
    console.error('shots needs node ≥ 22 (global WebSocket).');
    process.exit(2);
  }
  if (!existsSync(SERVER_BIN)) {
    console.log('building flux-server (cargo build -p flux-server)…');
    execSync('cargo build -p flux-server', { cwd: REPO_ROOT, stdio: 'inherit' });
  }
  if (!existsSync(path.join(DIST, 'index.html'))) {
    console.log('building web UI (npm run build)…');
    execSync('npm run build', { cwd: WEB_DIR, stdio: 'inherit' });
  }

  rmSync(OUT_DIR, { recursive: true, force: true });
  mkdirSync(OUT_DIR, { recursive: true });

  const serverPort = await freePort();
  const providerPort = await freePort();

  const workdir = path.join(tmpdir(), 'flux-shots-wd');
  rmSync(workdir, { recursive: true, force: true });
  mkdirSync(workdir, { recursive: true });
  writeFileSync(
    path.join(workdir, 'notes.md'),
    '# demo notes\n\nA sample file for the explorer preview.\n',
  );

  const tmp = mkdtempSync(path.join(tmpdir(), 'flux-shots-'));

  console.log('starting fake provider + flux-server…');
  startDetached(process.execPath, [path.join(HERE, 'fake-provider.mjs'), String(providerPort)], 'inherit');
  startDetached(
    SERVER_BIN,
    ['--db-path', path.join(tmp, 'shots.db'), '--web-assets-dir', DIST,
     '--host', '127.0.0.1', '--port', String(serverPort)],
    'inherit',
  );
  for (let i = 0; i < 60; i++) {
    try { if ((await fetch(`http://127.0.0.1:${serverPort}/`)).ok) break; } catch { /* not up */ }
    await sleep(250);
  }

  const chrome = findChrome();
  const cdpPort = await freePort();
  console.log(`launching headless chrome (${chrome})…`);
  startDetached(
    chrome,
    ['--headless=new', '--no-sandbox', '--disable-gpu', '--hide-scrollbars',
     '--window-size=1440,900', `--remote-debugging-port=${cdpPort}`,
     `--user-data-dir=${path.join(tmp, 'profile')}`,
     `http://127.0.0.1:${serverPort}/`],
    'ignore',
  );
  const { send, evalJs, waitFor, shot } = await connectCdp(cdpPort, String(serverPort));

  // Pin desktop metrics at dsf 2 — crisp type for the review.
  await send('Emulation.setDeviceMetricsOverride', { width: 1440, height: 900, deviceScaleFactor: 2, mobile: false });

  await waitFor(`!!document.getElementById('new-chat-btn')`, 'app mount');

  // Register the fake provider (raw gRPC-Web — the same wire the page uses).
  {
    const body = lpFrame(encodeAddProviderRequest({
      id: 'default',
      url: `http://127.0.0.1:${providerPort}/v1`,
      api_key: 'sk-shots',
    }));
    const resp = await fetch(`http://127.0.0.1:${serverPort}/flux.v1.ProviderService/AddProvider`, {
      method: 'POST', headers: { 'content-type': 'application/grpc-web+proto' }, body,
    });
    const { data, trailer } = parseGrpcWebBody(new Uint8Array(await resp.arrayBuffer()));
    if (trailerGrpcStatus(trailer) !== 0) throw new Error('provider_add refused');
    if (data.length === 0) throw new Error('provider_add: no response body');
  }

  // Create the chat in the marker workdir through the real dialog.
  await evalJs(`document.getElementById('new-chat-btn').click()`);
  await waitFor(`!!document.querySelector('select')`, 'new-chat dialog');
  await evalJs(`(() => {
    const s = document.querySelector('select');
    const setter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set;
    setter.call(s, 'default');
    s.dispatchEvent(new Event('change', { bubbles: true }));
  })()`);
  await evalJs(`(() => {
    const m = document.querySelector('input[list="provider-models-default"]');
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
    setter.call(m, 'shots-model');
    m.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  await waitFor(`[...document.querySelectorAll('button')].some((b) => b.textContent.includes('Create here') && !b.disabled)`, 'pin ready');
  await evalJs(`[...document.querySelectorAll('button')].find((b) => b.textContent.trim() === 'Root').click()`);
  await waitFor(`!!document.querySelector('button[title="tmp"]')`, 'root listing');
  await evalJs(`document.querySelector('button[title="tmp"]').click()`);
  await waitFor(`!!document.querySelector('button[title="flux-shots-wd"]')`, 'tmp listing');
  await evalJs(`document.querySelector('button[title="flux-shots-wd"]').click()`);
  await waitFor(
    `[...document.querySelectorAll('button')].some((b) => b.textContent.includes('Create here') && !b.disabled)`,
    'marker workdir listing',
  );
  await evalJs(`[...document.querySelectorAll('button')].find((b) => b.textContent.includes('Create here')).click()`);
  await waitFor(`!!document.querySelector('.chat-pane')`, 'chat pane');
  await waitFor(`!!document.querySelector('.fx-empty-state')`, 'empty state');

  // 1. Empty state (light).
  await shot('1-empty-light');

  // 2. Conversation: user bubble → assistant markdown reply + tool card.
  await evalJs(`(() => {
    const ta = document.getElementById('input');
    ta.value = 'inspect the project and summarize the build setup';
    ta.dispatchEvent(new Event('input', { bubbles: true }));
    document.getElementById('send').click();
  })()`);
  await waitFor(
    `document.querySelectorAll('.tool.done').length >= 1 &&
     (document.querySelector('.chat-pane')?.textContent ?? '').includes('ran fine')`,
    'tool round wrap-up',
  );
  await evalJs(`document.querySelector('.chat-pane').scrollTop = 1e9`);
  await sleep(400);
  await shot('2-conversation-light');

  // Dark theme, same conversation.
  await evalJs(`document.documentElement.dataset.theme = 'dark'`);
  await sleep(300);
  await shot('3-conversation-dark');

  // 4. Settings — ALSO the runtime smoke of the lazy-loaded dialog chunk:
  // the gear click must pull SettingsDialog-*.js and render the panels.
  await evalJs(`document.getElementById('settings-button').click()`);
  await waitFor(
    `(() => { const d = document.querySelector('[role="dialog"]'); return !!d && d.textContent.includes('Providers'); })()`,
    'settings dialog (lazy chunk)',
  );
  await sleep(600); // providers list fetch + catalog probe
  await shot('4-settings-providers-light');

  // Close settings, back to dark conversation, close the drawer → plain chat.
  await evalJs(`document.querySelector('[role="dialog"] button[aria-label="Close"], [role="dialog"] [data-state="open"] [aria-label="Close"]')?.click()`);
  await sleep(300);

  // 5. MOBILE regime — 390×844 (dsf 3, like a real phone).
  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 3, mobile: true });
  await send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 });
  await sleep(600);
  // Drawer out (the conversation behind shows the overlay regime).
  const drawerOpen = await evalJs(`document.getElementById('sidebar-layer').classList.contains('open')`);
  if (!drawerOpen) {
    await evalJs(`document.getElementById('sidebar-toggle').click()`);
    await sleep(400);
  }
  await shot('5-mobile-drawer-dark');
  await evalJs(`document.getElementById('sidebar-toggle').click()`);
  await sleep(400);
  await shot('6-mobile-chat-dark');

  console.log('\ndone.');
  process.exit(0);
}

main().catch((err) => {
  console.error('shots failed:', err.message);
  process.exit(1);
});
