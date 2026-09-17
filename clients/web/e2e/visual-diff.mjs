/**
 * visual-diff.mjs — pixel comparison for the screenshot tour, ZERO extra
 * dependencies: the PNG decode and the per-pixel diff run INSIDE the
 * headless Chrome the tour already launched (createImageBitmap +
 * OffscreenCanvas). Node only ships base64 blobs in and receives stats +
 * a diff heatmap out — the harness's "no extra dependencies" stance holds.
 *
 * The comparison routine is a plain function serialized into
 * Runtime.evaluate via .toString() (evaluated with awaitPromise), so it
 * must be fully self-contained: no closure references, no imports.
 *
 * Matching rule: a pixel differs when ANY channel moves more than
 * `tolerance` (0–255). Same-machine, same-Chrome, dsf-pinned captures are
 * near-binary — identical or genuinely different — so a channel tolerance
 * absorbs antialiasing noise without the full anti-aliased-pixel
 * classification heavier tools do. Acceptance is a BUDGET (max % of
 * pixels), not zero: content that legitimately varies between runs (the
 * tool card's frozen elapsed chip, relative sidebar times) lives far
 * under it, and real layout/color drift blows it instantly.
 *
 * Provides: parseVisualArgs, updateBaselines, compareShot, SHOT_NAMES_DOC
 * Depends: node:fs only
 */
import { readFileSync, writeFileSync, mkdirSync, copyFileSync, existsSync } from 'node:fs';
import path from 'node:path';

/** The routine that runs INSIDE the page (self-contained; serialized). */
async function fluxVisualDiffRoutine(aB64, bB64, tolerance) {
  const load = async (b64) => {
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return createImageBitmap(new Blob([bytes], { type: 'image/png' }));
  };
  let a;
  let b;
  try {
    [a, b] = await Promise.all([load(aB64), load(bB64)]);
  } catch (e) {
    return { error: 'png decode failed: ' + (e?.message ?? String(e)) };
  }
  if (a.width !== b.width || a.height !== b.height) {
    return {
      error: `size mismatch — baseline ${a.width}×${a.height} vs current ${b.width}×${b.height}`,
    };
  }
  const w = a.width;
  const h = a.height;
  const ca = new OffscreenCanvas(w, h);
  const cb = new OffscreenCanvas(w, h);
  const cd = new OffscreenCanvas(w, h);
  const xa = ca.getContext('2d', { willReadFrequently: true });
  const xb = cb.getContext('2d', { willReadFrequently: true });
  const xd = cd.getContext('2d', { willReadFrequently: true });
  xa.drawImage(a, 0, 0);
  xb.drawImage(b, 0, 0);
  const A = xa.getImageData(0, 0, w, h).data;
  const B = xb.getImageData(0, 0, w, h).data;
  const img = xd.createImageData(w, h);
  const D = img.data;
  let diff = 0;
  for (let i = 0; i < A.length; i += 4) {
    const dr = Math.abs(A[i] - B[i]);
    const dg = Math.abs(A[i + 1] - B[i + 1]);
    const db = Math.abs(A[i + 2] - B[i + 2]);
    if (dr > tolerance || dg > tolerance || db > tolerance) {
      diff++;
      // Diff heatmap: the BASELINE pixel tinted toward red by magnitude;
      // unchanged pixels dim to 22% so the eye lands on what moved.
      const m = Math.min(1, (dr + dg + db) / 384);
      D[i] = A[i] + (255 - A[i]) * m;
      D[i + 1] = A[i + 1] * (1 - m);
      D[i + 2] = A[i + 2] * (1 - m);
    } else {
      D[i] = A[i] * 0.22;
      D[i + 1] = A[i + 1] * 0.22;
      D[i + 2] = A[i + 2] * 0.22;
    }
    D[i + 3] = 255;
  }
  xd.putImageData(img, 0, 0);
  const out = { width: w, height: h, diffPixels: diff, ratio: diff / (w * h), diffPngB64: null };
  if (diff > 0) {
    const blob = await cd.convertToBlob({ type: 'image/png' });
    const buf = new Uint8Array(await blob.arrayBuffer());
    let s = '';
    for (let i = 0; i < buf.length; i += 0x8000) {
      s += String.fromCharCode.apply(null, buf.subarray(i, i + 0x8000));
    }
    out.diffPngB64 = btoa(s);
  }
  return out;
}

/** CLI flags for the visual modes (parsed here, consumed by shots.mjs).
 *  --check            re-tour, then compare every shot against .baseline/
 *  --update-baseline  re-tour, then re-snapshot the shots into .baseline/
 *  --max-diff=PCT     acceptance budget, % of pixels (default 0.2)
 *  --tolerance=N      per-channel noise floor 0–255 (default 16) */
export function parseVisualArgs(argv) {
  const out = { mode: 'shots', maxDiff: 0.2, tolerance: 16 };
  for (const arg of argv) {
    if (arg === '--check') out.mode = 'check';
    else if (arg === '--update-baseline') out.mode = 'update-baseline';
    else if (arg.startsWith('--max-diff=')) {
      const v = Number(arg.slice('--max-diff='.length));
      if (!(v >= 0 && v <= 100)) throw new Error(`--max-diff must be 0–100, got: ${arg}`);
      out.maxDiff = v;
    } else if (arg.startsWith('--tolerance=')) {
      const v = Number(arg.slice('--tolerance='.length));
      if (!(v >= 0 && v <= 255)) throw new Error(`--tolerance must be 0–255, got: ${arg}`);
      out.tolerance = v;
    } else if (arg === '--help' || arg === '-h') {
      out.mode = 'help';
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }
  return out;
}

/** Snapshot the fresh shots as the session's accepted appearance —
 * local + gitignored BY DESIGN: the baseline bounds one work batch, it
 * never pins the design into the repo. */
export function updateBaselines(shotsDir, baselineDir, names) {
  mkdirSync(baselineDir, { recursive: true });
  for (const name of names) {
    copyFileSync(path.join(shotsDir, `${name}.png`), path.join(baselineDir, `${name}.png`));
  }
}

/** Compare one shot against its baseline IN THE PAGE. Writes the diff
 * heatmap next to the current shot (`<name>.diff.png`) when anything
 * moved. Returns { ok, diffPixels, ratio, width, height } — or throws on
 * infrastructure failure (missing baseline, size mismatch, decode). */
export async function compareShot({ evalJs, shotsDir, baselineDir, name, tolerance }) {
  const baselinePath = path.join(baselineDir, `${name}.png`);
  const currentPath = path.join(shotsDir, `${name}.png`);
  if (!existsSync(baselinePath)) {
    throw new Error(`no baseline for ${name} — run --update-baseline first`);
  }
  const aB64 = readFileSync(baselinePath).toString('base64');
  const bB64 = readFileSync(currentPath).toString('base64');
  const expr =
    `(${fluxVisualDiffRoutine.toString()})(${JSON.stringify(aB64)}, ${JSON.stringify(bB64)}, ${tolerance})`;
  const r = await evalJs(expr);
  if (!r || r.error) throw new Error(`${name}: ${r?.error ?? 'in-page compare returned nothing'}`);
  if (r.diffPngB64) {
    writeFileSync(path.join(shotsDir, `${name}.diff.png`), Buffer.from(r.diffPngB64, 'base64'));
  }
  return { ok: true, diffPixels: r.diffPixels, ratio: r.ratio, width: r.width, height: r.height };
}
