import { describe, it, expect, beforeEach, vi, afterEach } from 'vitest';
import {
  splitAndFold,
  ParagraphSplitter,
  renderIncremental,
  renderStableSlice,
} from '../../lib/render';
import {
  _resetReasoningForTest,
  _setReasoningEntryForTest,
  getController,
  disposeController,
  getReasoningEntry,
  type StreamController,
} from '../../services/stream';
import { ensurePane, _resetPanesForTest } from '../../services/panes';
import { useFlux } from '../../core/state';

const stubRender = (s: string) => `<p>${s}</p>`;

describe('splitAndFold', () => {
  it('splits on double newlines', () => {
    const result = splitAndFold('para1\n\npara2\n\npara3');
    expect(result.parts).toEqual(['para1', 'para2']);
    expect(result.tail).toBe('para3');
  });

  it('folds unbalanced fence paragraphs back into tail', () => {
    const result = splitAndFold('before\n\n```\ncode\n\n```\ncode continued');
    expect(result.parts).toEqual(['before']);
    expect(result.tail).toContain('```');
    expect(result.tail).toContain('code continued');
  });

  it('folds until fences balance', () => {
    const result = splitAndFold('a\n\n```\nb\n\nc\n\n```\nd');
    expect(result.tail).toContain('d');
  });

  it('folds a closing fence that lands in its own paragraph', () => {
    const result = splitAndFold('```rust\nlet x=1;\n\n```\n\nmore');
    // The bare closing fence must not be committed as a standalone part
    // (renders as an empty <pre>) — fold until the last part balances.
    expect(result.parts).toEqual([]);
    expect(result.tail).toContain('```');
    expect(result.tail).toContain('more');
  });

  it('returns empty parts for single paragraph', () => {
    const result = splitAndFold('only one paragraph');
    expect(result.parts).toEqual([]);
    expect(result.tail).toBe('only one paragraph');
  });

  it('returns empty strings for empty input', () => {
    const result = splitAndFold('');
    expect(result.parts).toEqual([]);
    expect(result.tail).toBe('');
  });
});

describe('ParagraphSplitter', () => {
  it('matches splitAndFold on structured deltas', () => {
    const splitter = new ParagraphSplitter();
    splitter.push('para1\n\npara2\n\npara3');
    expect(splitter.getParts()).toEqual(['para1', 'para2']);
    expect(splitter.getTail()).toBe('para3');
  });

  it('fast path: a plain delta grows the tail without re-segmenting', () => {
    const splitter = new ParagraphSplitter();
    splitter.push('para1\n\npara2');
    const before = splitter.getParts();
    splitter.push(' more words');
    expect(splitter.getParts()).toBe(before); // same array — structure untouched
    expect(splitter.getTail()).toBe('para2 more words');
  });

  it('commits a paragraph when a separator arrives', () => {
    const splitter = new ParagraphSplitter();
    splitter.push('abcdefghij');
    expect(splitter.getParts()).toEqual([]);
    splitter.push('\n\nnext paragraph');
    expect(splitter.getParts()).toEqual(['abcdefghij']);
    expect(splitter.getTail()).toBe('next paragraph');
  });

  it('folds committed parts back when a fence opens in the tail', () => {
    const splitter = new ParagraphSplitter();
    splitter.push('para\n\ncode intro');
    expect(splitter.getParts()).toEqual(['para']);
    splitter.push('\n\n```js\nconst x = 1;');
    // The open construct straddles the boundary — the earlier part folds
    // back into the tail (splitAndFold semantics).
    expect(splitter.getParts()).toEqual([]);
    expect(splitter.getTail()).toContain('para');
    expect(splitter.getTail()).toContain('```js');
  });

  it('fold-back tail with embedded separators stays equivalent to a full re-split', () => {
    const splitter = new ParagraphSplitter();
    let raw = '';
    for (const chunk of [
      'before\n\n```\ncode', // fold-back: parts=[], tail embeds \n\n
      ' plain tail growth', // fast path blocked (tail embeds \n\n) — slow path
      '\n\ntail paragraph', // structured delta re-segments the embedded tail
    ]) {
      splitter.push(chunk);
      raw += chunk;
      const expected = splitAndFold(raw);
      expect(splitter.getParts()).toEqual(expected.parts);
      expect(splitter.getTail()).toBe(expected.tail);
    }
  });

  it('incremental state always equals a full re-split (structured fuzz)', () => {
    const splitter = new ParagraphSplitter();
    const chunks = [
      'intro text',
      '\n\n```python\ncode line',
      ' more code\n\nstill inside',
      '\n```\n\nclosed now\n\nfinal',
      ' tail growth',
      '\n\nafter ``` tick',
      '\n\nlast paragraph',
    ];
    let raw = '';
    for (const chunk of chunks) {
      splitter.push(chunk);
      raw += chunk;
      const expected = splitAndFold(raw);
      expect(splitter.getParts()).toEqual(expected.parts);
      expect(splitter.getTail()).toBe(expected.tail);
    }
  });
});

