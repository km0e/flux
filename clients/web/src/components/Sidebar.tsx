/**
 * Sidebar.tsx — left panel: conversations list (+ file explorer tab).
 *
 * Radix primitives own the behavior (DropdownMenu for row actions, Tabs for
 * panels); a selection = a lease handover driven by the activeChatId
 * subscription in mount. A client-side filter (name/workdir substring)
 * narrows the list.
 *
 * Provides: Sidebar
 * Depends: core/state.ts, core/bridge.ts, lib/cn.ts, lib/format.ts,
 *          services/*, components/ui/*, components/Explorer.tsx
 */
import { lazy, Suspense, useEffect, useRef, useState } from 'react';
import { MoreHorizontal, Pencil, Plus, Search, Trash2 } from 'lucide-react';
import { useFlux } from '../core/state';
import { bridge } from '../core/bridge';
import { cn } from '../lib/cn';
import { relativeTime } from '../lib/format';
import { log } from '../logger';
import { clearChatPane } from '../services/panes';
import { deleteDraft } from '../services/drafts';
import { dialogs } from '../services/dialogs';
import { startNewChatFlow } from '../services/new-chat';
import { selectChat } from '../services/commands';
// The Files tree (react-arborist + react-window) is a secondary surface —
// loaded on first Files-tab activation instead of the initial bundle.
const Explorer = lazy(() => import('./Explorer'));
import { Button, Badge, TextField, IconButton, Spinner } from './ui';
import { Tabs, TabsContent, TabsList, TabsTrigger } from './ui/tabs';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from './ui/dropdown-menu';
import type { Chat } from '../core/state';

/** The chat row's ⋯ menu (Rename/Delete) — a feature composite over the
 * generic dropdown primitives (ui/dropdown-menu owns only styling); hover
 * reveals on pointer devices, always visible on touch.
 *
 * The row itself is an activation surface for OPENING the chat (click +
 * Enter/Space on its role="button"), so the menu's own activators must be
 * contained here: a tap on ⋯ bubbles a click (touch always synthesizes
 * one, even though Radix opens on pointerdown), and Enter/Space on the
 * focused trigger bubble too — either leaking into the row selects the
 * chat, and on the mobile drawer selection CLOSES the drawer, yanking the
 * list away mid-action (rename/delete became unreachable). Radix ignores
 * click and handles Enter/Space/ArrowDown itself, so stopping just those
 * at this wrapper costs nothing. */
function ChatRowMenu(props: {
  label: string;
  onRename: () => void;
  onDelete: () => void;
}): React.ReactElement {
  const stopRowActivation = (e: React.SyntheticEvent) => e.stopPropagation();
  return (
    <span
      className="flex shrink-0 items-center"
      onClick={stopRowActivation}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') stopRowActivation(e);
      }}
    >
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <IconButton
            label={props.label}
            className={cn(
              'size-6 max-md:size-9 rounded-md',
              'opacity-0 touch:opacity-100 group-hover/row:opacity-100 focus-visible:opacity-100',
              'data-[state=open]:bg-hover data-[state=open]:text-fg data-[state=open]:opacity-100',
            )}
          >
            <MoreHorizontal size={14} />
          </IconButton>
        </DropdownMenuTrigger>
        <DropdownMenuContent>
          <DropdownMenuItem onSelect={props.onRename}>
            <Pencil size={12} aria-hidden="true" className="text-muted" />
            Rename
          </DropdownMenuItem>
          <DropdownMenuItem danger onSelect={props.onDelete}>
            <Trash2 size={12} aria-hidden="true" />
            Delete
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </span>
  );
}

