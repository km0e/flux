import { describe, it, expect, beforeEach } from 'vitest';
import { getPane, clearChatPane, _resetPanesForTest } from '../../services/panes';

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
});