describe('renderIncremental', () => {
  it('renders committed paragraphs once each (cache stable across calls)', () => {
    const cache: string[] = [];
    const parts = ['p1', 'p2'];
    const res = renderIncremental(parts, 'tail', cache, stubRender);
    expect(res.committed).toBe(2);
    expect(cache).toEqual(['<p>p1</p>', '<p>p2</p>']);

    // Tail growth: committed paragraphs come from the cache — the only
    // render call is the tail itself (an open-fence tail would not even do
    // that; the fence prefix cache covers it).
    const renderSpy = vi.fn(stubRender);
    const res2 = renderIncremental(parts, 'tail more', cache, renderSpy);
    expect(res2.committed).toBe(2);
    expect(renderSpy).toHaveBeenCalledTimes(1);
    expect(renderSpy).toHaveBeenCalledWith('tail more');
    expect(cache).toEqual(['<p>p1</p>', '<p>p2</p>']);
  });

  it('commits newly completed paragraphs only', () => {
    const cache: string[] = [];
    const parts = ['p1', 'p2', 'p3'];
    renderIncremental(parts.slice(0, 1), 'p2\n\ntail', cache, stubRender);
    expect(cache).toEqual(['<p>p1</p>']);
    renderIncremental(parts, 'tail', cache, stubRender);
    expect(cache).toEqual(['<p>p1</p>', '<p>p2</p>', '<p>p3</p>']);
  });

  it('prunes the cache when parts fold back into the tail', () => {
    const cache: string[] = [];
    renderIncremental(['p1', 'p2'], 'tail', cache, stubRender);
    expect(cache.length).toBe(2);
    // Fold-back: committed shrinks to 1 — cache must follow.
    renderIncremental(['p1'], 'p2\n\n```open', cache, stubRender);
    expect(cache).toEqual(['<p>p1</p>']);
  });

  it('renders an empty tail as empty HTML', () => {
    const cache: string[] = [];
    const res = renderIncremental(['p1'], '', cache, stubRender);
    expect(res.tailHtml).toBe('');
    expect(res.committed).toBe(1);
  });
});

describe('renderStableSlice', () => {
  const stubRender = (s: string) => `<p>${s}</p>`;

  it('renders an unclosed construct as escaped code, types progressively', () => {
    const html = renderStableSlice('intro text ```python\nco', stubRender);
    expect(html).toContain('intro text');
    expect(html).toContain('```python');
    expect(html).toContain('<pre><code>');
  });

  it('reuses the cached fence prefix across increments (O(1) per frame)', () => {
    const renderSpy = vi.fn(stubRender);
    const fenceCache = { key: '', html: '' };
    renderStableSlice('intro\n\n```\ncode', renderSpy, fenceCache);
    const r2 = renderStableSlice('intro\n\n```\ncode more', renderSpy, fenceCache);
    // The prefix (everything before the last ```) is cached — the second
    // frame renders only the escaped tail.
    expect(r2).toContain('code more');
  });

  it('escapes HTML inside the open construct', () => {
    const html = renderStableSlice('a ```<script>', stubRender);
    expect(html).not.toContain('<script>');
    expect(html).toContain('&lt;script&gt;');
  });

  it('renders the slice normally once the construct closes', () => {
    const html = renderStableSlice('intro ```code``` more', stubRender);
    expect(html).toBe('<p>intro ```code``` more</p>');
  });
});

