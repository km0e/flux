/**
 * chat-input.test.tsx — composer behavior: Enter-to-send (IME-safe),
 * Shift+Enter newline, the unified send/stop button, per-chat draft
 * preservation (switch away saves, remount restores, send clears,
 * deletion prunes).
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, fireEvent } from '@testing-library/react';
import { ChatInput } from '../../components/ChatInput';
import { getDraft, saveDraft, pruneDrafts } from '../../services/drafts';
import { pairForkDraft, stashForkDraft } from '../../services/forkDraft';

// Drafts persist across renders BY DESIGN (that is the feature) — wipe the
// memory LRU + sessionStorage before every test so the shared "test-chat"
// id of the behavior tests above cannot leak composer text between cases.
beforeEach(() => {
  sessionStorage.clear();
  pruneDrafts(new Set());
});

describe('ChatInput', () => {
  it('Enter sends the trimmed text and clears the field', () => {
    const onSend = vi.fn();
    const { getByPlaceholderText } = render(
      <ChatInput chatId="test-chat" onSend={onSend} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    const input = getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement;
    input.value = '  hello  ';
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(onSend).toHaveBeenCalledWith('hello');
    expect(input.value).toBe('');
  });

  it('IME composition Enter does not send', () => {
    const onSend = vi.fn();
    const { getByPlaceholderText } = render(
      <ChatInput chatId="test-chat" onSend={onSend} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    const input = getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement;
    input.value = '候选文本';
    fireEvent.keyDown(input, { key: 'Enter', isComposing: true });
    expect(onSend).not.toHaveBeenCalled();
    // legacy keyCode 229 signal too
    fireEvent.keyDown(input, { key: 'Enter', keyCode: 229 });
    expect(onSend).not.toHaveBeenCalled();
  });

  it('Shift+Enter inserts a newline (does not send)', () => {
    const onSend = vi.fn();
    const { getByPlaceholderText } = render(
      <ChatInput chatId="test-chat" onSend={onSend} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    const input = getByPlaceholderText(/Ask Flux/);
    fireEvent.keyDown(input, { key: 'Enter', shiftKey: true });
    expect(onSend).not.toHaveBeenCalled();
  });

  it('empty input never sends', () => {
    const onSend = vi.fn();
    const { getByPlaceholderText } = render(
      <ChatInput chatId="test-chat" onSend={onSend} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    fireEvent.keyDown(getByPlaceholderText(/Ask Flux/), { key: 'Enter' });
    expect(onSend).not.toHaveBeenCalled();
  });

  it('the button sends when idle', () => {
    const onSend = vi.fn();
    const { getByRole, getByPlaceholderText } = render(
      <ChatInput chatId="test-chat" onSend={onSend} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    (getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement).value = 'hi';
    fireEvent.click(getByRole('button', { name: 'Send' }));
    expect(onSend).toHaveBeenCalledWith('hi');
  });

  it('the button cancels while streaming (stop semantics)', () => {
    const onCancel = vi.fn();
    const { getByRole } = render(
      <ChatInput chatId="test-chat" onSend={vi.fn()} onCancel={onCancel} disabled={false} streaming={true} />,
    );
    fireEvent.click(getByRole('button', { name: 'Stop' }));
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it('disabled input cannot send', () => {
    const onSend = vi.fn();
    const { getByPlaceholderText } = render(
      <ChatInput chatId="test-chat" onSend={onSend} onCancel={vi.fn()} disabled={true} streaming={false} />,
    );
    const input = getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement;
    expect(input.disabled).toBe(true);
    input.value = 'x';
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(onSend).not.toHaveBeenCalled();
  });

  it('mobile viewport swaps the placeholder for the short variant', () => {
    // The desktop placeholder carries the keyboard-hint suffix, which
    // wraps to a clipped second line at the mobile 16px composer font —
    // the mobile regime drops it. Stub matchMedia to the mobile match
    // (jsdom has none; the hook falls back to desktop without one).
    const mq = { matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() } as never;
    vi.stubGlobal('matchMedia', vi.fn(() => mq));
    const { getByPlaceholderText } = render(
      <ChatInput chatId="test-chat" onSend={vi.fn()} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    expect(getByPlaceholderText('Ask Flux…')).toBeTruthy();
    expect(document.querySelector('#input')?.getAttribute('placeholder')).not.toContain('Enter');
    vi.unstubAllGlobals();
  });
});

describe('ChatInput drafts (per-chat composer preservation)', () => {
  const type = (input: HTMLTextAreaElement, text: string) => {
    input.value = text;
  };

  it('restores the saved draft when remounting for the same chat', () => {
    const first = render(
      <ChatInput chatId="draft-a" onSend={vi.fn()} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    type(first.getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement, 'half-typed thought');
    first.unmount();

    const second = render(
      <ChatInput chatId="draft-a" onSend={vi.fn()} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    expect((second.getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement).value).toBe(
      'half-typed thought',
    );
  });

  it('does not leak drafts across chats', () => {
    const a = render(
      <ChatInput chatId="draft-leak-a" onSend={vi.fn()} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    type(a.getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement, 'chat A text');
    a.unmount();

    const b = render(
      <ChatInput chatId="draft-leak-b" onSend={vi.fn()} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    expect((b.getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement).value).toBe('');
  });

  it('clears the draft on send (an empty remount stays empty)', () => {
    const onSend = vi.fn();
    const first = render(
      <ChatInput chatId="draft-send" onSend={onSend} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    const input = first.getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement;
    type(input, 'going out');
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(onSend).toHaveBeenCalledWith('going out');
    expect(input.value).toBe('');
    first.unmount();

    const second = render(
      <ChatInput chatId="draft-send" onSend={vi.fn()} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    expect((second.getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement).value).toBe('');
  });

  it('a fork prefill wins over the saved draft and is consumed once', () => {
    saveDraft('draft-fork', 'stale saved text');
    stashForkDraft('draft-fork-src', 'fork redo turn');
    expect(pairForkDraft('draft-fork', 'draft-fork-src')).toBe(true);

    const first = render(
      <ChatInput chatId="draft-fork" onSend={vi.fn()} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    expect((first.getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement).value).toBe(
      'fork redo turn',
    );
    first.unmount();

    // The fork handoff is a once-consumable pairing: a second mount never
    // re-takes it (the saved-draft layer is the only path back).
    const second = render(
      <ChatInput chatId="draft-fork" onSend={vi.fn()} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    expect((second.getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement).value).toBe(
      'fork redo turn', // rides the unmount-save of the first mount, not the pairing
    );
    expect(pairForkDraft('draft-fork', 'draft-fork-src')).toBe(false);
  });

  it('pruneDrafts drops drafts of chats that no longer exist', () => {
    saveDraft('draft-dead', 'lost chat');
    saveDraft('draft-alive', 'kept chat');
    pruneDrafts(new Set(['draft-alive']));
    expect(getDraft('draft-dead')).toBe('');
    expect(sessionStorage.getItem('flux:draft:draft-dead')).toBeNull();
    expect(getDraft('draft-alive')).toBe('kept chat');
  });
});
