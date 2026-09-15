/**
 * title.test.ts — the document title composition: the pure composer's
 * format matrix, the store attention counter (hidden-tab gate + clear),
 * and syncTitle's store read.
 */
import { describe, it, expect, beforeEach } from 'vitest';
import { composeTitle, syncTitle } from '../../services/title';
import { useFlux, resetFluxForTest } from '../../core/state';

function setHidden(hidden: boolean): void {
  Object.defineProperty(document, 'hidden', { value: hidden, configurable: true });
}

describe('composeTitle (the format matrix)', () => {
  it('bare app with no conversation and nothing happening', () => {
    expect(composeTitle({ streaming: false, events: 0 })).toBe('Flux');
  });

  it('names the active conversation, app identity anchored last', () => {
    expect(composeTitle({ streaming: false, name: 'Support', events: 0 })).toBe(
      'Support — Flux',
    );
  });

  it('marks a streaming round', () => {
    expect(composeTitle({ streaming: true, name: 'Support', events: 0 })).toBe(
      'Support — working… — Flux',
    );
  });

  it('prefixes background attention with the count', () => {
    expect(composeTitle({ streaming: true, name: 'Support', events: 2 })).toBe(
      '(2) Support — working… — Flux',
    );
  });

  it('counts survive without a conversation', () => {
    expect(composeTitle({ streaming: false, events: 1 })).toBe('(1) Flux');
  });
});

describe('backgroundEvents counter', () => {
  beforeEach(() => {
    resetFluxForTest();
    setHidden(false);
  });

  it('counts ONLY while the tab is hidden', () => {
    useFlux.getState().bumpBackgroundEvents();
    expect(useFlux.getState().backgroundEvents).toBe(0); // visible — no count

    setHidden(true);
    useFlux.getState().bumpBackgroundEvents();
    useFlux.getState().bumpBackgroundEvents();
    expect(useFlux.getState().backgroundEvents).toBe(2);
  });

  it('clears on visibility restore', () => {
    setHidden(true);
    useFlux.getState().bumpBackgroundEvents();
    expect(useFlux.getState().backgroundEvents).toBe(1);
    useFlux.getState().clearBackgroundEvents();
    expect(useFlux.getState().backgroundEvents).toBe(0);
  });
});

describe('syncTitle (the store read)', () => {
  beforeEach(() => {
    resetFluxForTest();
    setHidden(false);
  });

  it('writes the composed title from the live store', () => {
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'Support', createdAt: 1, active: false, workdir: '', provider: '', model: '' },
      ],
      activeChatId: 'c1',
      streaming: { c1: true },
    });
    setHidden(true);
    useFlux.getState().bumpBackgroundEvents();
    syncTitle();
    expect(document.title).toBe('(1) Support — working… — Flux');
  });

  it('falls back to the app name with no conversation', () => {
    syncTitle();
    expect(document.title).toBe('Flux');
  });
});