describe('StreamController idle dispose', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    document.body.innerHTML = '<div id="messages-wrap"></div>';
  });

  afterEach(() => {
    vi.useRealTimers();
    document.body.innerHTML = '';
    disposeController('c1');
  });

  it('auto-disposes an idle controller after 30s', () => {
    const ctrl1 = getController('c1');
    expect(controllersRegistryHas('c1')).toBe(true);

    vi.advanceTimersByTime(30_000);

    const ctrl2 = getController('c1');
    expect(ctrl2).not.toBe(ctrl1);
  });

  it('does not dispose an active controller', () => {
    const ctrl = getController('c1');
    ctrl.renderType = 'text';

    vi.advanceTimersByTime(30_000);

    const ctrl2 = getController('c1');
    expect(ctrl2).toBe(ctrl);
  });

  it('dispose() clears the idle timer', () => {
    const ctrl = getController('c1');
    disposeController('c1');

    vi.advanceTimersByTime(30_000);

    const ctrl2 = getController('c1');
    expect(ctrl2).not.toBe(ctrl);
  });

  it('disposes a controller after a finalized text segment', () => {
    const ctrl = getController('c1');
    ctrl.appendText('short');
    ctrl.flushRender(); // boundary — lands pending renders, then finalizes
    expect(ctrl.renderType).toBe('');

    vi.advanceTimersByTime(30_000);

    const ctrl2 = getController('c1');
    expect(ctrl2).not.toBe(ctrl);
  });

  it('re-arms the idle timer while a long render is active, then disposes after it finalizes', () => {
    const ctrl = getController('c1');
    ctrl.renderType = 'text';

    // First fire while still rendering: must not dispose, and must re-arm.
    vi.advanceTimersByTime(30_000);
    expect(ctrl.renderType).toBe('text');
    expect(controllersRegistryHas('c1')).toBe(true);

    // Long stream finally ends — the re-armed timer must still collect it.
    ctrl.renderType = '';
    vi.advanceTimersByTime(30_000);

    const ctrl2 = getController('c1');
    expect(ctrl2).not.toBe(ctrl);
  });
});

/** Helper: check if a controller is in the registry (via Module internals) */
function controllersRegistryHas(chatId: string): boolean {
  const a = getController(chatId);
  const b = getController(chatId);
  return a === b;
}

