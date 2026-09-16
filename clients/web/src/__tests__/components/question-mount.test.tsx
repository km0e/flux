/**
 * question-mount.test.tsx — the askQuestion inline card's MOUNT semantics
 * (B10): the card lands in the question's OWN chat pane (found by chat id,
 * never a DOM-order guess — panes hide with inline display:none, so the
 * old `:not([hidden])` query hit the first-created pane and the card was
 * invisible whenever more than one chat had been visited), a question for
 * a background chat announces itself on the toast stack, a pane wipe /
 * chat deletion resolves the pending promise as dismissed instead of
 * hanging, and a newer question supersedes an older pending one.
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { fireEvent, waitFor } from '@testing-library/react';
import { registerDialogs } from '../../components/dialogs/registry';
import { dialogs, QUESTION_DISMISSED, resetDialogsForTest } from '../../services/dialogs';
import {
  ensurePane,
  clearChatPane,
  getPaneIfExists,
  _resetPanesForTest,
} from '../../services/panes';
import { useFlux, resetFluxForTest } from '../../core/state';

describe('askQuestion mount semantics (B10)', () => {
  beforeEach(() => {
    resetFluxForTest();
    resetDialogsForTest();
    _resetPanesForTest();
    document.body.innerHTML = '<div id="messages-wrap"></div>';
    registerDialogs();
  });

  afterEach(() => {
    // Dismantle every pane (resolves anything still pending) + roots.
    for (const id of ['c1', 'c2']) clearChatPane(id);
    _resetPanesForTest();
  });

  it('mounts into the question chat OWN pane, not the first DOM pane', async () => {
    // Two panes; c1 is created FIRST (DOM order), c2 is the active chat.
    ensurePane('c1');
    ensurePane('c2');
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'One', createdAt: 1, active: false, workdir: '', provider: '', model: '' },
        { id: 'c2', name: 'Two', createdAt: 2, active: false, workdir: '', provider: '', model: '' },
      ],
      activeChatId: 'c2',
    });

    const p = dialogs.askQuestion('c1', { text: 'Which database?', options: ['postgres'] });
    // The card lives in c1's pane — even though c1's pane is NOT the first
    // visible one (both panes are display:block here; the point is the id
    // addressing, and a display:none c1 would have defeated the old query).
    const card = document.getElementById('question-card-c1');
    expect(card).toBeTruthy();
    expect(card!.parentElement!.dataset.chatId).toBe('c1');
    // The ACTIVE chat's pane carries no card.
    expect(getPaneIfExists('c2')!.querySelector('.fx-question-mount')).toBeNull();

    // Answering through the card resolves the promise and unmounts it.
    // (Concurrent roots commit asynchronously — wait for the option.)
    const pane1 = getPaneIfExists('c1')!;
    await waitFor(() =>
      expect(
        Array.from(pane1.querySelectorAll('button')).some((b) => b.textContent === 'postgres'),
      ).toBe(true),
    );
    fireEvent.click(
      Array.from(pane1.querySelectorAll('button')).find((b) => b.textContent === 'postgres')!,
    );
    await expect(p).resolves.toBe('postgres');
    expect(document.getElementById('question-card-c1')).toBeNull();
  });

  it('a question for a background chat pushes a discoverability toast (B10)', async () => {
    ensurePane('c1');
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'One', createdAt: 1, active: false, workdir: '', provider: '', model: '' },
      ],
      activeChatId: 'c1',
    });
    useFlux.setState({ activeChatId: '' }); // c1 is now background
    const p = dialogs.askQuestion('c1', { text: 'Proceed?' });
    const toasts = useFlux.getState().toasts;
    expect(toasts.some((t) => t.kind === 'info' && t.text.includes('One'))).toBe(true);
    clearChatPane('c1');
    await expect(p).resolves.toBe(QUESTION_DISMISSED);
  });

  it('a pane wipe (stale resync) resolves the pending question as dismissed', async () => {
    ensurePane('c1');
    const p = dialogs.askQuestion('c1', { text: 'Proceed?' });
    expect(document.getElementById('question-card-c1')).toBeTruthy();
    clearChatPane('c1'); // the wipe path
    await expect(p).resolves.toBe(QUESTION_DISMISSED);
  });

  it('a NEWER question supersedes an older pending one for the same chat', async () => {
    ensurePane('c1');
    const first = dialogs.askQuestion('c1', { text: 'First?' });
    const second = dialogs.askQuestion('c1', { text: 'Second?', options: ['a', 'b'] });
    await expect(first).resolves.toBe(QUESTION_DISMISSED); // superseded, not hung
    // Only ONE card remains, carrying the newer question (async commit).
    await waitFor(() => {
      const card = document.getElementById('question-card-c1')!;
      expect(card.textContent).toContain('Second?');
    });
    clearChatPane('c1');
    await expect(second).resolves.toBe(QUESTION_DISMISSED);
  });

  it('no pane host at all (pre-render): toast + dismissal, never silence (B8)', async () => {
    document.body.innerHTML = ''; // no #messages-wrap
    const p = dialogs.askQuestion('c9', { text: 'Anyone there?' });
    const toasts = useFlux.getState().toasts;
    expect(toasts.some((t) => t.kind === 'error')).toBe(true);
    await expect(p).resolves.toBe(QUESTION_DISMISSED);
  });
});
