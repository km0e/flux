/**
 * command-palette.test.tsx — the Ctrl/Cmd+K surface: opens from the store,
 * filters over title+keywords, keyboard navigation runs the highlighted
 * action, running closes the palette, empty queries get a status line.
 */
import { describe, it, expect, beforeEach } from 'vitest';
import { render, fireEvent, act, screen } from '@testing-library/react';
import { CommandPalette } from '../../components/CommandPalette';
import { useFlux, resetFluxForTest } from '../../core/state';

function openPalette() {
  render(<CommandPalette />);
  act(() => {
    useFlux.setState({ paletteOpen: true });
  });
}

function input(): HTMLInputElement {
  return screen.getByLabelText('Type a command') as HTMLInputElement;
}

describe('CommandPalette', () => {
  beforeEach(() => {
    resetFluxForTest();
  });

  it('renders nothing until opened, then lists actions and chats', () => {
    const { container } = render(<CommandPalette />);
    expect(container.querySelector('[aria-label="Command palette"]')).toBeNull();
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'Parser fix', createdAt: 1, active: false, workdir: '/repo/a', provider: '', model: '' },
      ],
    });
    openPalette();
    const listbox = screen.getByRole('listbox', { name: 'Commands' });
    expect(listbox.textContent).toContain('New chat');
    expect(listbox.textContent).toContain('Parser fix');
  });

  it('filters over title and keywords; an empty result gets a status line', () => {
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'Parser fix', createdAt: 1, active: false, workdir: '/repo/a', provider: '', model: '' },
      ],
    });
    openPalette();
    act(() => {
      fireEvent.change(input(), { target: { value: '/repo/a' } });
    });
    // The workdir is KEYWORDS, not title — a match proves the filter reads both.
    expect(screen.getByRole('listbox').textContent).toContain('Parser fix');
    expect(screen.getByRole('listbox').textContent).not.toContain('New chat');
    act(() => {
      fireEvent.change(input(), { target: { value: 'zzz-nothing' } });
    });
    expect(screen.getByRole('status').textContent).toContain('No matching commands');
  });

  it('Enter runs the highlighted action and closes the palette', () => {
    useFlux.setState({ sidebarOpen: true });
    openPalette();
    act(() => {
      fireEvent.change(input(), { target: { value: 'toggle sidebar' } });
    });
    act(() => {
      fireEvent.keyDown(input(), { key: 'Enter' });
    });
    expect(useFlux.getState().sidebarOpen).toBe(false);
    expect(useFlux.getState().paletteOpen).toBe(false);
  });

  it('ArrowDown/ArrowUp move the selection with wrap-around', () => {
    openPalette();
    const box = screen.getByRole('listbox');
    const options = [...box.querySelectorAll('[role="option"]')];
    expect(options.length).toBeGreaterThan(1);
    expect(options[0].getAttribute('aria-selected')).toBe('true');
    act(() => {
      fireEvent.keyDown(input(), { key: 'ArrowDown' });
    });
    expect(options[1].getAttribute('aria-selected')).toBe('true');
    act(() => {
      fireEvent.keyDown(input(), { key: 'ArrowUp' });
      fireEvent.keyDown(input(), { key: 'ArrowUp' });
    });
    // Wrapped backwards past the first row.
    expect(options[options.length - 1].getAttribute('aria-selected')).toBe('true');
  });

  it('clicking an option runs it', () => {
    useFlux.setState({ sidebarOpen: true });
    openPalette();
    act(() => {
      fireEvent.click(screen.getByRole('option', { name: /Toggle sidebar/ }));
    });
    expect(useFlux.getState().sidebarOpen).toBe(false);
    expect(useFlux.getState().paletteOpen).toBe(false);
  });
});