describe('reasoning block', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    document.body.innerHTML = '<div id="messages-wrap"></div>';
    _resetPanesForTest();
  });

  afterEach(() => {
    vi.useRealTimers();
    document.body.innerHTML = '';
    _resetReasoningForTest();
    disposeController('c1');
    disposeController('idle-test');
  });

  /** Create a reasoning state entry owned by the controller. */
  function setupReasoning(ctrl: StreamController, raw: string): HTMLDivElement {
    const details = document.createElement('details');
    const content = document.createElement('div');
    content.dataset.raw = raw;
    _setReasoningEntryForTest(ctrl.chatId, { el: details, content });
    return content;
  }

  it('renders reasoning content on flush (rAF-coalesced)', () => {
    const ctrl = getController('c1');
    ctrl.appendReasoning('hello world');
    ctrl.flushRenderNow();

    const rs = getReasoningEntry('c1');
    expect(rs).toBeTruthy();
    expect(rs!.el.open).toBe(true);
    expect(rs!.content.textContent).toContain('hello world');
  });

  it('does not rewrite committed reasoning paragraphs when only the tail changes', () => {
    const ctrl = getController('c1');
    ctrl.appendReasoning('para one\n\npara two');
    ctrl.flushRenderNow();

    const parts = document.querySelector('.stream-parts');
    const firstPara = parts?.firstElementChild;
    expect(firstPara?.textContent).toContain('para one');

    ctrl.appendReasoning(' more');
    ctrl.flushRenderNow();
    expect(parts?.firstElementChild).toEqual(firstPara); // same node — no re-parse
  });

  it('closeReasoning removes the state entry (content already rendered)', () => {
    const ctrl = getController('c1');
    ctrl.appendReasoning('hello world');
    ctrl.flushRenderNow();
    const content = getReasoningEntry('c1')!.content;

    ctrl.closeReasoning();

    expect(getReasoningEntry(ctrl.chatId)).toBeUndefined();
    expect(content.textContent).toContain('hello world');
  });

  it('finishes the reasoning block when appendText starts a new segment mid-reasoning', () => {
    const ctrl = getController('c1');
    ctrl.appendReasoning('thinking about it');
    ctrl.flushRenderNow();

    ctrl.appendText('answer'); // flushRenderNow + closeReasoning + resetStreamDom

    expect(getReasoningEntry('c1')).toBeUndefined();
    const summary = document.querySelector('.message.thinking summary');
    expect(summary?.textContent).toMatch(/^Thought for \d+s$/);
  });

  it('lands the pending reasoning render when appendText closes the segment', () => {
    // The reasoning tail is rAF-deferred; the segment-closing text delta must
    // flush it synchronously or the final reasoning chars never reach the DOM.
    const ctrl = getController('c1');
    ctrl.appendReasoning('thinking hard');
    ctrl.appendText('answer');
    expect(getReasoningEntry('c1')).toBeUndefined();

    const summary = document.querySelector('.message.thinking');
    expect(summary?.textContent).toContain('thinking hard');
    // And the text bubble holds the answer (pending text render lands on flush).
    ctrl.flushRenderNow();
    const bubble = document.querySelector('.message.assistant .stream-tail');
    expect(bubble?.textContent).toContain('answer');
  });

  it('skips closeReasoning on a pure-text segment (P2)', () => {
    const ctrl = getController('c1');
    const closeSpy = vi.spyOn(ctrl, 'closeReasoning');
    // Pure-text stream never creates a reasoning block → no per-delta close.
    ctrl.appendText('hello ');
    ctrl.appendText('world');
    expect(closeSpy).not.toHaveBeenCalled();
  });

  it('calls closeReasoning when text follows reasoning in one segment (P2)', () => {
    const ctrl = getController('c1');
    ctrl.appendReasoning('think');
    const closeSpy = vi.spyOn(ctrl, 'closeReasoning');
    ctrl.appendText('answer');
    expect(closeSpy).toHaveBeenCalledTimes(1);
  });

  it('starts a fresh reasoning segment after a tool card (no stale cache bleed)', () => {
    // Regression (thinking→tool→thinking interleave): the paragraph cache is
    // indexed by position — reusing segment 1's cache for segment 2 rendered
    // OLD paragraphs into the new block instead of the new thinking.
    const ctrl = getController('c1');
    ctrl.appendReasoning('old thinking para one\n\nold thinking para two');
    ctrl.addToolCard('t1', 'bash', '{}');
    ctrl.appendReasoning('new thinking after tool');
    ctrl.flushRenderNow();

    const blocks = document.querySelectorAll('.message.thinking');
    // tool_start closes segment 1; segment 2 is a new block.
    expect(blocks.length).toBe(2);

    // DOM order: thinking → tool card → thinking (the pane's empty-state
    // placeholder and other non-message children are skipped).
    const pane = document.querySelector('.chat-pane')!;
    const order = Array.from(pane.children)
      .map((c) => (c as HTMLElement).className as string)
      .filter((cls) => cls.includes('thinking') || cls.includes('tool'))
      .map((cls) => (cls.includes('tool') ? 'tool' : 'thinking'));
    expect(order).toEqual(['thinking', 'tool', 'thinking']);

    // Segment 2 shows ONLY its own content — no stale paragraphs bled in.
    const second = blocks[1] as HTMLDetailsElement;
    expect(second.textContent).toContain('new thinking after tool');
    expect(second.textContent).not.toContain('old thinking');
    // Segment 1 keeps its own content.
    expect(blocks[0].textContent).toContain('old thinking para one');
    // Segment 1 is finalized (summary renamed), segment 2 is live-thinking.
    expect(blocks[0].querySelector('summary')?.textContent).toMatch(/^Thought for \d+s$/);
  });

  it('text after reasoning starts a fresh bubble (timeline order holds)', () => {
    // Regression (text → thinking → text interleave): text₂ used to continue
    // inside the SAME bubble while the thinking block sits after it — the
    // pane then showed [text₁+text₂][thinking] instead of the actual
    // timeline [text₁][thinking][text₂].
    const ctrl = getController('c1');
    ctrl.appendText('first part. ');
    ctrl.appendReasoning('thinking about it');
    ctrl.appendText('second part.');
    ctrl.flushRenderNow();

    const pane = document.querySelector('.chat-pane')!;
    const order = Array.from(pane.children)
      .map((c) => (c as HTMLElement).className as string)
      .filter((cls) => cls.includes('assistant') || cls.includes('thinking'))
      .map((cls) => (cls.includes('thinking') ? 'thinking' : 'text'));
    expect(order).toEqual(['text', 'thinking', 'text']);

    const bubbles = Array.from(pane.querySelectorAll('.message.assistant'));
    expect(bubbles[0].textContent).toContain('first part.');
    expect(bubbles[0].textContent).not.toContain('second part.');
    expect(bubbles[1].textContent).toContain('second part.');
    // The thinking block between them is finalized.
    const summary = document.querySelector('.message.thinking summary');
    expect(summary?.textContent).toMatch(/^Thought for \d+s$/);
  });

  it('dispose clears all stream and reasoning state', () => {
    const ctrl = getController('c1');
    setupReasoning(ctrl, 'hello world');
    ctrl.ensureStream();
    ctrl.dispose();

    expect(useFlux.getState().streaming[ctrl.chatId]).toBeUndefined();
    expect(getReasoningEntry(ctrl.chatId)).toBeUndefined();
  });
});

