/**
 * RightDock.tsx — the right-edge dock: a TAB STRIP over file previews and
 * terminal sessions.
 *
 * Tabs: one per opened file (multi-file, editor-style) + one per terminal
 * session (created by the strip's "+" or the empty state's New-terminal
 * action — never auto-spawned; pinned after the file tabs). The file actions (Copy /
 * Raw / truncated badge) FLOAT in the strip's right side — the content
 * pane below is chrome-free (no duplicated file name row). The dock is a
 * flex sibling of the chat column inside the body row: widening it PUSHES
 * the conversation left — it never covers content. Narrow viewports flip
 * it to a full-height overlay via the #right-dock media query in app.css.
 *
 * The dock is drag-resizable from its left edge (window pointer listeners
 * + body class + persist on release); file bodies are always horizontally
 * scrolling (wrap support removed).
 *
 * Provides: RightDock
 * Depends: core/state.ts, core/prefs.ts, services/terminal.ts,
 *          components/FileTabView.tsx, components/TerminalPanel.tsx,
 *          components/FileIcon.tsx
 */
import { useEffect, useState } from 'react';
import { Plus, SquareTerminal, X } from 'lucide-react';
import { useFlux } from '../core/state';
import { storePreviewWidth, PREVIEW_MAX_WIDTH, PREVIEW_MIN_WIDTH } from '../core/prefs';
import { createTerminal, killTerminal, terminalSession } from '../services/terminal';
import { useEdgeResize } from '../hooks/useEdgeResize';
import { Badge, Button, IconButton } from './ui';
import { cn } from '../lib/cn';
import { copyText } from '../lib/clipboard';
import { FileIcon } from './FileIcon';
import { FileTabView } from './FileTabView';
import { TerminalPanel } from './TerminalPanel';

