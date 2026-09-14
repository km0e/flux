/**
 * Explorer.tsx — file tree for the active chat's workdir (sidebar Files tab).
 *
 * Built on react-arborist (a mature React tree with keyboard
 * navigation, virtualization and a11y — Radix has no tree). Nodes are
 * addressed by full path (id IS the path) so the tree and the right-dock
 * preview share one address space. Directories lazy-load their children
 * via `fs_list` on first expand (onToggle). Drag-and-drop is disabled —
 * this is a browser, not a mover.
 *
 * Refresh: a manual button re-lists every loaded directory in place
 * (expansion state preserved), and an optional auto-refresh interval
 * (Off/5s/15s/60s, persisted) repeats it. The component unmounts with the
 * Files tab, so the timer never runs hidden. Listing failures surface on
 * the unified toast stack (Toasts.tsx) — never inline on the rows.
 *
 * The component remounts per workdir (Sidebar passes key={workdir}).
 *
 * Provides: Explorer
 * Depends: services/fs.ts, core/state.ts, core/prefs.ts, react-arborist,
 *          components/ui/*, components/FileIcon.tsx
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { Tree, type NodeApi, type NodeRendererProps, type TreeApi } from 'react-arborist';
import { ChevronRight, FolderIcon as FolderGlyph, FolderOpen, RotateCw } from 'lucide-react';
import { listDir, isDisconnect } from '../services/fs';
import { openFilePreview } from '../services/filePreview';
import { useFlux } from '../core/state';
import { cn } from '../lib/cn';
import { fmtBytes } from '../lib/format';
import { IconButton } from './ui';
import type { FsEntry, GitEntryStatus } from '../core/types';
import { FileIcon } from './FileIcon';

export { Explorer };

/** Fixed auto-refresh cadence for the tree. Deliberately not configurable
 * yet — one honest default beats a picker nobody asked for; revisit when a
 * workflow actually needs another cadence. */
const AUTO_REFRESH_MS = 15_000;

/** Touch probe — evaluated ONCE per page load (a device does not gain or
 * lose touch mid-session); per-render matchMedia calls were a waste. */
const TOUCH_DEVICE =
  typeof matchMedia === 'function' && matchMedia('(hover: none)').matches;

/** Git status badge vocabulary — the letter, the color token, and the full
 * word for the tooltip (VS Code explorer language). */
const GIT_BADGE: Record<GitEntryStatus, { letter: string; className: string; word: string }> = {
  modified: { letter: 'M', className: 'text-warn', word: 'modified' },
  added: { letter: 'A', className: 'text-success', word: 'added' },
  untracked: { letter: 'U', className: 'text-success', word: 'untracked' },
  conflicted: { letter: 'C', className: 'text-danger', word: 'merge conflict' },
};

/** One node in the workdir tree. `id` IS the absolute path. Directories
 * carry `children: null` until first expanded (the lazy-load marker). */
interface FsNode {
  id: string;
  name: string;
  kind: 'dir' | 'file';
  size?: number;
  git?: GitEntryStatus;
  /** null = unloaded directory (lazy-load marker); undefined = file. */
  children?: FsNode[] | null;
}

function dirName(path: string): string {
  return path.split('/').filter(Boolean).pop() ?? path;
}

function entriesToNodes(dir: string, entries: FsEntry[]): FsNode[] {
  return entries.map((e) => ({
    id: (dir === '/' ? '' : dir) + '/' + e.name,
    name: e.name,
    kind: e.kind,
    size: e.size,
    git: e.git,
    children: e.kind === 'dir' ? null : undefined,
  }));
}

/** Container height probe — react-arborist virtualizes against a px height. */
function useHeight<T extends HTMLElement>(): [React.RefObject<T | null>, number] {
  const ref = useRef<T>(null);
  const [height, setHeight] = useState(0);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      setHeight(entries[0]?.contentRect.height ?? 0);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, height];
}