describe('scroll follow (stick state machine)', () => {
  /** Controllable layout stub: bottom = scrollTop 892+; the setter dispatches upward scroll events. */
  function stubLayout(pane: HTMLElement) {
    let top = 900;
    Object.defineProperty(pane, 'scrollTop', {
      configurable: true,
      get: () => top,
      set: (v: number) => {
        top = v;
        pane.dispatchEvent(new Event('scroll'));
      },
    });
    Object.defineProperty(pane, 'scrollHeight', {
      configurable: true,
      get: () => 1000,
    });
    Object.defineProperty(pane, 'clientHeight', {
      configurable: true,
      get: () => 100,
    });
    return {
      get top() {
        return top;
      },
      set top(v: number) {
        top = v;
      },
    };
  }

  /** rAF capture stub: render frames and follow frames share it. Running a
   * render frame makes scheduleFollow queue another follow frame — flushAll
   * loops until stable (render frame → follow frame). */
  function stubRaf() {
    const cbs: (FrameRequestCallback | null)[] = [];
    vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => {
      cbs.push(cb);
      return cbs.length;
    });
    vi.stubGlobal('cancelAnimationFrame', (id: number) => {
      cbs[id - 1] = null;
    });
    return {
      get scheduled() {
        return cbs.filter((c) => c !== null).length;
      },
      flushAll() {
        let guard = 5;
        while (cbs.some((c) => c !== null) && guard-- > 0) {
          for (const cb of cbs.splice(0)) cb?.(0);
        }
      },
    };
  }

  beforeEach(() => {
    document.body.innerHTML = '<div id="messages-wrap"></div>';
    _resetPanesForTest();
    useFlux.setState({ activeChatId: 'c1' });
    useFlux.setState({ streaming: {} });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    disposeController('c1');
    document.body.innerHTML = '';
  });

  it('coalesces N deltas into one render frame, then follows once', () => {
    const ctrl = getController('c1');
    const pane = ensurePane('c1');
    stubLayout(pane);
    const raf = stubRaf();

    ctrl.appendText('a');
    ctrl.appendText('b');
    ctrl.appendText('c');

    // P0-1: N deltas → ONE pending render callback (plus at most ensureStream's
    // follow frame — never one render per delta).
    expect(raf.scheduled).toBeLessThanOrEqual(2);

    raf.flushAll();
    // One render covered ALL deltas; the follow caught up.
    const body = (ctrl as unknown as { streamBody: HTMLDivElement | null }).streamBody;
    expect(body?.textContent).toContain('a');
    expect(body?.textContent).toContain('c');
    expect((pane as unknown as { scrollTop: number }).scrollTop).toBe(1000);
  });

  it('detaches on scroll-up and never yanks the user back', () => {
    const ctrl = getController('c1');
    const pane = ensurePane('c1');
    const layout = stubLayout(pane);
    const raf = stubRaf();

    ctrl.appendText('a');
    raf.flushAll();

    // Scrolling up: detach + cancel the scheduled follow frame.
    layout.top = 300;
    pane.dispatchEvent(new Event('scroll'));

    ctrl.appendText('b');
    raf.flushAll();
    // detached — deltas rendered but no programmatic scroll
    expect(layout.top).toBe(300);
  });

  it('keeps the scroll button visible while the user reads above during streaming', () => {
    // Regression: the old implementation cleared scrollBtnVisible=false per
    // delta unconditionally, fighting the stick machine — while the user reads
    // above (detached, no programmatic scroll, no scroll events) the button
    // flickered away. Visibility converges on the scroll event + setStreaming.
    const ctrl = getController('c1');
    const pane = ensurePane('c1');
    const layout = stubLayout(pane);
    const raf = stubRaf();
    useFlux.getState().setStreaming('c1', true);

    ctrl.appendText('a');
    raf.flushAll();

    // Scrolling up: the pane's scroll listener shows the button (streaming && !nearBottom).
    layout.top = 300;
    pane.dispatchEvent(new Event('scroll'));
    expect(useFlux.getState().scrollBtnVisible).toBe(true);

    // Subsequent deltas: must NOT clear the button's visibility (the user is still reading above).
    ctrl.appendText('b');
    raf.flushAll();
    expect(useFlux.getState().scrollBtnVisible).toBe(true);
  });

  it('content growth never detaches a following pane (burst regression)', () => {
    // Fixed regression: after a render burst the scroll event carries a stale
    // scrollTop vs the grown scrollHeight, so the old position-based
    // nearBottom(8) check failed → detached. Now an attached state only
    // detaches on upward scroll — content growth is never a detach signal.
    const ctrl = getController('c1');
    const pane = ensurePane('c1');
    let height = 1000;
    let top = 900;
    Object.defineProperty(pane, 'scrollTop', {
      configurable: true,
      get: () => top,
      set: (v: number) => {
        top = v;
        pane.dispatchEvent(new Event('scroll'));
      },
    });
    Object.defineProperty(pane, 'scrollHeight', {
      configurable: true,
      get: () => height,
    });
    Object.defineProperty(pane, 'clientHeight', {
      configurable: true,
      get: () => 100,
    });
    const raf = stubRaf();

    ctrl.appendText('a');
    raf.flushAll();

    // Burst: content grows 3× (a scrollHeight change dispatches no scroll event — matching browsers).
    height = 3000;
    // The render-triggered scroll event carries a stale top; the attached state must ignore position checks.
    pane.dispatchEvent(new Event('scroll'));
    ctrl.appendText('b');
    raf.flushAll();
    // the follow frame caught up to the grown content
    expect(top).toBe(3000);

    // Follow intact: subsequent deltas keep following.
    ctrl.appendText('c');
    raf.flushAll();
    expect(top).toBe(3000);
  });

  it('re-attaches when scrolled back near the bottom', () => {
    const ctrl = getController('c1');
    const pane = ensurePane('c1');
    const layout = stubLayout(pane);
    const raf = stubRaf();

    // Activate the follow (attach the listener) first, then simulate having scrolled up.
    ctrl.appendText('warm');
    raf.flushAll();
    layout.top = 300;
    pane.dispatchEvent(new Event('scroll'));
    ctrl.appendText('a');
    raf.flushAll();
    // detached
    expect(layout.top).toBe(300);

    // Scrolling down into the bottom 96px (bottom-96 = 804) → re-attach.
    layout.top = 850;
    pane.dispatchEvent(new Event('scroll'));
    ctrl.appendText('b');
    raf.flushAll();
    // back near bottom — follow resumed
    expect(layout.top).toBe(1000);
  });
});

