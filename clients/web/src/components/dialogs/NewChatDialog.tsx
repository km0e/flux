/**
 * NewChatDialog.tsx — filesystem browser + conversation-kind
 * picker, one dialog (D-26: the web host is the only host).
 *
 * Navigate anywhere the server's user can read (`fs_list` over the WS),
 * preview directory contents and file heads (`fs_read`), pick the kind,
 * then create the chat in the browsed directory. No server-configured
 * project list — the workdir source is the filesystem itself.
 *
 * Provides: NewChatDialog
 * Depends: services/fs.ts, services/dialogs.ts (types), components/ui/*
 */
import { useEffect, useRef, useState } from 'react';
import {
  ArrowUp,
  ChevronRight,
  FolderIcon as FolderGlyph,
  HardDrive,
  House,
  X,
} from 'lucide-react';
import { Button, IconButton } from '../ui';
import { Dialog, DialogContent, DialogTitle } from '../ui/dialog';
import { listDir, readFile, isDisconnect } from '../../services/fs';
import { useFlux } from '../../core/state';
import { fmtBytes } from '../../lib/format';
import type { NewChatChoice } from '../../services/dialogs';
import type { ChatKind } from '../../services/dialogs';
import type { FsEntry } from '../../core/types';
import { cn } from '../../lib/cn';
import { FileIcon } from '../FileIcon';
import { ProviderPicker } from '../ProviderPicker';

/** Breadcrumb segments for a path — root gets an explicit "/" label. */
function crumbs(path: string): { label: string; path: string }[] {
  const parts = path.split('/').filter(Boolean);
  const segs = parts.map((label, i) => ({
    label,
    path: '/' + parts.slice(0, i + 1).join('/'),
  }));
  return [{ label: '/', path: '/' }, ...segs];
}

/** How many trailing segments stay visible when the path collapses. */
const CRUMB_TAIL = 3;

/** The visible breadcrumb sequence: deep paths collapse their middle into
 * an ellipsis (root + "…" + the last CRUMB_TAIL segments) so the row is
 * ALWAYS a single unwrapped line — no scrolling, the current directory
 * stays visible. Short paths render in full. */
function visibleCrumbs(path: string): { label: string; path: string | null }[] {
  const segs = crumbs(path);
  if (segs.length <= CRUMB_TAIL + 2) return segs;
  return [
    segs[0],
    { label: '…', path: null }, // collapsed middle — not navigable
    ...segs.slice(-CRUMB_TAIL),
  ];
}

const KIND_OPTIONS: { value: ChatKind; label: string; hint: string }[] = [
  { value: 'classic', label: 'Classic', hint: 'context keeps accumulating' },
  { value: 'feature', label: 'Feature', hint: 'context re-scaffolds per feature' },
];

/** Kind selector — a radiogroup with roving tabindex and arrow keys. */
function KindPicker(props: { value: ChatKind; onChange: (k: ChatKind) => void }): React.ReactElement {
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  const onKeyDown = (e: React.KeyboardEvent) => {
    const idx = KIND_OPTIONS.findIndex((o) => o.value === props.value);
    if (e.key === 'ArrowRight' || e.key === 'ArrowDown') {
      e.preventDefault();
      const next = KIND_OPTIONS[(idx + 1) % KIND_OPTIONS.length];
      props.onChange(next.value);
      refs.current[KIND_OPTIONS.indexOf(next)]?.focus();
    } else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') {
      e.preventDefault();
      const prev = KIND_OPTIONS[(idx - 1 + KIND_OPTIONS.length) % KIND_OPTIONS.length];
      props.onChange(prev.value);
      refs.current[KIND_OPTIONS.indexOf(prev)]?.focus();
    }
  };
  return (
    <div role="radiogroup" aria-label="Conversation type" onKeyDown={onKeyDown} className="flex flex-wrap gap-2">
      {KIND_OPTIONS.map((o, i) => (
        <button
          key={o.value}
          ref={(el) => {
            refs.current[i] = el;
          }}
          type="button"
          role="radio"
          aria-checked={props.value === o.value}
          tabIndex={props.value === o.value ? 0 : -1}
          className={cn(
            'flex cursor-pointer items-center gap-2 rounded-lg border px-3.5 py-2 text-sm transition-colors',
            'focus:outline-none focus-visible:ring-2 focus-visible:ring-accent',
            props.value === o.value
              ? 'border-accent bg-accent-dim text-fg'
              : 'border-border bg-inset text-muted hover:border-border-strong hover:text-fg',
          )}
          onClick={() => props.onChange(o.value)}
        >
          <span
            aria-hidden="true"
            className={cn(
              'size-2 rounded-full border',
              props.value === o.value ? 'border-accent bg-accent' : 'border-faint',
            )}
          />
          <span>
            {o.label}
            <em className="ml-1 text-2xs text-muted not-italic"> — {o.hint}</em>
          </span>
        </button>
      ))}
    </div>
  );
}