export default function Explorer(): React.ReactElement {
  const workdir = useFlux((s) => s.chats.find((c) => c.id === s.activeChatId)?.workdir) ?? '';
  const [nodes, setNodes] = useState<FsNode[]>(() =>
    workdir
      ? [
          {
            id: workdir,
            name: dirName(workdir),
            kind: 'dir',
            children: null,
          },
        ]
      : [],
  );
  const [loaded, setLoaded] = useState<Set<string>>(() => new Set());
  const [failedDirs, setFailedDirs] = useState<Set<string>>(() => new Set());
  const [refreshing, setRefreshing] = useState(false);
  const treeRef = useRef<TreeApi<FsNode> | null>(null);
  // Latest data for non-reactive lookups (onToggle closes over the ref, not
  // the render-time snapshot — the callback must see freshly loaded children).
  const nodesRef = useRef<FsNode[]>(nodes);
  nodesRef.current = nodes;
  const [wrapRef, height] = useHeight<HTMLDivElement>();

  const pushToast = useCallback(
    (text: string) => useFlux.getState().pushToast('error', text),
    [],
  );

  /** (Re-)list one directory: called lazily on first expand and again by
   * manual/auto refresh for every loaded dir. Real failures surface on the
   * unified toast stack; a disconnect stays SILENT — the top bar owns
   * outage communication, and the auto-refresh retries on its own once the
   * connection is back. */
  const loadDir = useCallback(
    async (dir: string) => {
      setLoaded((prev) => new Set(prev).add(dir));
      const r = await listDir(dir);
      if (r.error) {
        setFailedDirs((prev) => new Set(prev).add(dir));
        if (!isDisconnect(r.error)) {
          pushToast(`Failed to list ${dirName(dir)} — ${r.error}`);
        }
        return;
      }
      setFailedDirs((prev) => {
        if (!prev.has(dir)) return prev;
        const next = new Set(prev);
        next.delete(dir);
        return next;
      });
      const kids = entriesToNodes(r.path ?? dir, r.entries);
      // No-change guard: the auto-refresh tick re-lists every loaded dir;
      // when the listing is identical to what the tree already holds,
      // skip the state write — a new nodes array would re-render every
      // visible row every 15s even in a still directory.
      const current = nodesById(nodesRef.current, dir)?.children;
      if (
        Array.isArray(current) &&
        current.length === kids.length &&
        current.every(
          (c, i) =>
            c.id === kids[i].id &&
            c.kind === kids[i].kind &&
            c.size === kids[i].size &&
            c.git === kids[i].git,
        )
      ) {
        return;
      }
      setNodes((prev) => {
        const update = (list: FsNode[]): FsNode[] =>
          list.map((n) =>
            n.id === dir
              ? { ...n, children: kids }
              : n.children && n.children.length > 0
                ? { ...n, children: update(n.children) }
                : n,
          );
        return update(prev);
      });
    },
    [pushToast],
  );

  // Auto-expand the root after the first load.
  useEffect(() => {
    if (!workdir || loaded.size > 0) return;
    treeRef.current?.open(workdir);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [workdir]);

  /** Lazy-load a directory's children on first expand. */
  const onToggle = useCallback(
    (id: string) => {
      const node = nodesById(nodesRef.current, id);
      if (node?.kind === 'dir' && node.children === null) void loadDir(id);
    },
    [loadDir],
  );

  /** Refresh every loaded directory in place (expansion state preserved).
   * Hidden browser tabs are skipped — the tick means nothing while unseen.
   * An outage is skipped too: requests would fail fast into silence, so
   * the tree just goes stale until the connection (and the next tick)
   * returns. */
  const refresh = useCallback(async () => {
    if (
      !workdir ||
      document.hidden ||
      useFlux.getState().connectionStatus !== 'connected'
    )
      return;
    const dirs = [...loaded];
    if (dirs.length === 0) return;
    setRefreshing(true);
    try {
      await Promise.all(dirs.map((dir) => loadDir(dir)));
    } finally {
      setRefreshing(false);
    }
  }, [workdir, loaded, loadDir]);

  // Auto-refresh tick at the fixed cadence — Explorer unmounts with the
  // Files tab, so the timer never runs while the tree is hidden.
  useEffect(() => {
    const t = setInterval(() => void refresh(), AUTO_REFRESH_MS);
    return () => clearInterval(t);
  }, [refresh]);

  const onActivate = useCallback(
    (node: NodeApi<FsNode>) => {
      if (node.data.kind === 'file') openFilePreview(node.data.id);
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );

  // Subscription BEFORE the no-workdir early return — hooks never sit
  // behind conditionals.
  const offline = useFlux((s) => s.connectionStatus !== 'connected');

  if (!workdir) {
    return (
      <div className="p-4 text-xs text-muted">
        Open a conversation to browse its working directory.
      </div>
    );
  }

  const rootFailed = failedDirs.has(workdir);

  return (
    <div id="explorer" className="flex min-h-0 flex-1 flex-col">
      {/* Toolbar: the tree root's name (the full workdir rides the chat
          header — no duplicate path) + manual refresh (auto-refresh runs
          on a fixed cadence). */}
      <div className="flex shrink-0 items-center gap-1 border-b border-border px-2 py-1">
        <span
          className="min-w-0 flex-1 truncate font-mono text-2xs text-faint"
          title={workdir}
        >
          {dirName(workdir)}
        </span>
        <IconButton
          label="Refresh files"
          title={`Refresh the file tree (auto-refreshes every ${AUTO_REFRESH_MS / 1000}s)`}
          onClick={() => void refresh()}
          disabled={refreshing}
          className="size-6"
        >
          <RotateCw size={13} className={cn(refreshing && 'animate-spin')} />
        </IconButton>
      </div>
      <div ref={wrapRef} className="min-h-0 flex-1 overflow-y-auto">
        <Tree
          ref={treeRef}
          data={nodes}
          openByDefault={false}
          disableDrag
          disableDrop
          disableEdit
          disableMultiSelection
          indent={14}
          // Touch: 36px rows clear the tap floor; desktop keeps the dense 24px.
          rowHeight={TOUCH_DEVICE ? 36 : 24}
          height={Math.max(height, 120)}
          width="100%"
          onToggle={onToggle}
          onActivate={onActivate}
          aria-label="Work directory files"
        >
          {NodeRenderer}
        </Tree>
        {rootFailed && (
          <div className="p-3 text-xs text-muted">
            {offline
              ? 'Server disconnected — the tree reloads when the connection is back.'
              : '(directory unavailable)'}
          </div>
        )}
      </div>
    </div>
  );
}

function nodesById(list: FsNode[], id: string): FsNode | null {
  for (const n of list) {
    if (n.id === id) return n;
    if (n.children) {
      const hit = nodesById(n.children, id);
      if (hit) return hit;
    }
  }
  return null;
}

/** Row renderer — the tree's visual voice (glyph, name, size, guides).
 * Git status colors the name and adds a letter badge (VS Code explorer
 * language: M modified, A added, U untracked, C conflict); directories
 * aggregate their subtree, so a changed folder marks every ancestor. */
function NodeRenderer(props: NodeRendererProps<FsNode>): React.ReactElement {
  const { node, style } = props;
  const d = node.data;
  // Rows are real components — subscribe to the preview path so the active
  // highlight follows the dock without a tree-data change.
  const previewing = useFlux((s) => s.activeDockTab === d.id);
  const git = d.git ? GIT_BADGE[d.git] : null;

  return (
    <div
      style={style}
      className={cn(
        'group flex h-full cursor-pointer items-center gap-1.5 rounded-sm px-1 pr-2',
        'text-xs transition-colors duration-fast hover:bg-hover',
        previewing && 'bg-active',
      )}
      title={git ? `${d.id} (${git.word})` : d.id}
      onClick={(e) => {
        e.stopPropagation();
        if (d.kind === 'dir') node.toggle();
        else node.activate();
      }}
    >
      <span
        className={cn(
          'inline-flex w-3 shrink-0 items-center justify-center text-faint transition-transform duration-fast',
          node.isOpen && 'rotate-90',
          d.kind === 'file' && 'invisible',
        )}
        aria-hidden="true"
      >
        <ChevronRight size={11} />
      </span>
      {d.kind === 'dir' ? (
        node.isOpen ? (
          <FolderOpen size={13} aria-hidden="true" className={cn('shrink-0', git ? git.className : 'text-muted')} />
        ) : (
          <FolderGlyph size={13} aria-hidden="true" className={cn('shrink-0', git ? git.className : 'text-muted')} />
        )
      ) : (
        <FileIcon name={d.name} />
      )}
      <span
        className={cn(
          'min-w-0 flex-1 truncate',
          git ? git.className : d.kind === 'file' && 'text-muted',
        )}
      >
        {d.name}
      </span>
      {git && (
        <span
          className={cn(
            'shrink-0 font-mono text-2xs leading-none tabular-nums',
            git.className,
          )}
          aria-label={`git: ${git.word}`}
        >
          {git.letter}
        </span>
      )}
      {d.size !== undefined && (
        <span className="shrink-0 text-2xs text-faint tabular-nums">{fmtBytes(d.size)}</span>
      )}
    </div>
  );
}
