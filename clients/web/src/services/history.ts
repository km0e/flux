/**
 * history.ts — Complete chat-history rendering into a pane.
 *
 * Receives `chat_history` payloads from the server and constructs the
 * static message DOM (user/assistant bubbles, reasoning blocks, tool cards).
 *
 * Provides: renderHistoryMessages
 * Depends: lib/markdown.ts, services/panes.ts, services/stream.ts,
 *          lib/dom.ts, core/types.ts
 */

import { log } from '../logger';
import { preloadHighlighter } from '../lib/highlight';
import { renderMarkdown } from '../lib/markdown';
import { getPane, hideEmptyState } from './panes';
import { disposeController } from './stream';
import { dialogs } from './dialogs';
import { bridge } from '../core/bridge';
import {
  createMessageBubble,
  createReasoningBlock,
  createToolCard,
  escapeCssSelector,
  forceFollow,
  setToolCardResult,
  toolIconSvg,
} from '../lib/dom';
import type { HistoryMessage } from '../core/types';
import { useFlux } from '../core/state';

// ── History rendering ──

/**
 * Render complete chat history messages into a pane.
 * Called when the server sends `chat_history` in response to a first
 * `chat_open` subscription or a `chat_claim` (D-06 single-message claim).
 */
export async function renderHistoryMessages(chatId: string, messages: HistoryMessage[]): Promise<void> {
  // The highlighter is a deferred async chunk prefetched at boot; awaiting
  // it here is the one place the tiny load window is realistic (history can
  // render before the prefetch lands) — without the wait, code blocks would
  // render plain and, being committed-once, stay plain until re-open.
  await preloadHighlighter();
  log.debug('renderHistory ' + chatId + ' msgs=' + messages.length);
  // Safety net: never tear down a live round. If this chat is actively
  // streaming, the live stream is rendering newer content — rebuilding from
  // a history snapshot would dispose the controller and clear the pane,
  // dropping in-flight frames. (The server sends history before subscribing
  // on ChatOpen, so this is a race/duplicate-open path, not the norm.)
  if (useFlux.getState().streaming[chatId]) {
    log.debug('renderHistory skipped: ' + chatId + ' is streaming');
    return;
  }
  // Dispose any active stream controller for this chat
  disposeController(chatId);

  const pane = getPane(chatId);

  // Clear existing message DOM (keep empty state element)
  for (const child of Array.from(pane.children)) {
    if (!child.id || !child.id.startsWith('empty-')) {
      child.remove();
    }
  }

  if (messages.length > 0) {
    hideEmptyState(chatId);
    // Opening a chat means looking at the latest message: scroll to the
    // bottom + reset stick, so the live follow is not lost afterwards. Without
    // the scroll a long history stays parked at the top, the latest reply out
    // of view.
    forceFollow(pane);
  }

  let staggerIdx = 0;

  for (const msg of messages) {
    switch (msg.role) {
      case 'user': {
        // Manual rebase affordance (§2.1 of the protocol-hardening ledger):
        // history row ids are the rebase base keys. Gates: the id must be
        // present (assembled messages carry none) and the operator holds
        // the lease (a mutation).
        const rebaseable =
          msg.id !== undefined && useFlux.getState().readonlyChats[chatId] !== true;
        const bubble = createMessageBubble({
          role: 'user',
          text: msg.content,
          staggerIndex: staggerIdx++,
          historyId: rebaseable ? msg.id : undefined,
        });
        const rebaseBtn = bubble.el.querySelector<HTMLButtonElement>('.msg-rebase');
        rebaseBtn?.addEventListener('click', () => {
          void rebaseFrom(chatId, msg.id!, msg.content);
        });
        pane.appendChild(bubble.el);
        break;
      }
      case 'assistant': {
        // Reasoning block
        if (msg.reasoning_content) {
          const block = createReasoningBlock({
            summary: 'Thought for a bit',
            html: renderMarkdown(msg.reasoning_content),
            staggerIndex: staggerIdx,
          });
          block.el.open = false;
          pane.appendChild(block.el);
        }

        // Assistant message body — reasoning-only rounds (e.g. thinking +
        // tool calls with no prose) have empty content: no bubble at all,
        // or a hollow shell renders under the thinking block.
        if (msg.content) {
          const bubble = createMessageBubble({
            role: 'assistant',
            html: renderMarkdown(msg.content),
            raw: msg.content, // enables createCopyButton to read the raw text
            staggerIndex: staggerIdx++,
          });
          pane.appendChild(bubble.el);
        }

        // Tool call cards — `tool_calls` is absent on the wire when empty
        // (the server skips serializing empty vecs).
        for (const tc of msg.tool_calls ?? []) {
          const toolEl = createToolCard({
            id: tc.id,
            name: tc.name,
            args: tc.arguments,
            status: 'done',
            staggerIndex: staggerIdx++,
          });
          pane.appendChild(toolEl);
        }
        break;
      }
      case 'tool': {
        const result = msg.content ?? '';
        // Merge into the already-rendered call card first (assistant
        // tool_calls built cards by id) — consistent with the live path
        // (tool_start builds the card, tool_result fills it): call + result in
        // ONE card instead of an extra anonymous orphan "result" card.
        const callCard = msg.tool_call_id
          ? pane.querySelector<HTMLElement>(
              `[data-tool-call-id="${escapeCssSelector(msg.tool_call_id)}"]`,
            )
          : null;
        if (callCard) {
          setToolCardResult(callCard, result);
          break;
        }
        // Fallback: no matching call card (truncated history / result-only message) → a standalone result card.
        const toolEl = createToolCard({
          id: msg.tool_call_id || '',
          name: '',
          args: undefined,
          status: 'done',
          staggerIndex: staggerIdx++,
        });
        // Override tool card appearance for inline tool results — same
        // icon system as createToolCard, anonymous "result" status.
        const header = toolEl.querySelector('.tool-header') as HTMLElement;
        if (header) {
          header.innerHTML =
            '<span class="tool-icon">' +
            toolIconSvg('result') +
            '</span>' +
            '<span class="tool-args-summary"></span>' +
            '<span class="tool-status completed"> result</span>';
          header.setAttribute('aria-label', 'Tool result');
        }
        // setToolCardResult fills the result container (meta + pre + copy
        // button) — the same shape the matched-card path gets.
        if (result) setToolCardResult(toolEl, result);
        pane.appendChild(toolEl);
        break;
      }
    }
  }
}

/** Manual rebase (the §2.1 entry): confirm, then send the rebase with the
 * clicked message's row id as the base — everything at/below it archives
 * out of the model's context (messages ABOVE it stay). The server applies
 * at the machine gate (a live round finishes first), echoes the ACTUAL
 * base in the response, and announces `context_rebased` on the stream —
 * the notice bubble renders from that, never from here. */
async function rebaseFrom(chatId: string, messageId: number, snippet: string): Promise<void> {
  const preview = snippet.length > 60 ? snippet.slice(0, 60) + '…' : snippet;
  let confirmed = false;
  try {
    confirmed = await dialogs.confirmRebase(preview);
  } catch (err) {
    log.error('rebase confirm failed: ' + (err instanceof Error ? err.message : String(err)));
    return;
  }
  if (!confirmed) return;
  bridge.send({ type: 'rebase', chat_id: chatId, base_message_id: messageId });
}