describe('stream segmentation', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    document.body.innerHTML = '<div id="messages-wrap"></div>';
    _resetPanesForTest();
  });

  afterEach(() => {
    vi.useRealTimers();
    disposeController('c1');
    document.body.innerHTML = '';
    _resetPanesForTest();
  });

  it('a tool card closes the segment: old bubble finalizes, post-tool text starts fresh', () => {
    const ctrl = getController('c1');
    ctrl.appendText('a');
    ctrl.addToolCard('tc1', 'bash', 'echo hi');
    ctrl.appendText('b');

    const raw = (ctrl as unknown as { rawText: string }).rawText;
    expect(raw).toBe('b'); // fresh bubble holds only post-tool text
    expect(document.querySelectorAll('.message.assistant').length).toBe(2);

    const liveBubbles = document.querySelectorAll('.message.assistant.live');
    expect(liveBubbles.length).toBe(1); // only the new bubble stays live
    const oldBubble = document.querySelector('.message.assistant:not(.live)');
    expect(oldBubble?.textContent).toContain('a');
  });

  it('a late delta after finalization starts a fresh bubble (retry path)', () => {
    const ctrl = getController('c1');
    ctrl.appendText('a');
    ctrl.flushRender(); // boundary — lands pending renders, then finalizes
    ctrl.appendText('b');

    const raw = (ctrl as unknown as { rawText: string }).rawText;
    expect(raw).toBe('b'); // fresh bubble holds only post-finalize text
    expect(document.querySelectorAll('.message.assistant').length).toBe(2);
  });

  it('dispose keeps the already-rendered DOM (no content loss)', () => {
    const ctrl = getController('c1');
    ctrl.appendText('hello world');
    ctrl.flushRender();
    disposeController('c1');

    const tail = document.querySelector('.stream-tail');
    expect(tail?.textContent).toContain('hello world');
  });

  it('renders an unclosed code construct as escaped text (stable, streams fully)', () => {
    const ctrl = getController('c1');
    ctrl.appendText('intro text ```python\ncode');
    ctrl.flushRenderNow();

    const tail = document.querySelector('.stream-tail');
    expect(tail?.textContent).toContain('intro text');
    expect(tail?.textContent).toContain('```python');
    expect(tail?.querySelector('pre code')).toBeTruthy();

    ctrl.appendText(' line\n``` done');
    ctrl.flushRenderNow();
    expect(tail?.textContent).toContain('done');
  });

  it('renders committed paragraphs into per-paragraph wrappers (append-only)', () => {
    const ctrl = getController('c1');
    ctrl.appendText('abcdefghij\n\nnext');
    ctrl.flushRenderNow();

    // Committed paragraph in its own stable wrapper; the trailing paragraph
    // stays in the live tail.
    const parts = document.querySelector('.stream-parts')!;
    const wrappers = parts.querySelectorAll('.stream-part');
    expect(wrappers.length).toBe(1);
    expect(wrappers[0].textContent).toContain('abcdefghij');
    const tailText = document.querySelector('.stream-tail')?.textContent ?? '';
    expect(tailText).toContain('next');
  });

  it('does not rebuild committed paragraph nodes when only the tail changes (P0-2)', () => {
    const ctrl = getController('c1');
    ctrl.appendText('para one\n\npara two');
    ctrl.flushRenderNow();

    const parts = document.querySelector('.stream-parts');
    const firstPara = parts?.firstElementChild;
    expect(firstPara?.textContent).toContain('para one');

    // A tail-only delta must not touch the parts region — committed nodes
    // keep their DOM identity (and their highlight runs exactly once).
    ctrl.appendText(' more');
    ctrl.flushRenderNow();
    expect(parts?.firstElementChild).toEqual(firstPara);
    expect(parts?.children.length).toBe(1);
  });

  it('a paragraph commit appends a new wrapper without rebuilding earlier ones (P0-2)', () => {
    const ctrl = getController('c1');
    ctrl.appendText('para one\n\npara two\n\npara three');
    ctrl.flushRenderNow();

    const parts = document.querySelector('.stream-parts')!;
    expect(parts.querySelectorAll('.stream-part').length).toBe(2);
    const first = parts.firstElementChild;

    ctrl.appendText('\n\npara four');
    ctrl.flushRenderNow();
    expect(parts.querySelectorAll('.stream-part').length).toBe(3);
    expect(parts.firstElementChild).toEqual(first); // append-only — no rebuild
  });

  it('flushRender lands pending renders and finalizes the segment (sync)', () => {
    const ctrl = getController('c1');
    ctrl.appendText('hello world');
    ctrl.flushRender();

    // Finalized synchronously — pending renders landed before the finalize.
    expect(ctrl.streamDiv?.classList.contains('live')).toBe(false);
    const self = ctrl as unknown as { streamBody: HTMLDivElement | null };
    expect(self.streamBody!.textContent).toContain('hello world');
  });

  it('a 100-delta burst runs ONE incremental render and renders everything (P0-1)', () => {
    // The stutter regression: N deltas must produce ONE incremental render,
    // not N synchronous ones — a burst can never exceed the frame budget.
    vi.stubGlobal('requestAnimationFrame', (_cb: FrameRequestCallback) => 1);
    vi.stubGlobal('cancelAnimationFrame', () => {});
    try {
      const ctrl = getController('c1');
      const renderSpy = vi.spyOn(ctrl as unknown as { renderText: () => void }, 'renderText');
      for (let i = 0; i < 100; i++) {
        ctrl.appendText(`delta ${i} word\n`);
      }
      expect(renderSpy).not.toHaveBeenCalled(); // nothing rendered synchronously

      ctrl.flushRenderNow();
      expect(renderSpy).toHaveBeenCalledTimes(1); // coalesced into ONE render
      const body = (ctrl as unknown as { streamBody: HTMLDivElement | null }).streamBody;
      expect(body!.textContent).toContain('delta 99 word');
    } finally {
      vi.unstubAllGlobals();
    }
  });
});

