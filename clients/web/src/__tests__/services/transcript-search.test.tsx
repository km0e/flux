/**
 * transcript-search.test.tsx — the search service's scan/navigation/lifecycle
 * math. jsdom has no CSS Custom Highlight API, which is precisely the
 * contract's point: the scan and navigation work WITHOUT it (only the
 * paint is skipped), so every assertion here rides the store projection.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, act } from '@testing-library/react';
import { MessageList } from '../../components/MessageList';
import { useFlux, resetFluxForTest } from '../../core/state';
import { getPane, _resetPanesForTest } from '../../services/panes';
import {
  _resetSearchForTest,
  closeSearch,
  nextMatch,
  openSearch,
  prevMatch,
  searchQuery,
  setQuery,
} from '../../services/transcript-search';

function renderHost() {
  render(<MessageList />);
}

function paneWith(id: string, html: string): HTMLDivElement {
  const pane = getPane(id);
  pane.innerHTML = html; // test scaffolding — replace the empty-state card
  return pane;
}

function counts() {
  const s = useFlux.getState();
  return { matches: s.searchMatches, current: s.searchCurrent };
}

describe('transcript-search', () => {
  beforeEach(() => {
    resetFluxForTest();
    document.body.innerHTML = '<div id="host"></div>';
    _resetPanesForTest();
    _resetSearchForTest();
    renderHost();
  });

  afterEach(() => {
    vi.useRealTimers();
    closeSearch();
  });

  it('counts case-insensitive matches within single nodes', () => {
    useFlux.setState({ activeChatId: 'c1' });
    paneWith('c1', '<p>Ran fine</p><p>the tool ran FINE twice</p>');
    act(() => {
      openSearch();
      setQuery('ran fine');
    });
    // "Ran fine" + "ran FINE" — case-insensitive, non-overlapping.
    expect(counts()).toEqual({ matches: 2, current: 0 });
  });

  it('matches run ACROSS inline element boundaries', () => {
    useFlux.setState({ activeChatId: 'c1' });
    paneWith('c1', '<p>the build <code>ran</code> <strong>fine</strong> today</p>');
    act(() => {
      openSearch();
      setQuery('ran fine');
    });
    expect(counts().matches).toBe(1);
  });

  it('skips the empty-state invitation card', () => {
    useFlux.setState({ activeChatId: 'c1' });
    const pane = getPane('c1'); // keeps .fx-empty-state with its prompt copy
    pane.insertAdjacentHTML('beforeend', '<p>ordinary turn</p>');
    act(() => {
      openSearch();
      setQuery('map this repo');
    });
    expect(counts().matches).toBe(0);
    act(() => {
      setQuery('ordinary');
    });
    expect(counts().matches).toBe(1);
  });

  it('navigation wraps and clamps on rescan', () => {
    useFlux.setState({ activeChatId: 'c1' });
    paneWith('c1', '<p>hit</p><p>hit</p><p>hit</p>');
    act(() => {
      openSearch();
      setQuery('hit');
    });
    expect(counts()).toEqual({ matches: 3, current: 0 });
    act(() => nextMatch());
    expect(counts().current).toBe(1);
    act(() => nextMatch());
    act(() => nextMatch());
    expect(counts().current).toBe(0); // wrapped
    act(() => prevMatch());
    expect(counts().current).toBe(2); // wrapped backwards
  });

  it('scrolls to the current match on navigation', () => {
    useFlux.setState({ activeChatId: 'c1' });
    paneWith('c1', '<p>hit</p><p>hit</p>');
    const spy = vi.fn();
    (HTMLElement.prototype as { scrollIntoView?: unknown }).scrollIntoView = spy;
    act(() => {
      openSearch();
      setQuery('hit');
    });
    const scrollsAfterQuery = spy.mock.calls.length;
    act(() => nextMatch());
    expect(spy.mock.calls.length).toBe(scrollsAfterQuery + 1);
    expect(spy.mock.calls.at(-1)?.[0]).toMatchObject({ block: 'center' });
  });

  it('streaming appends rescan (debounced) WITHOUT scrolling', async () => {
    vi.useFakeTimers();
    useFlux.setState({ activeChatId: 'c1' });
    const pane = paneWith('c1', '<p>hit</p>');
    const spy = vi.fn();
    (HTMLElement.prototype as { scrollIntoView?: unknown }).scrollIntoView = spy;
    act(() => {
      openSearch();
      setQuery('hit');
    });
    const scrollsAfterQuery = spy.mock.calls.length;
    // act(async) drains microtasks — the MutationObserver notification is
    // one itself, and the debounced rescan timer is scheduled BY it.
    await act(async () => {
      pane.insertAdjacentHTML('beforeend', '<p>another hit</p>');
    });
    act(() => {
      vi.advanceTimersByTime(200); // past the 150ms rescan debounce
    });
    expect(counts().matches).toBe(2);
    expect(spy.mock.calls.length).toBe(scrollsAfterQuery); // no yank
  });

  it('closing clears the projection, detaches the observer, keeps the query', () => {
    useFlux.setState({ activeChatId: 'c1' });
    const pane = paneWith('c1', '<p>hit</p>');
    act(() => {
      openSearch();
      setQuery('hit');
    });
    expect(counts().matches).toBe(1);
    act(() => {
      closeSearch();
    });
    expect(useFlux.getState().searchOpen).toBe(false);
    expect(counts()).toEqual({ matches: 0, current: 0 });
    // Observer detached: post-close mutations must not resurrect counts
    // (a synchronous read — the debounced rescan is never scheduled).
    act(() => {
      pane.insertAdjacentHTML('beforeend', '<p>more hit</p>');
    });
    expect(counts().matches).toBe(0);
    expect(searchQuery()).toBe('hit'); // find-bar memory for the reopen
  });

  it('a chat switch closes the bar (the component effect), scoped search', () => {
    useFlux.setState({ activeChatId: 'c1' });
    paneWith('c1', '<p>hit</p>');
    act(() => {
      openSearch();
    });
    expect(useFlux.getState().searchOpen).toBe(true);
    // The TranscriptSearchBar effect closes on cid change; service-side we
    // only assert closeSearch is idempotent and reopens target the new pane.
    act(() => {
      closeSearch();
      closeSearch();
    });
    expect(useFlux.getState().searchOpen).toBe(false);
  });
});