/** One conversation row: name + time + workdir line + hover ⋯ menu. */
function ChatRow(props: {
  chat: Chat;
  activeId: string;
  /** Suppress the In-use badge: this row is one side of an in-flight lease
   * switch, so its wire `active` flag is stale (the flip lands with the
   * next chats broadcast). Rendering it would flash the badge for a frame. */
  suppressBadge: boolean;
  renaming: boolean;
  onStartEditing: (id: string) => void;
  onCommitRename: (id: string, raw: string) => void;
  onSelect: (id: string) => void;
  onRowKeyDown: (e: React.KeyboardEvent, id: string) => void;
  onDelete: (id: string) => void;
}): React.ReactElement {
  const { chat: c, activeId } = props;
  const [draft, setDraft] = useState(c.name);
  const inputRef = useRef<HTMLInputElement>(null);

  // Entering rename mode focuses + selects the input.
  useEffect(() => {
    if (props.renaming) {
      setDraft(c.name);
      inputRef.current?.focus();
      inputRef.current?.select();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.renaming]);

  const commit = () => props.onCommitRename(c.id, draft.trim()); // '' = cancel

  const active = c.id === activeId;
  // Running = the server's round-boundary truth (any window) OR this
  // window's own round-level flag — the local flag is instant (set at
  // send), the broadcast truth lags it by at most a frame.
  const localStreaming = useFlux((s) => s.streaming[c.id] ?? false);
  const running = (c.running ?? false) || localStreaming;
  return (
    // The wrapper owns the hover group + the menu's positioning context —
    // the menu is a SIBLING of the role=button row, not a child: a button
    // inside a button is a nested-interactive violation (axe serious), and
    // the overlay costs nothing (the menu never needed the row's flex).
    <div className="group/row relative">
      <div
        role="button"
        tabIndex={0}
        aria-current={active ? 'true' : undefined}
        aria-label={`Open chat ${c.name || 'New Chat'}${running ? ', running' : ''}`}
        className={cn(
          // The 2px left rail is the selection marker: every row carries it
          // (transparent when inactive) so the active state never shifts layout.
          // pr-8 reserves the corner where the (sibling) actions menu overlays.
          'flex cursor-pointer items-center gap-1 border-l-2 pl-3 pr-8 py-2',
          'transition-colors duration-fast focus:outline-none',
          active
            ? 'border-l-accent bg-active'
            : cn('border-l-transparent', 'hover:bg-hover'),
        )}
        onClick={() => props.onSelect(c.id)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            props.onSelect(c.id);
          } else {
            props.onRowKeyDown(e, c.id);
          }
        }}
      >
      <div className="min-w-0 flex-1">
        {props.renaming ? (
          /* Edit mode: swallow click/keys so the row's select and menu
             handlers never see them; Enter/blur commit and Esc reverts. */
          <div
            className="w-full"
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === 'Escape') {
                e.preventDefault();
                props.onCommitRename(c.id, ''); // cancel
              }
            }}
          >
            <input
              ref={inputRef}
              className="h-7 w-full rounded-sm border border-accent bg-inset px-1.5 text-sm text-fg focus:outline-none"
              aria-label="Chat name"
              maxLength={80}
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onBlur={commit}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  commit();
                }
              }}
            />
          </div>
        ) : (
          <>
            <div className="flex items-center gap-1.5">
              {/* The sidebar's running marker: a round is in flight on this
                  chat — visible for chats running in OTHER windows too (the
                  server's chats broadcast carries the flag), not just for
                  streams this window observes. */}
              {running && <Spinner className="size-2.5 shrink-0 text-accent" />}
              <span className="truncate text-sm">{c.name || 'New Chat'}</span>
              {/* wire active = lease held by ANY window (incl. this one) —
                  only flag it when the holder is NOT the selected chat, and
                  never while the row's flag is stale (in-flight switch). */}
              {c.active && c.id !== activeId && !props.suppressBadge && (
                <Badge tone="warn">In use</Badge>
              )}
            </div>
            {/* cwd line (the sandbox boundary) + the chat's last activity —
                both machine facts, both mono. Activity (not creation) is
                what a conversation list means by "5m ago". */}
            <div className="mt-0.5 flex items-center gap-2">
              {c.workdir && (
                <span className="min-w-0 flex-1 truncate font-mono text-2xs text-faint" title={c.workdir}>
                  {c.workdir}
                </span>
              )}
              {!c.workdir && <span className="flex-1" />}
              <span className="shrink-0 font-mono text-2xs text-faint tabular-nums">
                {relativeTime(c.lastActivityAt ?? c.createdAt)}
              </span>
            </div>
          </>
        )}
      </div>
      </div>
      {!props.renaming && (
        <div className="absolute top-1/2 right-1.5 -translate-y-1/2">
          <ChatRowMenu
            label={`Chat actions for ${c.name || 'chat'}`}
            onRename={() => props.onStartEditing(c.id)}
            onDelete={() => props.onDelete(c.id)}
          />
        </div>
      )}
    </div>
  );
}


