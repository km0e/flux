/**
 * chat-input.test.tsx — composer behavior: Enter-to-send (IME-safe),
 * Shift+Enter newline, the unified send/stop button.
 */
import { describe, it, expect, vi } from 'vitest';
import { render, fireEvent } from '@testing-library/react';
import { ChatInput } from '../../components/ChatInput';

describe('ChatInput', () => {
  it('Enter sends the trimmed text and clears the field', () => {
    const onSend = vi.fn();
    const { getByPlaceholderText } = render(
      <ChatInput onSend={onSend} onCancel={vi.fn()} disabled={false} streaming={false} />,
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
      <ChatInput onSend={onSend} onCancel={vi.fn()} disabled={false} streaming={false} />,
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
      <ChatInput onSend={onSend} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    const input = getByPlaceholderText(/Ask Flux/);
    fireEvent.keyDown(input, { key: 'Enter', shiftKey: true });
    expect(onSend).not.toHaveBeenCalled();
  });

  it('empty input never sends', () => {
    const onSend = vi.fn();
    const { getByPlaceholderText } = render(
      <ChatInput onSend={onSend} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    fireEvent.keyDown(getByPlaceholderText(/Ask Flux/), { key: 'Enter' });
    expect(onSend).not.toHaveBeenCalled();
  });

  it('the button sends when idle', () => {
    const onSend = vi.fn();
    const { getByRole, getByPlaceholderText } = render(
      <ChatInput onSend={onSend} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    (getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement).value = 'hi';
    fireEvent.click(getByRole('button', { name: 'Send' }));
    expect(onSend).toHaveBeenCalledWith('hi');
  });

  it('the button cancels while streaming (stop semantics)', () => {
    const onCancel = vi.fn();
    const { getByRole } = render(
      <ChatInput onSend={vi.fn()} onCancel={onCancel} disabled={false} streaming={true} />,
    );
    fireEvent.click(getByRole('button', { name: 'Stop' }));
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it('disabled input cannot send', () => {
    const onSend = vi.fn();
    const { getByPlaceholderText } = render(
      <ChatInput onSend={onSend} onCancel={vi.fn()} disabled={true} streaming={false} />,
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
      <ChatInput onSend={vi.fn()} onCancel={vi.fn()} disabled={false} streaming={false} />,
    );
    expect(getByPlaceholderText('Ask Flux…')).toBeTruthy();
    expect(document.querySelector('#input')?.getAttribute('placeholder')).not.toContain('Enter');
    vi.unstubAllGlobals();
  });
});
