/**
 * transcript-search-bar.test.tsx — the floating search bar: the match
 * projection renders, Enter/Shift+Enter drive the service navigation,
 * close clears, and a chat switch closes the bar (scoped search).
 */
import { describe, it, expect, beforeEach } from 'vitest';
import { render, fireEvent, act } from '@testing-library/react';
import { MessageList } from '../../components/MessageList';
import { useFlux, resetFluxForTest } from '../../core/state';
import { getPane, _resetPanesForTest } from '../../services/panes';
import {
  _resetSearchForTest,
  openSearch,
  setQuery,
} from '../../services/transcript-search';

describe('TranscriptSearchBar', () => {
  beforeEach(() => {
    resetFluxForTest();
    document.body.innerHTML = '<div id="host"></div>';
    _resetPanesForTest();
    _resetSearchForTest();
  });

  it('renders only while open, with the counter and named controls', () => {
    const { container } = render(<MessageList />);
    expect(container.querySelector('#transcript-search')).toBeNull();
    useFlux.setState({ activeChatId: 'c1' });
    getPane('c1').innerHTML = '<p>hit</p><p>hit</p>';
    act(() => {
      openSearch();
      setQuery('hit');
    });
    const bar = container.querySelector('#transcript-search');
    expect(bar).not.toBeNull();
    expect(bar?.getAttribute('role')).toBe('search');
    expect(bar?.textContent).toContain('1/2');
    expect(container.querySelector('button[aria-label="Next match"]')).not.toBeNull();
    expect(container.querySelector('button[aria-label="Previous match"]')).not.toBeNull();
    expect(container.querySelector('button[aria-label="Close search"]')).not.toBeNull();
  });

  it('shows 0/0 for no matches and updates through navigation', () => {
    const { container } = render(<MessageList />);
    useFlux.setState({ activeChatId: 'c1' });
    getPane('c1').innerHTML = '<p>hit</p>';
    act(() => {
      openSearch();
      setQuery('miss');
    });
    expect(container.querySelector('#transcript-search')?.textContent).toContain('0/0');
    act(() => {
      setQuery('hit');
    });
    expect(container.querySelector('#transcript-search')?.textContent).toContain('1/1');
  });

  it('typing in the input drives the service; Enter navigates forward', () => {
    const { container } = render(<MessageList />);
    useFlux.setState({ activeChatId: 'c1' });
    getPane('c1').innerHTML = '<p>alpha</p><p>alpha</p><p>alpha</p>';
    act(() => {
      openSearch();
    });
    const input = container.querySelector<HTMLInputElement>('#transcript-search input');
    expect(input).not.toBeNull();
    act(() => {
      fireEvent.change(input!, { target: { value: 'alpha' } });
    });
    expect(useFlux.getState().searchMatches).toBe(3);
    act(() => {
      fireEvent.keyDown(input!, { key: 'Enter' });
    });
    expect(useFlux.getState().searchCurrent).toBe(1);
    act(() => {
      fireEvent.keyDown(input!, { key: 'Enter', shiftKey: true });
      fireEvent.keyDown(input!, { key: 'Enter', shiftKey: true });
    });
    expect(useFlux.getState().searchCurrent).toBe(2); // wrapped backwards
  });

  it('the close button clears the store projection', () => {
    const { container } = render(<MessageList />);
    useFlux.setState({ activeChatId: 'c1' });
    getPane('c1').innerHTML = '<p>hit</p>';
    act(() => {
      openSearch();
      setQuery('hit');
    });
    act(() => {
      fireEvent.click(
        container.querySelector('button[aria-label="Close search"]') as HTMLButtonElement,
      );
    });
    expect(useFlux.getState().searchOpen).toBe(false);
    expect(useFlux.getState().searchMatches).toBe(0);
  });

  it('a chat switch closes the bar', () => {
    const { container, rerender } = render(<MessageList />);
    useFlux.setState({ activeChatId: 'c1' });
    getPane('c1').innerHTML = '<p>hit</p>';
    act(() => {
      openSearch();
      setQuery('hit');
    });
    expect(container.querySelector('#transcript-search')).not.toBeNull();
    act(() => {
      useFlux.setState({ activeChatId: 'c2' });
    });
    rerender(<MessageList />);
    expect(useFlux.getState().searchOpen).toBe(false);
    expect(container.querySelector('#transcript-search')).toBeNull();
  });
});
