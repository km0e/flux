import { describe, it, expect, vi, afterEach } from 'vitest';
import { scheduleFollow, scrollPaneToBottom } from '../../lib/follow';

/**
 * Stick state machine semantics around the bottom-clamp immunity.
 *
 * jsdom does no layout: scrollHeight/clientHeight read 0, scrollTop writes
 * fire no scroll event. The tests therefore drive the machine through
 * mocked scroll metrics + manually dispatched scroll events, reproducing
 * the EXACT event sequences the browser produces:
 *   - a clamp: content shrinks while pinned at the bottom → the browser
 *     pulls scrollTop down to scrollHeight - clientHeight and dispatches
 *     a scroll event that reads as "goingUp" (the bug: it detached the
 *     stick state, killing the follow for the rest of the stream);
 *   - a real user scroll-up: goingUp AND off the bottom → must detach.
 */

/** A scroll box with controllable metrics. */
function makeScrollBox(scrollHeight = 1000, clientHeight = 100) {
  const el = document.createElement('div');
  const m = { scrollTop: 0, scrollHeight, clientHeight };
  Object.defineProperty(el, 'scrollTop', {
    configurable: true,
    get: () => m.scrollTop,
    set: (v: number) => {
      m.scrollTop = v;
    },
  });
  Object.defineProperty(el, 'scrollHeight', { configurable: true, get: () => m.scrollHeight });
  Object.defineProperty(el, 'clientHeight', { configurable: true, get: () => m.clientHeight });
  return { el, m };
}

/** Capture rAF callbacks instead of running them — the tests decide when
 * (and whether) a scheduled follow frame executes. */
function captureRaf() {
  const calls: FrameRequestCallback[] = [];
  const spy = vi
    .spyOn(window, 'requestAnimationFrame')
    .mockImplementation((cb: FrameRequestCallback) => {
      calls.push(cb);
      return calls.length;
    });
  return { calls, spy };
}

/** Pin the fixture at the bottom and let the machine materialize
 * (stick = true), as after forceFollow / a pinned stream. */
function pinToBottom(m: ReturnType<typeof makeScrollBox>['m'], el: HTMLElement) {
  m.scrollTop = m.scrollHeight - m.clientHeight; // 900: exactly at the bottom
  scrollPaneToBottom(el, 50); // near bottom → stick = true (the write over-scrolls; browsers clamp)
  el.dispatchEvent(new Event('scroll')); // the write's own event: moving down, lastTop syncs
}

describe('stick state machine — bottom-clamp immunity', () => {
  afterEach(() => {
    vi.restoreAllMocks();
    document.body.innerHTML = '';
  });

  it('keeps the follow through a bottom-clamp scroll event (shrink while pinned)', () => {
    const { el, m } = makeScrollBox();
    const { calls, spy } = captureRaf();
    pinToBottom(m, el);

    // A boundary streaming render SHRINKS the content while pinned (a
    // negative-net reflow: paragraph commit differences, code-fence
    // fold-back): the browser clamps scrollTop down to the new bottom and
    // dispatches a scroll event. This event is goingUp but NOT the user
    // leaving — the stick state must survive it, or every later delta's
    // scheduleFollow silently no-ops behind the stick gate.
    m.scrollHeight = 950;
    m.scrollTop = 850; // the clamp: exactly the new bottom (950 - 100)
    el.dispatchEvent(new Event('scroll'));

    // The next delta's follow must still be armed — and its write must land.
    scheduleFollow(el);
    expect(calls).toHaveLength(1);
    calls[0]!(0);
    expect(m.scrollTop).toBe(950); // wrote to the new bottom (the bug left it at 850)
    spy.mockRestore();
  });

  it('still detaches on a real user scroll-up (off the bottom)', () => {
    const { el, m } = makeScrollBox();
    const { calls, spy } = captureRaf();
    pinToBottom(m, el);

    // The user scrolls up 140px — goingUp AND away from the bottom.
    m.scrollTop = 760;
    el.dispatchEvent(new Event('scroll'));

    scheduleFollow(el);
    expect(calls).toHaveLength(0); // detached: no follow frame scheduled
    spy.mockRestore();
  });

  it('re-attaches when the user scrolls back down into the enter window', () => {
    const { el, m } = makeScrollBox();
    const { calls, spy } = captureRaf();
    pinToBottom(m, el);

    m.scrollTop = 760; // user up → detach
    el.dispatchEvent(new Event('scroll'));

    m.scrollTop = 920; // user down into the 96px enter window → re-attach
    el.dispatchEvent(new Event('scroll'));

    scheduleFollow(el);
    expect(calls).toHaveLength(1);
    calls[0]!(0);
    expect(m.scrollTop).toBe(1000);
    spy.mockRestore();
  });

  it('tolerates a ≤2px first tick as a possible clamp, detaching on the next', () => {
    const { el, m } = makeScrollBox();
    const { calls, spy } = captureRaf();
    pinToBottom(m, el);

    // A 1px wheel tick sits inside the clamp tolerance — tolerated (the
    // deliberate cost of clamp immunity; a real scroll-up continues).
    m.scrollTop = 899;
    el.dispatchEvent(new Event('scroll'));
    scheduleFollow(el);
    expect(calls).toHaveLength(1);
    calls[0]!(0);

    // The next tick leaves the tolerance band → detach, as intended.
    m.scrollTop = 890;
    el.dispatchEvent(new Event('scroll'));
    calls.length = 0;
    scheduleFollow(el);
    expect(calls).toHaveLength(0);
    spy.mockRestore();
  });
});
