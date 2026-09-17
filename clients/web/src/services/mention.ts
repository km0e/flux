/**
 * mention.ts — file-path completion for the composer's @-references.
 *
 * Owns a per-session directory cache (fs_list round-trips keyed by abs
 * dir, 15s TTL — the Explorer's refresh rhythm; the AGENT creates files
 * mid-session, so a permanent cache would hide exactly the files the
 * user wants to reference next). Independent of the Explorer's tree
 * state — completion works with the Files tab never opened.
 *
 * Token scope: '@' + everything non-whitespace up to the caret. A '/'
 * inside the token drills into subdirectories ('src/li' → list src,
 * match 'li'). Completions are workdir-RELATIVE paths (dirs trail with
 * '/'), the form the tool boundary resolves.
 *
 * Provides: mentionTokenAt, relativeToWorkdir, completeMention,
 *           _resetMentionForTest
 * Depends: services/fs.ts
 */
import { listDir } from './fs';

/** Cache entries older than this re-list (agent-created files appear). */
const TTL_MS = 15_000;
const MAX_ITEMS = 12;

interface DirEntry {
  at: number;
  dirs: string[];
  files: string[];
}

const cache = new Map<string, DirEntry>();

/** The @-token under the caret, or null. Whitespace closes a mention;
 * the '@' must open the segment (no @-mid-word surprises). */
export function mentionTokenAt(
  text: string,
  caret: number,
): { start: number; token: string } | null {
  let i = caret - 1;
  while (i >= 0 && !/\s/.test(text[i] as string)) i--;
  const seg = text.slice(i + 1, caret);
  if (!seg.startsWith('@')) return null;
  return { start: i + 1, token: seg.slice(1) };
}

/** An absolute tree path (the Explorer's node ids) → the chat-relative
 * form the composer inserts. Paths outside the workdir stay absolute
 * (the tool boundary will reject them — honest over surprising). */
export function relativeToWorkdir(path: string, workdir: string): string {
  if (!workdir) return path;
  const strip = (p: string) => p.replace(/\/+$/, '');
  const w = strip(workdir);
  const p = strip(path);
  if (p === w) return '';
  if (p.startsWith(w + '/')) return p.slice(w.length + 1);
  return p;
}

/** Completions for one token: subdirectory drill-ins (trailing '/') and
 * files, workdir-relative, dirs first, capped. */
export async function completeMention(
  workdir: string,
  token: string,
): Promise<string[]> {
  if (!workdir) return [];
  const lastSlash = token.lastIndexOf('/');
  const dirPart = lastSlash === -1 ? '' : token.slice(0, lastSlash + 1);
  const base = (lastSlash === -1 ? token : token.slice(lastSlash + 1)).toLowerCase();
  const root = workdir.replace(/\/+$/, '');
  const absDir = dirPart ? `${root}/${dirPart}`.replace(/\/+$/, '') : root;
  const listing = await listDirCached(absDir);
  if (!listing) return [];
  const dirs = listing.dirs
    .filter((d) => d.toLowerCase().includes(base))
    .sort()
    .map((d) => dirPart + d + '/');
  const files = listing.files
    .filter((f) => f.toLowerCase().includes(base))
    .sort()
    .map((f) => dirPart + f);
  return [...dirs, ...files].slice(0, MAX_ITEMS);
}

async function listDirCached(absDir: string): Promise<DirEntry | null> {
  const hit = cache.get(absDir);
  if (hit && Date.now() - hit.at < TTL_MS) return hit;
  const r = await listDir(absDir);
  if (r.error) return hit ?? null; // a stale listing beats nothing
  const dirs: string[] = [];
  const files: string[] = [];
  for (const e of r.entries) {
    (e.kind === 'dir' ? dirs : files).push(e.name);
  }
  const fresh = { at: Date.now(), dirs, files };
  cache.set(absDir, fresh);
  return fresh;
}

/** Test isolation — the cache outlives a wiped DOM. */
export function _resetMentionForTest(): void {
  cache.clear();
}