describe('tool call preview (tool_preview → pending card → tool_start upgrade)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    document.body.innerHTML = '<div id="messages-wrap"></div>';
    _resetPanesForTest();
  });

  afterEach(() => {
    vi.useRealTimers();
    disposeController('c1');
    document.body.innerHTML = '';
    _resetPanesForTest();
  });

  it('addToolPreview creates a dimmed pending card with a live args tail', () => {
    const ctrl = getController('c1');
    ctrl.addToolPreview('call_1', 'bash');
    ctrl.appendToolPreviewArgs('call_1', '{"cmd":"ec');
    ctrl.appendToolPreviewArgs('call_1', 'ho hi"}');

    const card = document.querySelector<HTMLElement>('[data-tool-call-id="call_1"]')!;
    expect(card.classList.contains('pending')).toBe(true);
    expect(card.classList.contains('tool')).toBe(true);
    // Full raw accumulates in the dataset; the tail slice renders.
    expect(card.dataset.rawArgs).toBe('{"cmd":"echo hi"}');
    const tail = card.querySelector('.tool-args-live')!;
    expect(tail.textContent).toBe('{"cmd":"echo hi"}');
    expect(card.querySelector('.tool-status')!.textContent).toContain('preparing');
  });

  it('the args tail renders only the last 200 chars', () => {
    const ctrl = getController('c1');
    ctrl.addToolPreview('call_1', 'write_file');
    ctrl.appendToolPreviewArgs('call_1', 'x'.repeat(350));

    const card = document.querySelector<HTMLElement>('[data-tool-call-id="call_1"]')!;
    expect(card.dataset.rawArgs).toHaveLength(350);
    const tail = card.querySelector('.tool-args-live')!;
    expect(tail.textContent).toBe('…' + 'x'.repeat(200));
  });

  it('a delta for an unknown call id is a no-op', () => {
    const ctrl = getController('c1');
    ctrl.appendToolPreviewArgs('ghost', 'args');
    expect(document.querySelectorAll('[data-tool-call-id="ghost"]').length).toBe(0);
  });

  it('tool_start upgrades the pending card IN PLACE (no duplicate)', () => {
    const ctrl = getController('c1');
    ctrl.addToolPreview('call_1', 'bash');
    ctrl.appendToolPreviewArgs('call_1', '{"cmd":"echo hi"}');
    ctrl.addToolCard('call_1', 'bash', '{"cmd":"echo hi"}');

    const cards = document.querySelectorAll('[data-tool-call-id="call_1"]');
    expect(cards.length).toBe(1);
    const card = cards[0]!;
    expect(card.classList.contains('pending')).toBe(false);
    expect(card.classList.contains('running')).toBe(true);
    expect(card.querySelector('.tool-args-live')).toBeNull();
    // Card order in the pane is unchanged (upgraded in place).
    const pane = document.querySelector('.chat-pane')!;
    expect(pane.lastElementChild).toBe(card);
  });

  it('duplicate identity previews are idempotent (no double card)', () => {
    const ctrl = getController('c1');
    ctrl.addToolPreview('call_1', 'bash');
    ctrl.addToolPreview('call_1', 'bash');
    expect(document.querySelectorAll('[data-tool-call-id="call_1"]').length).toBe(1);
  });

  it('a preview without args renders an empty tail (hidden by CSS)', () => {
    const ctrl = getController('c1');
    ctrl.addToolPreview('call_1', 'bash');
    const card = document.querySelector<HTMLElement>('[data-tool-call-id="call_1"]')!;
    const tail = card.querySelector('.tool-args-live')!;
    expect(tail.textContent).toBe('');
  });
});