/** Inline message for a listing that failed because the server is down —
 * kept OUT of the error state so the reconnect self-heal can key on it. */
const OFFLINE_HINT = 'server disconnected';

export function NewChatDialog(props: {
  onCreate: (choice: NewChatChoice) => void;
  onCancel: () => void;
}): React.ReactElement {
  // Inheritance: the last opened chat (the active one; the most recent as
  // the fallback) seeds EVERY field but the name — kind, provider pin,
  // model, and the initially browsed workdir all start from it. The dialog
  // remounts per invocation, so the initializers re-read the store fresh
  // each time it opens.
  const chats = useFlux((s) => s.chats);
  const activeChatId = useFlux((s) => s.activeChatId);
  const providers = useFlux((s) => s.providers);
  const seed = chats.find((c) => c.id === activeChatId) ?? chats[0];

  const [path, setPath] = useState<string>(''); // browsed dir (canonical)
  const [parent, setParent] = useState<string | null>(null);
  const [entries, setEntries] = useState<FsEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [kind, setKind] = useState<ChatKind>(seed?.kind ?? 'classic');
  const [providerId, setProviderId] = useState(seed?.provider ?? '');
  const [model, setModel] = useState(seed?.model ?? '');
  const [preview, setPreview] = useState<{
    name: string;
    content: string;
    truncated: boolean;
    error?: string;
  } | null>(null);

  const connStatus = useFlux((s) => s.connectionStatus);
  const offlineError = error === OFFLINE_HINT;

  // A seeded pin whose provider has since been unregistered resets to the
  // prompt — the select cannot hold a value its options don't carry. Runs
  // when the registry reply lands (providers was empty at mount) and
  // whenever the seeded pin changes.
  useEffect(() => {
    if (providers.length > 0 && providerId && !providers.some((p) => p.id === providerId)) {
      setProviderId('');
      setModel('');
    }
  }, [providers, providerId]);

  const navigate = (p?: string) => {
    setLoading(true);
    setError(null);
    setPreview(null);
    listDir(p).then((r) => {
      if (r.error) {
        // The top bar owns outage communication — an offline browse fails
        // into the inline state below (and retries on reconnect) instead of
        // popping a context-free error toast.
        setError(isDisconnect(r.error) ? OFFLINE_HINT : r.error);
        if (!isDisconnect(r.error)) {
          useFlux
            .getState()
            .pushToast('error', `Could not list ${r.requested || 'the directory'} — ${r.error}`);
        }
      } else {
        setPath(r.path ?? r.requested);
        setParent(r.parent ?? null);
        setEntries(r.entries);
      }
      setLoading(false);
    });
  };

  // Initial listing — the seed chat's workdir when inheriting (a new chat
  // usually serves the same project), otherwise the server's default start
  // dir ($HOME). Mount-only: navigate() is stable enough for the picker's
  // lifetime (one dialog at a time; re-invocations go through click handlers).
  useEffect(() => {
    navigate(seed?.workdir || undefined);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Self-heal: a listing that failed offline retries once the connection is
  // back, keeping the browsed path ('' = the server's home listing).
  useEffect(() => {
    if (connStatus === 'connected' && offlineError) navigate(path || undefined);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connStatus]);

  const openFile = (entry: FsEntry) => {
    const full = (path === '/' ? '' : path) + '/' + entry.name;
    setPreview({ name: entry.name, content: '', truncated: false });
    readFile(full).then((r) => {
      if (r.error && !isDisconnect(r.error)) {
        useFlux.getState().pushToast('error', `Could not read ${entry.name} — ${r.error}`);
      }
      setPreview({
        name: entry.name,
        content: r.content ?? '',
        truncated: r.truncated ?? false,
        error: r.error,
      });
    });
  };

  const openDir = (name: string | null) => {
    // null = parent (".."); a name joins below the current path.
    const target = name === null ? (parent ?? '/') : (path === '/' ? '' : path) + '/' + name;
    navigate(target);
  };

  return (
    <Dialog open onOpenChange={(open) => !open && props.onCancel()}>
      {/* Flex column + overflow-y-hidden: the dialog NEVER scrolls as a
          whole — the file list below is the only scrollable region (it
          flexes to the remaining 88vh budget; a directory listing is
          effectively unbounded, so the list absorbs every shortfall). */}
      {/* Fixed height: the listing's entry count varies wildly (empty dir →
          a packed $HOME), and a content-sized dialog would visibly jump as
          listings load. The list area flexes inside the stable frame and
          scrolls; the preview pane squeezes it instead of growing the box. */}
      <DialogContent className="flex h-[min(88dvh,600px)] w-[min(94vw,960px)] flex-col overflow-y-hidden">
        <DialogTitle>New chat</DialogTitle>
        <div className="flex min-h-0 flex-1 flex-col gap-2.5">
          <KindPicker value={kind} onChange={setKind} />

          <ProviderPicker
            providerId={providerId}
            model={model}
            onChange={(next) => {
              setProviderId(next.provider);
              setModel(next.model);
            }}
          />

          <div
            className="flex min-w-0 items-center gap-0.5 whitespace-nowrap rounded-md border border-border bg-inset px-2.5 py-1.5 font-mono text-xs"
            role="navigation"
            aria-label="Path"
          >
            {visibleCrumbs(path || '/').map((c, i) => (
              <span key={`${c.path ?? '…'}-${i}`} className="flex min-w-0 items-center gap-0.5">
                {i > 0 && <span className="shrink-0 text-faint">›</span>}
                {c.path === null ? (
                  <span className="shrink-0 px-1 text-faint" title="collapsed path segments">
                    …
                  </span>
                ) : (
                  <button
                    type="button"
                    className="min-w-0 cursor-pointer truncate rounded-sm px-1 py-px text-info hover:bg-hover"
                    onClick={() => navigate(c.path as string)}
                    title={c.path}
                  >
                    {c.label}
                  </button>
                )}
              </span>
            ))}
          </div>

          <div className="flex gap-1.5">
            <Button variant="secondary" size="sm" onClick={() => navigate()} title="Server start directory">
              <House size={13} aria-hidden="true" />
              Home
            </Button>
            <Button variant="secondary" size="sm" onClick={() => navigate('/')}>
              <HardDrive size={13} aria-hidden="true" />
              Root
            </Button>
            {parent !== null && (
              <Button variant="secondary" size="sm" onClick={() => openDir(null)}>
                <ArrowUp size={13} aria-hidden="true" />
                Up
              </Button>
            )}
          </div>

          <div
            className="flex min-h-0 flex-1 flex-col overflow-y-auto rounded-md border border-border bg-bg py-1"
            role="listbox"
            aria-label="Directory contents"
          >
            {loading && <div className="p-3 text-sm text-muted">Loading…</div>}
            {!loading && error && (
              <div className="p-3 text-sm break-all text-muted">
                {offlineError
                  ? 'Server disconnected — the listing retries when the connection is back.'
                  : '(directory unavailable)'}
              </div>
            )}
            {!loading && !error && entries.length === 0 && (
              <div className="p-3 text-sm text-muted">Empty directory</div>
            )}
            {!loading &&
              !error &&
              entries.map((e) => {
                const dir = e.kind === 'dir';
                return (
                  <button
                    key={e.name}
                    type="button"
                    role="option"
                    aria-selected="false"
                    className={cn(
                      'flex h-8 w-full cursor-pointer items-center gap-1.5 rounded-sm px-2.5 pr-2.5 text-left text-sm',
                      'transition-colors duration-75 hover:bg-hover',
                      dir ? 'text-fg' : 'text-muted hover:text-fg',
                    )}
                    onClick={() => (dir ? openDir(e.name) : openFile(e))}
                    title={dir ? e.name : `${e.name} — preview`}
                  >
                    <span
                      aria-hidden="true"
                      className="inline-flex w-3 shrink-0 items-center justify-center text-faint"
                    >
                      {dir && <ChevronRight size={11} />}
                    </span>
                    {dir ? (
                      <FolderGlyph size={13} aria-hidden="true" className="shrink-0 text-muted" />
                    ) : (
                      <FileIcon name={e.name} />
                    )}
                    <span className="min-w-0 flex-1 truncate">{e.name}</span>
                    {!dir && e.size !== undefined && (
                      <span className="shrink-0 text-2xs text-faint tabular-nums">
                        {fmtBytes(e.size)}
                      </span>
                    )}
                  </button>
                );
              })}
          </div>

          {preview && (
            <div className="overflow-hidden rounded-md border border-border">
              <div className="flex items-center gap-2 bg-panel px-3 py-1.5">
                <span className="min-w-0 flex-1 truncate text-sm font-semibold">{preview.name}</span>
                {preview.truncated && <span className="text-2xs text-warn">truncated</span>}
                <IconButton label="Close preview" onClick={() => setPreview(null)}>
                  <X size={13} aria-hidden="true" />
                </IconButton>
              </div>
              {preview.error ? (
                <div className="p-3 text-sm break-all text-muted">(file could not be read)</div>
              ) : (
                <pre className="max-h-[32vh] overflow-auto bg-inset p-3 font-mono text-xs leading-relaxed">
                  {preview.content || '(empty file)'}
                </pre>
              )}
            </div>
          )}

          <div className="mt-1 flex items-center justify-between gap-3">
            <span className="min-w-0 flex-1 truncate font-mono text-xs text-faint" title={path}>
              {path || '…'}
            </span>
            <div className="flex shrink-0 gap-2">
              <Button variant="secondary" onClick={props.onCancel}>
                Cancel
              </Button>
              <Button
                variant="primary"
                disabled={!path || !providerId || !model.trim() || loading || !!error}
                onClick={() =>
                  path &&
                  providerId &&
                  model.trim() &&
                  props.onCreate({
                    kind,
                    workdir: path,
                    provider: providerId,
                    model: model.trim(),
                  })
                }
              >
                Create here
              </Button>
            </div>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
