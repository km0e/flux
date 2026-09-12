/**
 * message-list.test.tsx — the jump-to-bottom affordance.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, fireEvent, act } from '@testing-library/react';
import { MessageList } from '../../components/MessageList';
import { useFlux, resetFluxForTest } from '../../core/state';
import { getPane, _resetPanesForTest } from '../../services/panes';

/** A pane parked away from the bottom — the jump affordance's precondition. */
function tallPane(id: string): HTMLElement {
  const pane = getPane(id);
  Object.defineProperty(pane, 'scrollHeight', { value: 2000 });
  Object.defineProperty(pane, 'clientHeight', { value: 500 });
  return pane;
}

describe('MessageList', () => {
  beforeEach(() => {
    resetFluxForTest();
    document.body.innerHTML = '<div id="host"></div>';
    _resetPanesForTest(); // the pane registry outlives the wiped DOM
  });

  it('the button is hidden while at the bottom', () => {
    useFlux.setState({ scrollBtnVisible: false, activeChatId: 'c1' });
    const { container } = render(<MessageList />);
    const btn = container.querySelector('#scroll-bottom-btn') as HTMLButtonElement;
    expect(btn.className).toContain('opacity-0');
  });

  it('the button appears away from the bottom and jumps on click', () => {
    useFlux.setState({ activeChatId: 'c1' });
    // getPane() creates its pane inside #messages-wrap (rendered by the
    // component) — stub the prototype method it calls (jsdom has none).
    const scrollSpy = vi.fn();
    (HTMLElement.prototype as { scrollTo?: unknown }).scrollTo = scrollSpy;
    const { container } = render(<MessageList />);
    // The production visibility path: the pane's scroll listener.
    const pane = tallPane('c1');
    act(() => {
      fireEvent.scroll(pane);
    });
    const btn = container.querySelector('#scroll-bottom-btn') as HTMLButtonElement;
    expect(btn.className).not.toContain('opacity-0');
    fireEvent.click(btn);
    expect(scrollSpy).toHaveBeenCalled();
    expect(useFlux.getState().scrollBtnVisible).toBe(false);
  });

  it('a streaming toggle recomputes the button for the active chat', () => {
    // Stream appends change scrollHeight without scroll events — the round
    // start/end convergence lives in the MessageList subscription (moved
    // out of the store action).
    useFlux.setState({ activeChatId: 'c1' });
    const { container } = render(<MessageList />);
    tallPane('c1');
    act(() => {
      useFlux.getState().setStreaming('c1', true);
    });
    const btn = container.querySelector('#scroll-bottom-btn') as HTMLButtonElement;
    expect(useFlux.getState().scrollBtnVisible).toBe(true);
    expect(btn.className).not.toContain('opacity-0');
  });
});
