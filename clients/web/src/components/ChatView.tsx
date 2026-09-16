/**
 * ChatView.tsx — Main chat column.
 *
 * Composes the conversation header (identity + per-chat usage), the
 * message list, and the composer for the active chat; sends chat messages
 * and cancel requests to the server. The composer footer is a REAL row:
 * the provider/model pin at the input's bottom-left (the surface it acts
 * on) and the context-pressure meter at the right (usage.contextTokens
 * against the probed context_length — hidden when either is unknown).
 *
 * Provides: ChatView, ContextMeter
 * Depends: components/ChatHeader.tsx, components/MessageList.tsx,
 *          components/ChatInput.tsx, components/ProviderSwitchDialog.tsx,
 *          components/dialogs (viewer bar), core/state.ts, core/bridge.ts,
 *          services/stream-handler.ts
 */
import { useState } from 'react';
import { useFlux } from '../core/state';
import { bridge } from '../core/bridge';
import { MessageList } from './MessageList';
import { ChatInput } from './ChatInput';
import { ChatHeader } from './ChatHeader';
import { ProviderSwitchDialog } from './ProviderSwitchDialog';
import { appendUserMessage, appendInterjectedMessage, markInterrupt, discardInterrupt } from '../services/stream-handler';
import { Button } from './ui';
import { Tooltip } from './ui/tooltip';
import { useCoveredByDrawer } from '../hooks/useCoveredByDrawer';
import { cn } from '../lib/cn';

const CONN_BANNER: Record<'connecting' | 'disconnected' | 'failed', string> = {
  connecting: 'Connecting…',
  disconnected: 'Disconnected — queued messages will be sent on reconnect',
  failed: 'Connection failed — click Reconnect in the top bar to retry',
};

/** Context-pressure meter — `usage.contextTokens` against the model's
 * effective context length: the saved-model registry wins (the user's
 * deployment truth), the probed catalog falls back; `effectiveContextLength`
 * reads the store directly (frame data already rides it, no extra
 * request). Hidden when either side is unknown; the tone escalates as the
 * window fills. */
function ContextMeter(props: { cid: string; provider: string; model: string }): React.ReactElement | null {
  const usage = useFlux((s) => s.usage[props.cid]);
  const savedModels = useFlux((s) => s.savedModels);
  const catalog = useFlux((s) => s.providerModels[props.provider]);
  const len =
    savedModels.find((m) => m.provider === props.provider && m.model === props.model)?.params
      .context_length ?? catalog?.find((m) => m.id === props.model)?.context_length;
  const used = usage?.contextTokens ?? 0;
  if (!len || !used) return null;
  const pct = Math.min(100, Math.round((used / len) * 100));
  const title = `Context ${used.toLocaleString()} / ${len.toLocaleString()} tokens (${pct}%)`;
  return (
    <Tooltip content={title} side="top">
      <div id="context-meter" className="flex items-center gap-1.5" aria-label={title}>
        <span className="h-1 w-16 overflow-hidden rounded-full bg-hover">
          <span
            aria-hidden="true"
            className={cn(
              'block h-full rounded-full transition-all duration-base',
              pct > 92 ? 'bg-danger' : pct > 80 ? 'bg-warn' : 'bg-accent',
            )}
            style={{ width: `${pct}%` }}
          />
        </span>
        <span className="font-mono text-2xs tabular-nums text-faint">{pct}%</span>
      </div>
    </Tooltip>
  );
}

