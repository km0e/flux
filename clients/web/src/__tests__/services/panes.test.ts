import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { getPane, clearChatPane, switchToChat, _resetPanesForTest } from '../../services/panes';
import { useFlux } from '../../core/state';

describe('panes', () => {
  beforeEach(() => {
    document.body.innerHTML = '<div id="messages-wrap"></div>';
    _resetPanesForTest();
  });

  describe('getPane', () => {
    it('creates a new pane div inside messages-wrap', () => {
      const pane = getPane('c1');
      expect(pane.className).toContain('chat-pane');
      expect(pane.dataset.chatId).toBe('c1');
      const wrap = document.getElementById('messages-wrap');
      expect(wrap?.children).toHaveLength(1);
    });

    it('returns the same pane on subsequent calls', () => {
      const p1 = getPane('c1');
      const p2 = getPane('c1');
      expect(p1).toBe(p2);
    });

    it('throws when messages-wrap is missing', () => {
      document.body.innerHTML = '';
      expect(() => getPane('no-wrap')).toThrow('messages-wrap not found');
    });
  });

  describe('clearChatPane (leak regression)', () => {
    it('removes the pane from DOM and allows re-creation', () => {
      getPane('c1');
      const wrap = document.getElementById('messages-wrap');
      expect(wrap?.children).toHaveLength(1);

      clearChatPane('c1');
      // After clearChatPane → removePane, the div should be removed from DOM
      expect(wrap?.children).toHaveLength(0);

      // Re-creating should work (proves Map entry was deleted)
      getPane('c1');
      expect(wrap?.children).toHaveLength(1);
    });

    it('creating 100 panes caps at the LRU limit and deleting clears everything', () => {
      for (let i = 0; i < 100; i++) {
        getPane(`chat-${i}`);
      }
      const wrap = document.getElementById('messages-wrap');
      // The LRU cap bounds live panes (the most recent ones survive).
      expect(wrap?.children.length).toBeLessThanOrEqual(8);
      expect(wrap?.children.length).toBeGreaterThan(0);

      for (let i = 0; i < 100; i++) {
        clearChatPane(`chat-${i}`);
      }
      expect(wrap?.children).toHaveLength(0);
    });
  });

  describe('switchToChat', () => {
    afterEach(() => {
      vi.useRealTimers();
    });

    /** A pane carrying rendered history (what the skip/fade gate queries). */
    function withMessage(pane: HTMLDivElement): HTMLDivElement {
      const msg = document.createElement('div');
      msg.className = 'message user';
      pane.appendChild(msg);
      return pane;
    }

    it('reveals a populated target pane instantly; the outgoing one still dissolves', () => {
      vi.useFakeTimers();
      useFlux.setState({ activeChatId: 'a' });
      const a = withMessage(getPane('a'));
      const b = withMessage(getPane('b'));
      // Sentinel on the target: the instant path must not run the
      // set-0 → reflow → clear dance, i.e. must not touch inline opacity.
      b.style.opacity = '0.3';

      useFlux.setState({ activeChatId: 'b' }); // store first — the hide
      switchToChat('b'); // timer's guard reads it

      // Populated target: visible immediately, inline opacity untouched.
      expect(b.style.display).toBe('');
      expect(b.style.opacity).toBe('0.3');
      // Outgoing pane dissolves as the crossfade overlay…
      expect(a.classList.contains('switching-out')).toBe(true);
      // …and leaves the flow once the 120ms fade ends.
      vi.advanceTimersByTime(120);
      expect(a.style.display).toBe('none');
      expect(a.classList.contains('switching-out')).toBe(false);
    });

    it('keeps the fade-in for a blank target pane (first open, awaiting history)', () => {
      useFlux.setState({ activeChatId: 'a' });
      getPane('a');
      const blank = getPane('b'); // only the empty state — no .message yet
      blank.style.opacity = '0.3';

      useFlux.setState({ activeChatId: 'b' });
      switchToChat('b');

      // The fade path ran: opacity set to 0 then cleared after the reflow
      // (final inline ''), leaving the CSS transition to drive 0 → 1.
      expect(blank.style.display).toBe('');
      expect(blank.style.opacity).toBe('');
    });
  });
});