export function RightDock(): React.ReactElement | null {
  const open = useFlux((s) => s.dockOpen);
  const openFiles = useFlux((s) => s.openFiles);
  const terminalTabs = useFlux((s) => s.terminalTabs);
  const activeTab = useFlux((s) => s.activeDockTab);
  const activeChatId = useFlux((s) => s.activeChatId);
  if (!open) return null;

  const activeFile = openFiles.find((t) => t.id === activeTab);
  const closeDock = () => useFlux.getState().setDockOpen(false);
  const addTerminal = () => void createTerminal(activeChatId);

  // Left-edge drag: the shared contract (CSS-var direct write during the
  // drag, one store commit + persist on release, body class kills text
  // selection). The width tracks the pointer's distance from the
  // viewport's right edge (the dock is flush right in both regimes) and
  // always leaves ≥280px for the conversation.
  const startResize = useEdgeResize({
    cssVar: '--fx-preview-w',
    dragClass: 'resizing-preview',
    widthAt: (e) => window.innerWidth - e.clientX,
    clamp: (x) => {
      const max = Math.max(PREVIEW_MIN_WIDTH, Math.min(PREVIEW_MAX_WIDTH, window.innerWidth - 280));
      return Math.min(max, Math.max(PREVIEW_MIN_WIDTH, x));
    },
    commit: (next) => {
      useFlux.setState({ previewWidth: next });
      storePreviewWidth(next);
    },
  });

  // Keyboard resize on the separator (±24px per press, persisted at once).
  const resizeKey = (e: React.KeyboardEvent) => {
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
    e.preventDefault();
    const next = Math.min(
      PREVIEW_MAX_WIDTH,
      Math.max(PREVIEW_MIN_WIDTH, useFlux.getState().previewWidth + (e.key === 'ArrowLeft' ? 24 : -24)),
    );
    useFlux.setState({ previewWidth: next });
    storePreviewWidth(next);
  };

  // The file actions float in the strip (right side) — the content pane
  // stays chrome-free, so no file-name duplication with the tab.
  const isMarkdown = !!activeFile && /\.(md|markdown)$/i.test(activeFile.name);

  return (
    <div
      id="right-dock"
      role="complementary"
      aria-label="File and terminal dock"
      className="relative flex h-full shrink-0 flex-col border-l border-border bg-bg max-w-[calc(100vw-280px)]"
    >
      <div
        id="dock-resizer"
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize dock"
        tabIndex={0}
        onPointerDown={startResize}
        onKeyDown={resizeKey}
        className="absolute inset-y-0 -left-1 z-[1] w-2 cursor-col-resize transition-colors hover:bg-accent/30"
      />

      {/* Tab strip: file tabs · terminal tabs · "+" · floating file actions ·
          the dock close. */}
      <div className="flex items-center gap-1 border-b border-border bg-panel px-1.5 py-1.5">
        <div className="flex min-w-0 flex-1 items-stretch gap-0.5 overflow-x-auto rounded-md bg-inset p-0.5">
          {openFiles.map((t) => (
            <DockTab
              key={t.id}
              label={t.name}
              title={t.path}
              active={activeTab === t.id}
              icon={<FileIcon name={t.name} />}
              onSelect={() => useFlux.setState({ activeDockTab: t.id, dockOpen: true })}
              onClose={() => useFlux.getState().closeFileTab(t.id)}
              closeLabel={`Close ${t.name}`}
            />
          ))}
          {terminalTabs.map((t) => {
            const session = terminalSession(t.id);
            return (
              <DockTab
                key={t.id}
                label={`Terminal ${t.seq}`}
                title={`Interactive shell (chat workdir)${session?.status === 'exited' ? ' — exited' : ''}`}
                active={activeTab === t.id}
                icon={
                  <SquareTerminal
                    size={13}
                    aria-hidden="true"
                    className={cn(session?.status === 'running' ? 'text-success' : 'text-muted')}
                  />
                }
                onSelect={() => useFlux.setState({ activeDockTab: t.id, dockOpen: true })}
                onClose={() => killTerminal(t.id)}
                closeLabel={`Close terminal ${t.seq}`}
              />
            );
          })}
          <button
            type="button"
            aria-label="New terminal"
            title={
              activeChatId
                ? 'Open a new terminal (runs in the active chat\'s workdir)'
                : 'Open a chat first — terminals run in the chat\'s workdir'
            }
            disabled={!activeChatId}
            onClick={addTerminal}
            className={cn(
              'grid size-7 max-md:size-9 shrink-0 cursor-pointer place-items-center rounded-md text-muted',
              'transition-colors duration-fast hover:bg-hover hover:text-fg disabled:opacity-40',
            )}
          >
            <Plus size={14} aria-hidden="true" />
          </button>
        </div>

        {/* Floating file actions (active file tab only) — no duplicated
            name row in the content pane. */}
        {activeFile && (
          <div className="flex shrink-0 items-center gap-1 pl-1.5">
            {activeFile.truncated && (
              <Badge tone="warn" title="Only the first part of the file is shown (256KB budget)">
                truncated
              </Badge>
            )}
            {isMarkdown && !activeFile.error && !activeFile.loading && activeFile.content && (
              <StripButton
                pressed={activeFile.rawView}
                title="Toggle between rendered markdown and the raw source"
                onClick={() =>
                  useFlux.getState().patchFileTab(activeFile.id, { rawView: !activeFile.rawView })
                }
              >
                {activeFile.rawView ? 'Rendered' : 'Raw'}
              </StripButton>
            )}
            {activeFile.content && (
              <CopyStripButton text={activeFile.content} />
            )}
          </div>
        )}
        <IconButton label="Close dock" className="ml-1 self-center" onClick={closeDock}>
          <X size={13} aria-hidden="true" />
        </IconButton>
      </div>

      {/* Content: the active file tab, or the terminal session. */}
      {activeTab && terminalTabs.some((t) => t.id === activeTab) ? (
        <TerminalPanel tabId={activeTab} />
      ) : activeTab && openFiles.some((t) => t.id === activeTab) ? (
        <FileTabView tabId={activeTab} />
      ) : (
        /* Empty dock — an invitation to act, with the primary action
           inline (the toggle makes the dock reachable without content;
           the empty state must not dead-end). */
        <div className="grid flex-1 place-items-center px-4 pb-16 text-center">
          <div className="flex max-w-72 flex-col items-center gap-2.5">
            <span className="text-sm text-faint">
              {activeChatId
                ? 'Nothing open — pick a file in the Explorer, or start a terminal.'
                : 'Open a conversation — the dock browses its files and runs its terminals.'}
            </span>
            {activeChatId && (
              <Button variant="secondary" size="sm" onClick={addTerminal}>
                <Plus size={13} aria-hidden="true" />
                New terminal
              </Button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

/** Strip-level action button (Copy / Raw) — ghost-small, pressed state. */
function StripButton(props: {
  children: React.ReactNode;
  title: string;
  pressed?: boolean;
  onClick: () => void;
}): React.ReactElement {
  return (
    <button
      type="button"
      title={props.title}
      aria-pressed={props.pressed}
      onClick={props.onClick}
      className={cn(
        'h-[26px] cursor-pointer rounded-md px-2 text-2xs font-medium whitespace-nowrap',
        'transition-colors duration-fast',
        props.pressed ? 'bg-hover text-fg' : 'text-muted hover:bg-hover hover:text-fg',
      )}
    >
      {props.children}
    </button>
  );
}

/** The file tab's Copy action with feedback — the message copy button says
 * "Copied!", so a silent strip button reads as broken. The label flips for
 * a moment on success (a failed copy stays silent; the clipboard failure
 * is a permission story the user knows). */
function CopyStripButton(props: { text: string }): React.ReactElement {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const t = setTimeout(() => setCopied(false), 1400);
    return () => clearTimeout(t);
  }, [copied]);
  return (
    <StripButton
      title="Copy file contents"
      pressed={copied}
      onClick={() => {
        void copyText(props.text).then((ok) => {
          if (ok) setCopied(true);
        });
      }}
    >
      {copied ? 'Copied' : 'Copy'}
    </StripButton>
  );
}

/** One strip tab: select on click, independent close button. */
function DockTab(props: {
  label: string;
  title: string;
  active: boolean;
  icon: React.ReactNode;
  onSelect: () => void;
  onClose: () => void;
  closeLabel: string;
}): React.ReactElement {
  return (
    <div
      className={cn(
        'group flex min-w-0 max-w-44 shrink-0 cursor-pointer items-center gap-1.5 rounded-sm px-2 py-1 max-md:py-1.5 max-md:text-sm text-xs',
        'transition-colors duration-fast',
        props.active
          ? 'bg-elev text-fg shadow-sm'
          : 'bg-transparent text-muted hover:bg-hover hover:text-fg',
      )}
      role="tab"
      aria-selected={props.active}
      tabIndex={0}
      title={props.title}
      onClick={props.onSelect}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          props.onSelect();
        }
      }}
    >
      <span className="flex shrink-0 items-center">{props.icon}</span>
      <span className="min-w-0 truncate">{props.label}</span>
      <button
        type="button"
        aria-label={props.closeLabel}
        title={props.closeLabel}
        className="ml-0.5 hidden size-4 shrink-0 cursor-pointer place-items-center rounded-sm text-faint hover:bg-hover hover:text-fg group-hover:grid touch:grid touch:size-9"
        onClick={(e) => {
          e.stopPropagation();
          props.onClose();
        }}
      >
        <X size={10} aria-hidden="true" />
      </button>
    </div>
  );
}
