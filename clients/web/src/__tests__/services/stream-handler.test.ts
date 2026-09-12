import { describe, it, expect, beforeEach, vi } from 'vitest';
import { useFlux } from '../../core/state';
import { setBridge } from '../../core/bridge';
import type { ClientMessage } from '../../core/types';
import {
  appendUserMessage,
  markInterrupt,
  discardInterrupt,
  handleStreamEnd,
  handleStreamError,
  handleStreamGap,
  handleStreamCancelled,
  handleReasoningDelta,
  handleTextDelta,
  handleToolResult,
  incrementallyAnnounce,
  _resetAnnounceForTest,
  resetStreamingForReconnect,
} from '../../services/stream-handler';
import { renderHistoryMessages } from '../../services/history';
import { ensurePane, _resetPanesForTest } from '../../services/panes';
import { getController, getReasoningEntry, _resetReasoningForTest } from '../../services/stream';
import { createToolCard } from '../../lib/dom';

describe('stream-handler', () => {
  beforeEach(() => {
    // Reset state
    useFlux.getState().chats = [
      {
        id: 'c1',
        name: 'Test',
        createdAt: Date.now(),
        active: false,
        kind: 'classic',
        workdir: '/tmp/proj',
        provider: 'default',
        model: 'gpt-4o-mini',
      },
    ];
    useFlux.setState({ activeChatId: 'c1' });
    useFlux.setState({ streaming: {} });
    useFlux.setState({ usage: {} });
    _resetReasoningForTest();
    // Seed DOM and reset panes for isolation
    document.body.innerHTML = '<div id="messages-wrap"></div>';
    _resetPanesForTest();
    discardInterrupt('c1'); // module-level interrupt flag — isolate between tests
    setBridge({ send: vi.fn<(msg: ClientMessage) => void>() });
  });

  describe('appendUserMessage', () => {
    it('appends a user message bubble to the pane DOM', () => {
      appendUserMessage('hello world');
      const pane = document.querySelector('.chat-pane') as HTMLElement;
      expect(pane).toBeDefined();
      // User bubbles use class 'message user' (from dom.ts createMessageBubble)
      const userBubble = pane?.querySelector('.message.user') as HTMLElement;
      expect(userBubble).toBeDefined();
      if (userBubble) {
        expect(userBubble.textContent).toContain('hello world');
      }
    });

    it('appends a typing indicator after the bubble; first content removes it', () => {
      appendUserMessage('hello world');
      const pane = document.querySelector('.chat-pane') as HTMLElement;
      expect(pane.querySelector('#typing-indicator')).toBeTruthy();

      // The first reasoning/text delta → ensureReasoning/ensureStream removes the indicator
      handleReasoningDelta('c1', 'thinking...');
      expect(pane.querySelector('#typing-indicator')).toBeNull();
    });

    it('no-ops when no active chat', () => {
      useFlux.setState({ activeChatId: '' });
      appendUserMessage('orphan');
      // No pane and no chat mutation — the orphan message is dropped.
      expect(useFlux.getState().chats[0].name).toBe('Test');
    });

    it('marks the chat streaming when a user message is appended', () => {
      useFlux.setState({ activeChatId: 'chat-1' });
      appendUserMessage('hello');
      expect(useFlux.getState().streaming['chat-1']).toBe(true);
    });

    it('keeps streaming after appending over an existing controller (round 2+ path)', () => {
      useFlux.setState({ activeChatId: 'chat-1' });
      // Controller exists from the previous round — disposeController runs in
      // appendUserMessage and must not wipe the optimistic round-level truth.
      getController('chat-1');
      appendUserMessage('hello');
      expect(useFlux.getState().streaming['chat-1']).toBe(true);
    });
  });

  describe('renderHistoryMessages', () => {
    it('does not clear streaming state while the chat is streaming', () => {
      // A live round must be protected: a duplicate chat_open / refresh race
      // delivering history mid-stream must not tear down the pane or clear
      // the streaming flag (that is the in-flight content's state).
      const ctrl = getController('c1');
      ctrl.renderType = 'text'; // mark as active to prevent idle dispose
      useFlux.getState().setStreaming('c1', true);
      renderHistoryMessages('c1', []);
      expect(useFlux.getState().streaming['c1']).toBe(true);
    });

    it('renders user and assistant messages into the pane', async () => {
      await renderHistoryMessages('c1', [
        { role: 'user', content: 'hi', tool_calls: [] },
        { role: 'assistant', content: 'hello!', tool_calls: [], reasoning_content: undefined },
      ]);
      const pane = document.querySelector('.chat-pane') as HTMLElement;
      expect(pane?.querySelector('.message.user')).toBeDefined();
      expect(pane?.querySelector('.message.assistant')).toBeDefined();
    });
  });

  describe('handleStreamEnd (streaming clear)', () => {
    it('clears streaming state and flushes render', () => {
      useFlux.getState().setStreaming('c1', true);
      handleTextDelta('c1', 'hello');
      expect(useFlux.getState().streaming['c1']).toBe(true);

      handleStreamEnd('c1');
      expect(useFlux.getState().streaming['c1']).toBeUndefined();
    });

    it('appends a truncation notice when finish_reason signals a cut', () => {
      handleStreamEnd('c1', 'length');
      const pane = document.querySelector('.chat-pane') as HTMLElement;
      const noticeEl = pane?.querySelector('.message.notice') as HTMLElement;
      expect(noticeEl).toBeDefined();
      if (noticeEl) {
        expect(noticeEl.textContent).toContain('truncated');
      }
      expect(pane?.querySelector('.message.error')).toBeNull();
    });

    it('shows a content-filter notice for content_filter', () => {
      handleStreamEnd('c1', 'content_filter');
      const noticeEl = document.querySelector('.message.notice') as HTMLElement;
      expect(noticeEl?.textContent).toContain('content filter');
    });

    it('appends no notice when finish_reason is absent or normal', () => {
      handleStreamEnd('c1');
      expect(document.querySelector('.message.notice')).toBeNull();
      handleStreamEnd('c1', 'stop');
      expect(document.querySelector('.message.notice')).toBeNull();
      handleStreamEnd('c1', 'tool_calls');
      expect(document.querySelector('.message.notice')).toBeNull();
    });
  });

  describe('handleStreamError (streaming clear)', () => {
    it('clears streaming state on stream error', () => {
      useFlux.getState().setStreaming('c1', true);
      handleStreamError('c1', 'something went wrong');
      expect(useFlux.getState().streaming['c1']).toBeUndefined();
    });

    it('appends an error bubble to the pane', () => {
      handleStreamError('c1', 'test error');
      const pane = document.querySelector('.chat-pane') as HTMLElement;
      const errorEl = pane?.querySelector('.message.error') as HTMLElement;
      expect(errorEl).toBeDefined();
      if (errorEl) {
        expect(errorEl.textContent).toContain('test error');
      }
    });
  });

  describe('handleStreamCancelled', () => {
    it('clears streaming state on cancellation', () => {
      useFlux.getState().setStreaming('c1', true);
      handleStreamCancelled('c1');
      expect(useFlux.getState().streaming['c1']).toBeUndefined();
    });

    it('appends a neutral notice bubble, not an error', () => {
      handleStreamCancelled('c1');
      const pane = document.querySelector('.chat-pane') as HTMLElement;
      const noticeEl = pane?.querySelector('.message.notice') as HTMLElement;
      expect(noticeEl).toBeDefined();
      if (noticeEl) {
        expect(noticeEl.textContent).toContain('Cancelled');
      }
      expect(pane?.querySelector('.message.error')).toBeNull();
    });
  });

  describe('interrupt-send (R1: server-fused cancel + next message)', () => {
    it('suppresses the cancelled notice and keeps streaming live for the self-triggered cancel', () => {
      useFlux.getState().setStreaming('c1', true);
      markInterrupt('c1');
      handleStreamCancelled('c1');
      expect(document.querySelector('.message.notice')).toBeNull();
      // Streaming stays live through the wrap-up gap — the replacement
      // round follows server-side in the same event burst.
      expect(useFlux.getState().streaming['c1']).toBe(true);
    });

    it('carries the flag through the cancelled round stream_end (no Stop→Send flicker)', () => {
      useFlux.getState().setStreaming('c1', true);
      markInterrupt('c1');
      handleStreamCancelled('c1');
      handleStreamEnd('c1');
      // The replacement round started server-side at the wrap-up — the
      // flag must not drop here (its own stream_end clears it).
      expect(useFlux.getState().streaming['c1']).toBe(true);

      // The replacement round's own wrap-up clears normally.
      handleStreamEnd('c1');
      expect(useFlux.getState().streaming['c1']).toBeUndefined();
    });

    it('clears streaming at the first stream_end when no cancel was seen (Idle engine)', () => {
      // Interrupt-send on an Idle engine: the cancel is absorbed, the
      // message starts immediately — this stream_end IS the replacement
      // round's own end.
      useFlux.getState().setStreaming('c1', true);
      markInterrupt('c1');
      handleStreamEnd('c1');
      expect(useFlux.getState().streaming['c1']).toBeUndefined();

      // The flag retired — a later round behaves normally.
      useFlux.getState().setStreaming('c1', true);
      handleStreamEnd('c1');
      expect(useFlux.getState().streaming['c1']).toBeUndefined();
    });

    it('an explicit stop after the interrupt-send behaves like a plain cancel (stop means stop)', () => {
      useFlux.getState().setStreaming('c1', true);
      markInterrupt('c1');
      discardInterrupt('c1');
      handleStreamCancelled('c1');
      expect(document.querySelector('.message.notice')?.textContent).toContain('Cancelled');
      expect(useFlux.getState().streaming['c1']).toBeUndefined();
    });

    it('a stream error retires the flag (the replacement round failed; nothing follows)', () => {
      useFlux.getState().setStreaming('c1', true);
      markInterrupt('c1');
      handleStreamCancelled('c1');
      handleStreamEnd('c1'); // wrap-only: streaming stays live
      expect(useFlux.getState().streaming['c1']).toBe(true);

      handleStreamError('c1', 'boom');
      expect(useFlux.getState().streaming['c1']).toBeUndefined();
      // The retired flag must not keep a later round alive.
      useFlux.getState().setStreaming('c1', true);
      handleStreamEnd('c1');
      expect(useFlux.getState().streaming['c1']).toBeUndefined();
    });

    it('reconnect reset retires the flag', () => {
      useFlux.getState().setStreaming('c1', true);
      markInterrupt('c1');
      resetStreamingForReconnect();
      handleStreamCancelled('c1');
      expect(document.querySelector('.message.notice')?.textContent).toContain('Cancelled');
    });
  });

  describe('empty deltas', () => {
    it('does not create an assistant bubble or streaming state for an empty text delta', () => {
      handleTextDelta('c1', '');
      expect(useFlux.getState().streaming['c1']).toBeUndefined();
      expect(document.querySelectorAll('.message.assistant').length).toBe(0);
      expect(document.querySelectorAll('.chat-pane').length).toBe(0);
    });

    it('does not create a reasoning block for an empty reasoning delta', () => {
      handleReasoningDelta('c1', '');
      expect(getReasoningEntry('c1')).toBeUndefined();
      expect(document.querySelectorAll('.chat-pane').length).toBe(0);
    });
  });

  describe('reasoning-only round finalization', () => {
    it('finalizes the reasoning block on stream_end when no text arrived', () => {
      vi.useFakeTimers();
      try {
        const ctrl = getController('c1');
        ctrl.appendReasoning('thinking hard');
        expect(getReasoningEntry('c1')).toBeDefined();
        expect(ctrl.renderType).toBe('reasoning');

        handleStreamEnd('c1');
        // closeReasoning defers until the typewriter sweep completes.
        vi.advanceTimersByTime(5000);
        expect(getReasoningEntry('c1')).toBeUndefined();
        expect(ctrl.renderType).toBe('');
        const summary = document.querySelector('summary');
        expect(summary?.textContent).toMatch(/^Thought for \d+s$/);
      } finally {
        vi.useRealTimers();
      }
    });

    it('finalizes the reasoning block on stream error when no text arrived', () => {
      vi.useFakeTimers();
      try {
        const ctrl = getController('c1');
        ctrl.appendReasoning('thinking hard');
        expect(getReasoningEntry('c1')).toBeDefined();

        handleStreamError('c1', 'boom');
        vi.advanceTimersByTime(5000);
        expect(getReasoningEntry('c1')).toBeUndefined();
        expect(ctrl.renderType).toBe('');
      } finally {
        vi.useRealTimers();
      }
    });
  });

  describe('resetStreamingForReconnect', () => {
    it('clears streaming state and loaded chat', () => {
      useFlux.setState({ streaming: { 'chat-1': true, 'chat-2': true } });
      useFlux.setState({ loadedChatId: 'chat-1' });
      useFlux.getState().setReadOnly('chat-1', true);
      resetStreamingForReconnect();
      expect(useFlux.getState().streaming).toEqual({});
      expect(useFlux.getState().loadedChatId).toBe('');
      expect(useFlux.getState().readonlyChats['chat-1']).toBeUndefined();
    });
  });

  describe('handleStreamGap', () => {
    it('appends a reload notice; clicking it re-claims for the lease holder', () => {
      const send = vi.fn<(msg: ClientMessage) => void>();
      setBridge({ send });
      handleStreamGap('c1');

      const pane = ensurePane('c1');
      const el = pane.querySelector('.stream-gap') as HTMLElement;
      expect(el).toBeDefined();
      if (el) {
        el.click();
        // Resubscription must close first (close triggers ViewerGone, clearing
        // the dropped-frame mark) before reopening.
        // Reopen with claim, not open: not read-only = we hold the lease, and
        // open would silently downgrade the lease holder to a viewer; a refused
        // claim is degraded by the busy arm with a follow-up open.
        expect(send.mock.calls).toEqual([
          [{ type: 'chat_close', chat_id: 'c1' }],
          [{ type: 'chat_claim', chat_id: 'c1' }],
        ]);
      }
    });

    it('falls back to chat_open for a read-only viewer', () => {
      const send = vi.fn<(msg: ClientMessage) => void>();
      setBridge({ send });
      useFlux.getState().setReadOnly('c1', true);
      handleStreamGap('c1');

      const pane = ensurePane('c1');
      const el = pane.querySelector('.stream-gap') as HTMLElement;
      if (el) {
        el.click();
        // A pure viewer has no lease to recover: keep the open path.
        expect(send.mock.calls).toEqual([
          [{ type: 'chat_close', chat_id: 'c1' }],
          [{ type: 'chat_open', chat_id: 'c1' }],
        ]);
      }
    });
  });

  describe('handleToolResult', () => {
    it('completes a tool card inside the target chat pane (id with selector metacharacters)', () => {
      const pane = ensurePane('c1');
      const card = createToolCard({ id: 'x"y', name: 'bash', args: '{}', status: 'running' });
      pane.appendChild(card);

      expect(() => handleToolResult('c1', 'x"y', 'done')).not.toThrow();

      const el = pane.querySelector('[data-tool-call-id="x\\"y"]') as HTMLElement | null;
      expect(el).toBeTruthy();
      expect(el?.classList.contains('done')).toBe(true);
    });

    it('does not touch a same-id card in another chat when the result names this chat', () => {
      const paneA = ensurePane('c1');
      const paneB = ensurePane('c2');
      const cardA = createToolCard({ id: 'same-id', name: 'bash', args: '{}', status: 'running' });
      const cardB = createToolCard({ id: 'same-id', name: 'bash', args: '{}', status: 'running' });
      paneA.appendChild(cardA);
      paneB.appendChild(cardB);

      handleToolResult('c2', 'same-id', 'done');

      expect(cardB.classList.contains('done')).toBe(true);
      expect(cardA.classList.contains('done')).toBe(false);
    });

    it('no-ops without creating a pane when the target chat has none', () => {
      expect(() => handleToolResult('ghost', 'id', 'done')).not.toThrow();
      expect(document.querySelectorAll('.chat-pane').length).toBe(0);
    });
  });

  describe('incrementallyAnnounce', () => {
    beforeEach(() => {
      _resetAnnounceForTest();
    });

    it('announces a sentence once it completes across deltas', () => {
      incrementallyAnnounce('Hello ');
      incrementallyAnnounce('world.');
      const region = document.getElementById('stream-announcer');
      expect(region?.textContent).toBe('Hello world.');
    });

    it('does not re-announce already-announced text on later deltas', () => {
      incrementallyAnnounce('First sentence.');
      const region = document.getElementById('stream-announcer');
      expect(region?.textContent).toBe('First sentence.');
      incrementallyAnnounce(' Second typ');
      // No new complete sentence — announcement stays unchanged.
      expect(region?.textContent).toBe('First sentence.');
      incrementallyAnnounce('ing.');
      expect(region?.textContent).toBe('Second typing.');
    });

    it('holds a partial sentence without announcing until punctuation arrives', () => {
      incrementallyAnnounce('the cat sat on the mat without ending');
      // Nothing announced yet (no terminal punctuation) and no announcer created.
      expect(document.getElementById('stream-announcer')).toBeNull();
      incrementallyAnnounce('. And now another ran.');
      const region = document.getElementById('stream-announcer');
      expect(region?.textContent).toBe('And now another ran.');
    });

    it('caps the pending buffer so a punctuation-less run-on cannot grow unboundedly', () => {
      const big = 'x'.repeat(1000); // no punctuation — never completes a sentence
      incrementallyAnnounce(big);
      incrementallyAnnounce(big);
      expect(document.getElementById('stream-announcer')).toBeNull();
      // A later punctuated delta still announces correctly with the bounded tail.
      incrementallyAnnounce(' now end.');
      const region = document.getElementById('stream-announcer');
      expect(region?.textContent).toContain('end.');
    });
  });
});
