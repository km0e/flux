/**
 * chat-input.test.tsx — composer behavior: Enter-to-send (IME-safe),
 * Shift+Enter newline, the unified send/stop button, per-chat draft
 * preservation (switch away saves, remount restores, send clears,
 * deletion prunes).
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, fireEvent, screen } from '@testing-library/react';
import { ChatInput } from '../../components/ChatInput';
import { getDraft, saveDraft, pruneDrafts } from '../../services/drafts';
import { pairForkDraft, stashForkDraft } from '../../services/forkDraft';
import { useFlux, resetFluxForTest } from '../../core/state';
import { listDir } from '../../services/fs';
import { _resetMentionForTest } from '../../services/mention';
import type { FsListing } from '../../core/types';

vi.mock('../../services/fs', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../services/fs')>()),
  listDir: vi.fn(),
}));

function fsListing(entries: Array<[string, 'dir' | 'file']>): FsListing {
  return {
    type: 'fs_listing',
    requested: '',
    entries: entries.map(([name, kind]) => ({ name, kind })),
  };
}

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

// ── @-mention completion + drop-to-reference ─────────────────────────────
// A nested describe with its OWN store reset: the popup needs a chat with
// a workdir (the token has nowhere to resolve without one), and the file's
// other suites run store-free.
describe('ChatInput @-mentions', () => {
  beforeEach(() => {
    resetFluxForTest();
    vi.clearAllMocks();
    sessionStorage.clear();
    pruneDrafts(new Set());
    _resetMentionForTest(); // the dir cache outlives tests and would shadow the per-test listings
    useFlux.setState({
      chats: [
        { id: 'test-chat', name: 'T', createdAt: 1, active: false, workdir: '/proj', provider: '', model: '' },
      ],
    });
    vi.mocked(listDir).mockResolvedValue(
      fsListing([
        ['src', 'dir'],
        ['notes.md', 'file'],
        ['README.md', 'file'],
      ]),
    );
  });

  function type(text: string, caret: number) {
    const input = screen.getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement;
    input.value = text;
    input.setSelectionRange(caret, caret);
    fireEvent.input(input);
    return input;
  }

  it("typing '@' opens the completion popup with the workdir listing", async () => {
    render(<ChatInput chatId="test-chat" onSend={vi.fn()} onCancel={vi.fn()} disabled={false} streaming={false} />);
    type('@', 1);
    const popup = await screen.findByRole('listbox', { name: 'File path completion' });
    expect(popup.textContent).toContain('src/');
    expect(popup.textContent).toContain('notes.md');
  });

  it('Enter inserts the highlighted path + space, closes, and does NOT send', async () => {
    vi.mocked(listDir).mockResolvedValue(fsListing([['notes.md', 'file']]));
    const onSend = vi.fn();
    render(<ChatInput chatId="test-chat" onSend={onSend} onCancel={vi.fn()} disabled={false} streaming={false} />);
    const input = type('look at @', 9);
    await screen.findByRole('listbox', { name: 'File path completion' });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(input.value).toBe('look at notes.md ');
    expect(screen.queryByRole('listbox', { name: 'File path completion' })).toBeNull();
    expect(onSend).not.toHaveBeenCalled();
  });

  it('ArrowDown roves; Escape closes locally without sending', async () => {
    const onSend = vi.fn();
    render(<ChatInput chatId="test-chat" onSend={onSend} onCancel={vi.fn()} disabled={false} streaming={false} />);
    const input = type('@', 1);
    await screen.findByRole('listbox', { name: 'File path completion' });
    const options = () => [...screen.getByRole('listbox').querySelectorAll('[role="option"]')];
    expect(options()[0].getAttribute('aria-selected')).toBe('true'); // dirs first
    fireEvent.keyDown(input, { key: 'ArrowDown' });
    fireEvent.keyDown(input, { key: 'ArrowDown' });
    expect(options()[2].getAttribute('aria-selected')).toBe('true'); // files after dirs
    fireEvent.keyDown(input, { key: 'Escape' });
    expect(screen.queryByRole('listbox', { name: 'File path completion' })).toBeNull();
    expect(input.value).toBe('@'); // untouched
    expect(onSend).not.toHaveBeenCalled();
  });

  it('a directory choice keeps the popup open for the next segment', async () => {
    render(<ChatInput chatId="test-chat" onSend={vi.fn()} onCancel={vi.fn()} disabled={false} streaming={false} />);
    const input = type('@', 1);
    await screen.findByRole('listbox', { name: 'File path completion' });
    fireEvent.keyDown(input, { key: 'Enter' }); // picks 'src/' (dirs first)
    expect(input.value).toBe('@src/'); // dirs KEEP the @ — the token re-anchors
    // Still open — the drill-down listing resolves for the next segment.
    const popup = await screen.findByRole('listbox', { name: 'File path completion' });
    expect(popup).not.toBeNull();
  });

  it('dropping an Explorer row inserts the workdir-relative path at the caret', () => {
    render(<ChatInput chatId="test-chat" onSend={vi.fn()} onCancel={vi.fn()} disabled={false} streaming={false} />);
    const input = screen.getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement;
    input.value = 'review ';
    input.setSelectionRange(7, 7);
    fireEvent.drop(input, {
      dataTransfer: {
        types: ['text/plain'],
        getData: () => '/proj/src/lib/dom.ts',
      },
    });
    expect(input.value).toBe('review src/lib/dom.ts ');
  });
});