export function Sidebar(): React.ReactElement {
  const chats = useFlux((s) => s.chats);
  const activeId = useFlux((s) => s.activeChatId);
  const leaseSwitch = useFlux((s) => s.leaseSwitch);
  // The Files tab remounts per workdir (the tree roots at the chat's cwd).
  const activeWorkdir = chats.find((c) => c.id === activeId)?.workdir ?? 'none';
  const [tab, setTab] = useState<'chats' | 'files'>('chats');
  const [filter, setFilter] = useState('');
  // The id being renamed — plain state: the row callbacks are fresh
  // closures each render (no stale-capture machine, unlike the old impl).
  const [renamingId, setRenamingId] = useState<string | null>(null);

  // Relative-time freshness: an IDLE chat re-renders nothing, so its row's
  // "now" would sit frozen for hours. A minute tick re-renders the list —
  // skipped while the page is hidden (matches the Explorer's discipline)
  // and cheap otherwise (the rows are small; zustand re-renders only on
  // state change, and the tick is the only thing that moves).
  const [, bumpClock] = useState(0);
  useEffect(() => {
    const t = setInterval(() => {
      if (!document.hidden) bumpClock((n) => n + 1);
    }, 30_000);
    return () => clearInterval(t);
  }, []);

  const startEditing = (id: string) => {
    setRenamingId(id);
  };

  /** Enter/blur commit. '' = the Esc cancel path (revert, never send). */
  const commitRename = (id: string, raw: string) => {
    setRenamingId(null);
    if (raw === '') return;
    const chat = useFlux.getState().chats.find((c) => c.id === id);
    if (!chat || raw === chat.name) return;
    log.info('sidebar: rename (inline) ' + id);
    bridge.send({ type: 'chat_rename', chat_id: id, name: raw });
  };

  const onNewChat = () => startNewChatFlow();

  const onSelect = (id: string) => {
    log.info('sidebar: select ' + id);
    // The shared switch path (services/commands.ts) — the command
    // palette's chat entries ride the exact same flow.
    selectChat(id);
    // No standalone chat_open — an activeChatId change triggers
    // switchLease in mount → chat_claim, the single message carrying
    // history snapshot + subscription + lease. When occupied,
    // error{chat_busy} degrades to a read-only pane (handlers.ts sends the
    // follow-up chat_open).
  };

  /** Roving keyboard navigation on the chat list: ↑/↓ move row focus,
   * Enter/Space open (existing), Delete deletes. Rows are siblings, so
   * sibling focus is a property jump — no index bookkeeping. */
  const onRowKeyDown = (e: React.KeyboardEvent, id: string) => {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const row = e.currentTarget as HTMLElement;
      const next = e.key === 'ArrowDown' ? row.nextElementSibling : row.previousElementSibling;
      (next as HTMLElement | null)?.focus();
    } else if (e.key === 'Delete') {
      e.preventDefault();
      onDelete(id);
    }
  };

  const onDelete = (id: string) => {
    log.info('sidebar: delete ' + id);
    const name = useFlux.getState().chats.find((c) => c.id === id)?.name ?? id;
    void dialogs.confirmDelete(name).then((confirmed) => {
      if (!confirmed) return;
      // The server already aborted the task and broadcast the list; converge the local UI immediately.
      clearChatPane(id);
      deleteDraft(id); // ids never recycle — the draft is dead weight
      useFlux.getState().deleteChat(id);
      bridge.send({ type: 'chat_delete', chat_id: id });
    });
  };

  const q = filter.trim().toLowerCase();
  const visible = q
    ? chats.filter((c) => c.name.toLowerCase().includes(q) || c.workdir.toLowerCase().includes(q))
    : chats;

  return (
    <Tabs
      id="sidebar"
      value={tab}
      onValueChange={(v) => setTab((v as 'chats' | 'files') ?? 'chats')}
      className="flex min-h-0 flex-1 flex-col"
    >
      {/* The panel switcher IS the header — terminal creation lives in
          the dock (its surface): the tab strip's "+" and the empty
          state's action; the dock itself opens from ChatHeader. */}
      <div className="px-2 pt-2">
        <TabsList aria-label="Sidebar panels">
          <TabsTrigger value="chats">Chats</TabsTrigger>
          <TabsTrigger value="files">Files</TabsTrigger>
        </TabsList>
      </div>
      <TabsContent value="files" className="flex min-h-0 flex-1 flex-col">
        <Suspense fallback={<div className="p-4 text-xs text-muted">Loading files…</div>}>
          <Explorer key={activeWorkdir} />
        </Suspense>
      </TabsContent>
      <TabsContent value="chats" className="flex min-h-0 flex-1 flex-col">
        <div className="flex flex-col gap-2 border-b border-border p-2">
          <Button id="new-chat-btn" variant="primary" className="w-full max-md:h-10" onClick={onNewChat}>
            <Plus size={13} aria-hidden="true" />
            New chat
          </Button>
          <div className="relative">
            <Search
              size={12}
              aria-hidden="true"
              className="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-faint"
            />
            <TextField
              type="search"
              placeholder="Filter chats…"
              aria-label="Filter chats"
              className="pl-7"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
          </div>
        </div>
        <div id="conversation-list" className="min-h-0 flex-1 overflow-y-auto">
          {chats.length === 0 ? (
            <div className="p-4 text-xs text-muted">No conversations yet.</div>
          ) : visible.length === 0 ? (
            <div className="p-4 text-xs text-muted">No chats match “{filter.trim()}”.</div>
          ) : (
            visible.map((c) => (
              <ChatRow
                key={c.id}
                chat={c}
                activeId={activeId}
                suppressBadge={
                  leaseSwitch !== null && (leaseSwitch.from === c.id || leaseSwitch.to === c.id)
                }
                renaming={renamingId === c.id}
                onStartEditing={startEditing}
                onCommitRename={commitRename}
                onSelect={onSelect}
                onRowKeyDown={onRowKeyDown}
                onDelete={onDelete}
              />
            ))
          )}
        </div>
      </TabsContent>
    </Tabs>
  );
}

