/**
 * ChatHeader.tsx — the conversation's header row, above the message
 * column: the chat identity (name, kind, workdir), the dock toggle, and
 * the per-chat token usage. The global TopBar carries only app-wide
 * state; THIS row owns everything that belongs to the open conversation,
 * so the canvas has an anchor and the workdir (the sandbox boundary)
 * reads where the eye already is.
 *
 * The dock toggle is the dock's DIRECT affordance: opening it is not
 * gated on first opening a file or spawning a terminal (the dock's empty
 * state offers those actions itself).
 *
 * Provides: ChatHeader
 * Depends: core/state.ts, components/ui (Badge/IconButton), components/ui/tooltip,
 *          components/UsageStats
 */
import { useFlux } from '../core/state';
import { Badge, IconButton } from './ui';
import { Tooltip } from './ui/tooltip';
import { PanelRight } from 'lucide-react';
import { cn } from '../lib/cn';
import { UsageStats } from './UsageStats';

export function ChatHeader(): React.ReactElement | null {
  const cid = useFlux((s) => s.activeChatId);
  const active = useFlux((s) => s.chats.find((c) => c.id === s.activeChatId));
  const usage = useFlux((s) => (cid ? s.usage[cid] : undefined));
  const dockOpen = useFlux((s) => s.dockOpen);
  if (!cid || !active) return null;

  return (
    <div
      id="chat-header"
      aria-label="Conversation"
      className="flex h-11 shrink-0 items-center gap-2 border-b border-border bg-panel px-4"
    >
      <span
        className="min-w-0 max-w-72 truncate text-md font-semibold tracking-[-0.01em]"
        title={active.name}
      >
        {active.name || 'New Chat'}
      </span>
      {active.kind === 'feature' && (
        <Badge tone="accent" title="Feature mode — per-feature context">
          Feature
        </Badge>
      )}
      {active.workdir && (
        <span
          className="hidden min-w-0 flex-1 truncate font-mono text-2xs text-faint md:block"
          title={active.workdir}
        >
          {active.workdir}
        </span>
      )}
      <span className="flex-1 md:hidden" />
      {/* The dock's direct toggle — files and terminals are secondary
          surfaces; their home must be reachable without creating one. */}
      <Tooltip content="Toggle the file & terminal dock (Ctrl/Cmd+J)" side="bottom">
        <IconButton
          id="dock-toggle"
          label="Toggle dock"
          aria-pressed={dockOpen}
          onClick={() => useFlux.getState().setDockOpen(!dockOpen)}
          className={cn('size-7 max-md:size-9', dockOpen && 'bg-active text-accent')}
        >
          <PanelRight size={15} aria-hidden="true" />
        </IconButton>
      </Tooltip>
      <UsageStats usage={usage} provider={active.provider} model={active.model} />
    </div>
  );
}
