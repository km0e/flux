/**
 * filePreview.ts — open one file as a right-dock tab and land the fetched
 * content on it.
 *
 * Deliberately its own module (not a fs.ts export): Explorer clicks and
 * the round-artifacts panel (F-11) share ONE address space — the tab id
 * IS the path — and the Explorer tests mock the fs layer (readFile), so
 * this module must depend on fs.ts's EXPORTS, not live inside it (an
 * internal call would bypass the module mock and hit the wire).
 *
 * Provides: openFilePreview
 * Depends: services/fs.ts, core/state.ts
 */
import { readFile, isDisconnect } from './fs';
import { useFlux } from '../core/state';

/** Open one file as a dock tab (or activate it if already open) and land
 * the fetch result on THAT tab, stale-guarded by id. */
export function openFilePreview(path: string): void {
  const name = path.split('/').filter(Boolean).pop() ?? path;
  const id = path;
  useFlux.getState().addFileTab({
    id,
    path,
    name,
    content: '',
    truncated: false,
    loading: true,
    rawView: false,
  });
  void readFile(path).then((r) => {
    // Stale guard: only land the reply if this tab is still open.
    if (!useFlux.getState().openFiles.some((t) => t.id === id)) return;
    if (r.error && !isDisconnect(r.error)) {
      // Unified reporting — the pane itself shows a neutral placeholder.
      useFlux.getState().pushToast('error', `Could not read ${name} — ${r.error}`);
    }
    useFlux.getState().patchFileTab(id, {
      content: r.content ?? '',
      truncated: r.truncated ?? false,
      size: r.size,
      error: r.error,
      loading: false,
    });
  });
}