export function ChatView(): React.ReactElement {
  const cid = useFlux((s) => s.activeChatId);
  const active = useFlux((s) => s.chats.find((c) => c.id === s.activeChatId));
  const streaming = useFlux((s) => (cid ? (s.streaming[cid] ?? false) : false));
  const readonly = useFlux((s) => (cid ? (s.readonlyChats[cid] ?? false) : false));
  const connStatus = useFlux((s) => s.connectionStatus);
  const coveredByDrawer = useCoveredByDrawer();
  const [switching, setSwitching] = useState(false);

  const banner = connStatus === 'connected' ? null : CONN_BANNER[connStatus];

  const onSend = (text: string) => {
    if (!cid) return;
    // Direct store read, not the render-derived closure: a double Enter
    // in the same tick still hits this gate after the first send set it.
    if (useFlux.getState().streaming[cid]) {
      // R1 interrupt-send: ONE operation — the server fuses "cancel the
      // live round" and "queue my message" onto one kernel FIFO, so the
      // message can never be lost to the cancel's queue clear, no matter
      // what order independent HTTP requests would have arrived in. The
      // replacement round starts server-side at the wrap-up; the bubble
      // appends now (without disposing the live controller — the cancelled
      // round still owns it).
      markInterrupt(cid);
      appendInterjectedMessage(text);
      bridge.send({ type: 'chat', chat_id: cid, message: text, interrupt: true });
      return;
    }
    appendUserMessage(text);
    bridge.send({ type: 'chat', chat_id: cid, message: text });
  };

  const onCancel = () => {
    if (!cid) return;
    // An explicit stop after an interrupt-send kills the queued turn too
    // ("stop means stop") — retire the self-cancel bookkeeping so the
    // wrap-up shows the usual notice.
    discardInterrupt(cid);
    bridge.send({ type: 'cancel', chat_id: cid });
  };

  const onTakeOver = () => {
    if (!cid) return;
    // Optimistically clear the mark; a refused claim re-attaches the bar.
    useFlux.getState().setReadOnly(cid, false);
    bridge.send({ type: 'chat_claim', chat_id: cid });
  };

  return (
    // Inert while the mobile drawer covers this column: keyboard/AT focus
    // must not walk behind the overlay. The TopBar (outside this element)
    // stays reachable — its toggle is how the drawer closes.
    <div id="main" inert={coveredByDrawer} className="flex min-h-0 min-w-0 flex-1 flex-col">
      {/* The conversation's header row: identity + per-chat usage. */}
      <ChatHeader />
      <div className="flex min-h-0 flex-1 flex-col px-4 pb-3">
        <MessageList />
        {banner && (
          <div
            id="conn-banner"
            role="status"
            className="mx-auto w-full max-w-[var(--fx-chat-max)] border-t border-border px-3 py-1.5 text-2xs text-muted"
          >
            {banner}
          </div>
        )}
        {readonly ? (
          <div
            id="readonly-banner"
            role="status"
            className="mx-auto flex w-full max-w-[var(--fx-chat-max)] items-center justify-between gap-3 border-t border-warn/40 bg-warn/5 px-3 py-1.5 text-2xs text-warn"
          >
            <span>Read-only — this conversation is in use by another window</span>
            <Button size="sm" variant="secondary" onClick={onTakeOver}>
              Take over
            </Button>
          </div>
        ) : (
          <div className="mx-auto w-full max-w-[var(--fx-chat-max)]">
            <ChatInput
              key={cid}
              chatId={cid}
              onSend={onSend}
              onCancel={onCancel}
              /* The composer stays ENABLED through an outage: a message
                 typed offline queues in the connection manager and rides
                 out with the reconnect (the banner says so) — blocking
                 the keyboard would throw away the user's thought at the
                 exact moment the connection hiccupped. Only a missing
                 chat disables it. */
              disabled={!cid}
              streaming={streaming}
            />
            {/* Composer footer — the provider/model switch at the input's
                bottom-left (lease holders only: a read-only viewer has no
                lease to swap with, and this branch never renders for one);
                the context meter balances it at the right. Offline the swap
                cannot reach the server, so the chip sits inert alongside
                the disabled composer. */}
            {cid && active && (
              <div id="composer-meta" className="flex items-center gap-2 px-1 pt-1">
                <button
                  id="provider-chip"
                  type="button"
                  disabled={connStatus !== 'connected'}
                  className={cn(
                    'min-w-0 max-w-56 cursor-pointer truncate rounded-sm border border-border bg-inset px-1.5 py-0.5 max-md:min-h-9 max-md:py-1.5',
                    'font-mono text-2xs text-muted transition-colors duration-fast hover:border-border-strong hover:text-fg',
                    'disabled:cursor-not-allowed disabled:opacity-40',
                  )}
                  title="Switch provider / model (applies at the round boundary)"
                  onClick={() => setSwitching(true)}
                >
                  {active.provider || 'default'}
                  {active.model ? ` / ${active.model}` : ''}
                </button>
                <span className="flex-1" />
                <ContextMeter cid={cid} provider={active.provider} model={active.model} />
              </div>
            )}
          </div>
        )}
        {switching && cid && active && (
          <ProviderSwitchDialog
            chatId={cid}
            provider={active.provider}
            model={active.model}
            onClose={() => setSwitching(false)}
          />
        )}
      </div>
    </div>
  );
}
