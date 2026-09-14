/**
 * RoundPanel.tsx — the dock's Round tab: the current round's artifacts
 * (F-11).
 *
 * Files the round created/modified and tool invocations it made, since
 * the chat's last user message — the retrospection surface ("what did
 * this round just do?"). File rows open a preview tab (the same address
 * space as the Explorer: the path); invocation rows pulse their tool
 * card in the message stream. The data folds from the stream's
 * tool_start events (services/artifacts.ts) and rebuilds from the
 * history snapshot on re-open — the list is a convenience view over the
 * transcript, never a truth source.
 *
 * Provides: RoundPanel
 * Depends: core/state.ts, services/fs.ts, services/artifacts.ts,
 *          components/FileIcon.tsx, components/ui/*
 */
import { SquareTerminal, Puzzle } from 'lucide-react';
import { useFlux } from '../core/state';
import { openFilePreview } from '../services/filePreview';
import { jumpToToolCall } from '../services/artifacts';
import { FileIcon } from './FileIcon';
import { Badge } from './ui';
import { cn } from '../lib/cn';

export function RoundPanel(): React.ReactElement {
  const chatId = useFlux((s) => s.activeChatId);
  const artifacts = useFlux((s) => (s.activeChatId ? s.roundArtifacts[s.activeChatId] : undefined));

  if (!chatId) {
    return <Empty text="Open a conversation — the round lists what it changes." />;
  }
  if (!artifacts || artifacts.length === 0) {
    return (
      <Empty text="Nothing yet this round — file writes, edits and tool calls land here as the model works." />
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
      <div className="flex shrink-0 items-center gap-2 border-b border-border px-3 py-1.5">
        <span className="text-2xs font-medium text-muted">This round</span>
        <span className="text-2xs text-faint tabular-nums">{artifacts.length}</span>
        <span className="flex-1" />
        <span className="text-2xs text-faint">since your last message</span>
      </div>
      <ul aria-label="Round artifacts" className="m-0 flex list-none flex-col gap-px p-1.5">
        {artifacts.map((a) => (
          <li key={`${a.kind}:${a.target}`}>
            {a.kind === 'file' ? (
              <button
                type="button"
                onClick={() => openFilePreview(a.target)}
                title={`Open ${a.target}`}
                className={cn(
                  'flex w-full cursor-pointer items-center gap-2 rounded-sm px-1.5 py-1 text-left text-xs',
                  'transition-colors duration-fast hover:bg-hover',
                )}
              >
                <FileIcon name={a.target} />
                <span className="min-w-0 flex-1 truncate font-mono text-fg">
                  {fileName(a.target)}
                </span>
                <span className="min-w-0 flex-[1.4] truncate font-mono text-2xs text-faint">
                  {dirName(a.target)}
                </span>
                <Badge tone={a.change === 'write' ? 'accent' : 'default'} title={a.target}>
                  {a.change === 'write' ? 'W' : 'M'}
                </Badge>
              </button>
            ) : (
              <button
                type="button"
                onClick={() => jumpToToolCall(chatId, a.callId)}
                title="Show the tool call in the conversation"
                className={cn(
                  'flex w-full cursor-pointer items-center gap-2 rounded-sm px-1.5 py-1 text-left text-xs',
                  'transition-colors duration-fast hover:bg-hover',
                )}
              >
                {a.source === 'shell' ? (
                  <SquareTerminal size={13} aria-hidden="true" className="shrink-0 text-muted" />
                ) : (
                  <Puzzle size={13} aria-hidden="true" className="shrink-0 text-muted" />
                )}
                <span className="min-w-0 flex-1 truncate font-mono text-fg">{a.target}</span>
                <Badge title={a.source === 'shell' ? 'Shell invocation' : 'MCP tool invocation'}>
                  {a.source === 'shell' ? 'shell' : 'mcp'}
                </Badge>
              </button>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

function Empty({ text }: { text: string }): React.ReactElement {
  return (
    <div className="grid flex-1 place-items-center px-4 pb-16 text-center">
      <span className="max-w-64 text-sm text-faint">{text}</span>
    </div>
  );
}

function fileName(path: string): string {
  return path.split('/').filter(Boolean).pop() ?? path;
}

function dirName(path: string): string {
  const parts = path.split('/').filter(Boolean);
  return parts.slice(0, -1).join('/');
}
