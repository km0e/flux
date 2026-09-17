import { describe, it, expect, vi } from 'vitest';
import {
  createMessageBubble,
  createReasoningBlock,
  createToolCard,
  setToolCardResult,
  markToolCardComplete,
  createErrorBubble,
  createTypingIndicator,
  createCopyButton,
  isNearBottom,
  scrollPaneToBottom,
  toolSummary,
  followExpansion,
  followExpansionFrom,
  classifyToolResult,
} from '../../lib/dom';

describe('createCopyButton', () => {
  it('creates a button with "Copy" text', () => {
    const btn = createCopyButton(() => 'test');
    expect(btn.textContent).toBe('Copy');
    expect(btn.className).toBe('copy-btn');
  });

  it('writes to clipboard on click', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', {
      value: { writeText },
      configurable: true,
    });
    const btn = createCopyButton(() => 'hello');
    btn.click();
    expect(writeText).toHaveBeenCalledWith('hello');
  });
});

describe('createMessageBubble', () => {
  it('creates a user bubble with text', () => {
    const { el, body } = createMessageBubble({ role: 'user', text: 'hello' });
    expect(el.className).toContain('user');
    expect(body.textContent).toBe('hello');
  });

  it('user bubbles carry the copy affordance; clicking copies the text', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', {
      value: { writeText },
      configurable: true,
    });
    const { el } = createMessageBubble({ role: 'user', text: 'hello\nworld' });
    const btn = el.querySelector<HTMLButtonElement>('.msg-copy');
    expect(btn).not.toBeNull();
    expect(btn?.getAttribute('aria-label')).toBe('Copy message');
    btn!.click();
    expect(writeText).toHaveBeenCalledWith('hello\nworld');
    await vi.waitFor(() => expect(btn?.classList.contains('ok')).toBe(true));
  });

  it('user bubbles order affordances copy · fork · bubble', () => {
    const { el } = createMessageBubble({ role: 'user', text: 'hi', forkPoint: 7 });
    const kids = [...el.children].map((c) => c.className.split(' ')[0]);
    expect(kids).toEqual(['msg-copy', 'msg-fork', 'message-body']);
  });

  it('assistant bubbles keep the header Copy — never the icon affordance', () => {
    const { el } = createMessageBubble({ role: 'assistant', raw: 'x' });
    expect(el.querySelector('.msg-copy')).toBeNull();
    expect(el.querySelector('.copy-btn')).not.toBeNull();
  });

  it('creates an assistant bubble with live class', () => {
    const { el, body } = createMessageBubble({ role: 'assistant', raw: '', live: true });
    expect(el.className).toContain('assistant');
    expect(el.className).toContain('live');
    expect(body.dataset.raw).toBe('');
  });

  it('adds staggered class with animation delay', () => {
    const { el } = createMessageBubble({ role: 'assistant', raw: '', staggerIndex: 5 });
    expect(el.className).toContain('staggered');
    expect(el.style.getPropertyValue('--stagger')).toBe('0.2s');
  });
});

describe('createReasoningBlock', () => {
  it('creates a details element with summary and content', () => {
    const { el, content } = createReasoningBlock();
    expect(el.tagName).toBe('DETAILS');
    expect(el.className).toContain('thinking');
    expect(content.className).toBe('thinking-content prose');
  });

  it('omits the dots animation unless explicitly live', () => {
    // Static renders (history/viewer) must not play the animated ellipsis.
    const staticBlock = createReasoningBlock({ summary: 'Thought for a bit' });
    expect(staticBlock.el.querySelector('.dots')).toBeNull();

    const live = createReasoningBlock({ dots: true });
    expect(live.el.querySelector('.dots')).toBeTruthy();
  });

  it('live summary text carries no literal dots beside the animated span', () => {
    // The animated span IS the ellipsis — literal '...' in the text would
    // render six dots (three static at text height, three animated lower).
    const live = createReasoningBlock({ dots: true });
    const summary = live.el.querySelector('summary')!;
    expect(summary.textContent).toBe('Thinking');
    // Static fallback (no summary, no dots) keeps the literal form.
    expect(createReasoningBlock().el.querySelector('summary')!.textContent).toBe('Thinking...');
  });

  it('adds staggered class with animation delay', () => {
    const { el } = createReasoningBlock({ staggerIndex: 5 });
    expect(el.className).toContain('staggered');
    expect(el.style.getPropertyValue('--stagger')).toBe('0.2s');
  });
});

describe('createToolCard', () => {
  it('creates a running tool card with args', () => {
    const el = createToolCard({ id: 'call_1', name: 'bash', args: 'echo hi', status: 'running' });
    expect(el.dataset.toolCallId).toBe('call_1');
    expect(el.className).toContain('running');
    expect(el.querySelector('.tool-status')?.textContent).toContain('running');
    expect(el.querySelector('code')?.textContent).toBe('echo hi');
  });

  it('creates a done tool card', () => {
    const el = createToolCard({ id: 'call_2', name: 'read_file', status: 'done' });
    expect(el.className).toContain('done');
    expect(el.querySelector('.tool-status')?.textContent).toContain('completed');
  });

  it('toggles expanded class on header click', () => {
    const el = createToolCard({ id: 'call_3', name: 'grep', status: 'running' });
    const header = el.querySelector('.tool-header') as HTMLElement;
    expect(el.className).not.toContain('expanded');
    header.click();
    expect(el.className).toContain('expanded');
    header.click();
    expect(el.className).not.toContain('expanded');
  });

  it('adds staggered class with animation delay', () => {
    const el = createToolCard({ id: 'call_4', name: 'bash', status: 'done', staggerIndex: 5 });
    expect(el.className).toContain('staggered');
    expect(el.style.getPropertyValue('--stagger')).toBe('0.2s');
  });
});

describe('setToolCardResult', () => {
  it('defers the result <pre> to first expansion (copy button immediate)', () => {
    const el = createToolCard({ id: 'c1', name: 'tool', status: 'running' });
    setToolCardResult(el, 'result text');
    // Lazy materialization: collapsed cards carry NO result <pre>…
    expect(el.querySelector('pre')).toBeNull();
    // …but the copy button works off the registry immediately.
    const btn = el.querySelector('.copy-btn');
    expect(btn).not.toBeNull();

    // First expansion materializes the <pre> with the full result.
    (el.querySelector('.tool-header') as HTMLElement).click();
    const pre = el.querySelector('pre');
    expect(pre?.textContent).toBe('result text');
  });

  it('materializes immediately when the card is already expanded', () => {
    const el = createToolCard({ id: 'c1', name: 'tool', status: 'running' });
    (el.querySelector('.tool-header') as HTMLElement).click(); // expand BEFORE the result arrives
    setToolCardResult(el, 'late result');
    expect(el.querySelector('pre')?.textContent).toBe('late result');
  });

  it('materialization is idempotent — re-expansion never duplicates', () => {
    const el = createToolCard({ id: 'c1', name: 'tool', status: 'running' });
    setToolCardResult(el, 'once');
    const header = el.querySelector('.tool-header') as HTMLElement;
    header.click(); // expand
    header.click(); // collapse
    header.click(); // expand again
    expect(el.querySelectorAll('pre')).toHaveLength(1);
    expect(el.querySelector('pre')?.textContent).toBe('once');
  });

  it('an empty result stores nothing and never materializes', () => {
    const el = createToolCard({ id: 'c1', name: 'tool', status: 'running' });
    setToolCardResult(el, '');
    (el.querySelector('.tool-header') as HTMLElement).click();
    expect(el.querySelector('pre')).toBeNull();
    expect(el.querySelector('.copy-btn')).toBeNull();
  });
});

describe('markToolCardComplete', () => {
  it('flips classes and status text', () => {
    const el = createToolCard({ id: 'c1', name: 'tool', status: 'running' });
    markToolCardComplete(el, 'done');
    expect(el.className).toContain('done');
    expect(el.className).toContain('just-completed');
    const status = el.querySelector('.tool-status');
    expect(status?.textContent).toContain('completed');
  });
});

describe('classifyToolResult — the kernel marker verdicts', () => {
  it('plain results are ok', () => {
    expect(classifyToolResult('hello\nworld')).toEqual({ verdict: 'ok' });
  });

  it('the kernel failure prefix is an error', () => {
    expect(classifyToolResult('Error: file not found: x')).toEqual({ verdict: 'error' });
  });

  it('the interrupt mark is interrupted (the user did it)', () => {
    expect(classifyToolResult('[interrupted by user]\npartial output')).toEqual({
      verdict: 'interrupted',
    });
  });

  it('a trailing non-zero exit line is the exit verdict with its code', () => {
    expect(classifyToolResult('partial output\n(exit code: 3)')).toEqual({
      verdict: 'exit',
      code: 3,
    });
  });

  it('exit code 0 is ok', () => {
    expect(classifyToolResult('(exit code: 0)')).toEqual({ verdict: 'ok' });
  });
});

describe('tool card verdict painting', () => {
  it('an error result takes the danger voice and never fades', () => {
    const el = createToolCard({ id: 'c1', name: 'bash', status: 'running' });
    markToolCardComplete(el, 'Error: command failed');
    expect(el.className).toContain('tool-error');
    expect(el.className).toContain('done');
    expect(el.querySelector('.tool-status')?.className).toBe('tool-status error');
    expect(el.querySelector('.tool-status')?.textContent).toContain('error');
  });

  it('an interrupted result speaks the warn voice without the error rail', () => {
    const el = createToolCard({ id: 'c1', name: 'bash', status: 'running' });
    markToolCardComplete(el, '[interrupted by user]\npartial');
    expect(el.className).toContain('tool-interrupted');
    expect(el.className).not.toContain('tool-error');
    expect(el.querySelector('.tool-status')?.className).toBe('tool-status interrupted');
    expect(el.querySelector('.tool-status')?.textContent).toContain('interrupted');
  });

  it('an exit-code result carries its code in the status voice', () => {
    const el = createToolCard({ id: 'c1', name: 'bash', status: 'running' });
    markToolCardComplete(el, 'partial output\n(exit code: 3)');
    expect(el.className).toContain('tool-exit');
    expect(el.querySelector('.tool-status')?.textContent).toContain('exit 3');
  });

  it('the history path paints the verdict through setToolCardResult alone', () => {
    const el = createToolCard({ id: 'c1', name: 'bash', status: 'done' });
    setToolCardResult(el, 'Error: stored failure');
    expect(el.className).toContain('tool-error');
    expect(el.querySelector('.tool-status')?.className).toBe('tool-status error');
  });

  it('an ok result clears stale verdict classes and restores the completed voice', () => {
    const el = createToolCard({ id: 'c1', name: 'bash', status: 'running' });
    markToolCardComplete(el, 'Error: first attempt');
    markToolCardComplete(el, 'recovered');
    expect(el.className).not.toContain('tool-error');
    expect(el.querySelector('.tool-status')?.className).toBe('tool-status completed');
  });
});

describe('createErrorBubble', () => {
  it('creates an error div with icon and message', () => {
    const el = createErrorBubble('something went wrong');
    expect(el.className).toContain('error');
    // The icon is aria-hidden decoration + text nodes (textContent includes icon glyphs)
    expect(el.querySelector('.error-icon')?.getAttribute('aria-hidden')).toBe('true');
    expect(el.textContent).toContain('something went wrong');
  });

  it('pane-context errors carry retry + copy actions', () => {
    const el = createErrorBubble('boom', { retry: true });
    expect(el.querySelector('.error-actions')?.getAttribute('role')).toBe('group');
    expect(el.querySelector('.error-action[aria-label^="Retry"]')).not.toBeNull();
    expect(el.querySelector('.error-action[aria-label="Copy error message"]')).not.toBeNull();
  });

  it('the bare no-chat error carries no actions', () => {
    const el = createErrorBubble('boom');
    expect(el.querySelector('.error-actions')).toBeNull();
  });

  it('retry refills the composer with the LAST user turn — never sends', () => {
    document.body.innerHTML =
      '<div class="chat-pane">' +
      '  <div class="message user"><div class="message-body prose">run the tests please</div></div>' +
      '  <div class="tool done">x</div>' +
      '</div>';
    const pane = document.querySelector('.chat-pane') as HTMLElement;
    const el = createErrorBubble('boom', { retry: true });
    pane.appendChild(el); // in-pane: the walk-back has a sibling chain

    const events: CustomEvent<string>[] = [];
    const listener = (e: Event) => events.push(e as CustomEvent<string>);
    window.addEventListener('flux:compose', listener);

    (el.querySelector('.error-action[aria-label^="Retry"]') as HTMLButtonElement).click();

    window.removeEventListener('flux:compose', listener);
    expect(events).toHaveLength(1);
    expect(events[0].detail).toBe('run the tests please');
    document.body.innerHTML = '';
  });

  it('retry without a preceding user turn is a silent no-op', () => {
    document.body.innerHTML = '<div class="chat-pane"></div>';
    const pane = document.querySelector('.chat-pane') as HTMLElement;
    const el = createErrorBubble('boom', { retry: true });
    pane.appendChild(el);
    const events: Event[] = [];
    const listener = (e: Event) => events.push(e);
    window.addEventListener('flux:compose', listener);
    (el.querySelector('.error-action[aria-label^="Retry"]') as HTMLButtonElement).click();
    window.removeEventListener('flux:compose', listener);
    expect(events).toHaveLength(0);
    document.body.innerHTML = '';
  });
});

describe('toolSummary', () => {
  it('extracts the bash command (first line)', () => {
    expect(
      toolSummary('bash', JSON.stringify({ command: 'cargo test --workspace\necho done' })),
    ).toBe('cargo test --workspace');
  });

  it('extracts the file path for file tools and the pattern for search tools', () => {
    expect(toolSummary('read_file', JSON.stringify({ file_path: '/a/b.rs' }))).toBe('/a/b.rs');
    expect(toolSummary('edit_file', JSON.stringify({ file_path: '/a.rs', edits: [] }))).toBe('/a.rs');
    expect(toolSummary('grep', JSON.stringify({ patterns: ['TODO'], path: '/x' }))).toBe('TODO');
    expect(toolSummary('glob', JSON.stringify({ patterns: ['*.rs'] }))).toBe('*.rs');
    expect(
      toolSummary('grep', JSON.stringify({ patterns: ['TODO', 'FIXME'] })),
    ).toBe('2 patterns: TODO');
  });

  it('summarizes multi-item args as "N items: <first path>" (incl. legacy names)', () => {
    expect(
      toolSummary(
        'read_files', // legacy name — tools removed, history must still render
        JSON.stringify({ files: [{ file_path: '/a.rs' }, { file_path: '/b.rs' }] }),
      ),
    ).toBe('2 items: /a.rs');
    expect(
      toolSummary('edit_files', JSON.stringify({ edits: [{ file_path: '/c.rs' }] })),
    ).toBe('/c.rs');
    // An array field without file_path objects falls back to the raw text.
    expect(toolSummary('read_files', JSON.stringify({ files: ['nope'] }))).toBe('{"files":["nope"]}');
  });

  it('falls back to the first string field / raw text and truncates long values', () => {
    expect(toolSummary('mcp_x', JSON.stringify({ q: 'hello world' }))).toBe('hello world');
    expect(toolSummary('weird', 'not json')).toBe('not json');
    const long = 'x'.repeat(80);
    expect(toolSummary('read_file', JSON.stringify({ path: long }))).toHaveLength(60);
  });
});

describe('tool card header (icon + summary + status)', () => {
  it('shows the args summary in the collapsed header', () => {
    const el = createToolCard({
      id: 'c1',
      name: 'bash',
      args: JSON.stringify({ command: 'npm test' }),
      status: 'done',
    });
    const summary = el.querySelector('.tool-args-summary');
    expect(summary?.textContent).toBe('npm test');
    // Monochrome SVG icons replace emoji
    expect(el.querySelector('.tool-icon svg')).toBeTruthy();
  });

  it('tracks elapsed time while running and freezes it on completion', () => {
    // Explicit fake Date: elapsed-time math depends on Date.now() deltas; the
    // card must be mounted in the DOM or the isConnected self-check guard stops
    // its timer (mirrors real pane mounting).
    vi.useFakeTimers({
      toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval', 'Date'],
    });
    const el = createToolCard({ id: 'c1', name: 'bash', args: '{}', status: 'running' });
    document.body.appendChild(el);
    try {
      const elapsed = el.querySelector('.tool-elapsed')!;
      vi.advanceTimersByTime(2100);
      // 200ms ticks: at 2100ms the last tick fired at 2000ms → shows 2.0s
      expect(elapsed.textContent).toBe('2.0s');

      markToolCardComplete(el, 'ok');
      const status = el.querySelector('.tool-status')!;
      expect(status.textContent).toContain('completed');
      expect(status.textContent).toContain('2.1s'); // the elapsed time froze into the status
    } finally {
      el.remove();
      vi.useRealTimers();
    }
  });

  it('adds a result meta line for long results', () => {
    const el = createToolCard({ id: 'c1', name: 'bash', args: '{}', status: 'running' });
    markToolCardComplete(el, Array.from({ length: 8 }, (_, i) => `line ${i}`).join('\n'));
    expect(el.querySelector('.tool-result-meta')?.textContent).toBe('8 lines');

    const short = createToolCard({ id: 'c2', name: 'bash', args: '{}', status: 'running' });
    markToolCardComplete(short, 'one line');
    expect(short.querySelector('.tool-result-meta')).toBeNull();
  });
});

describe('createTypingIndicator', () => {
  it('creates an aria-hidden three-dot indicator', () => {
    const el = createTypingIndicator();
    expect(el.id).toBe('typing-indicator');
    expect(el.getAttribute('aria-hidden')).toBe('true');
    expect(el.querySelectorAll('span').length).toBe(3);
  });
});

describe('isNearBottom', () => {
  it('returns true when near bottom', () => {
    const el = document.createElement('div');
    Object.defineProperty(el, 'scrollHeight', { value: 500, configurable: true });
    Object.defineProperty(el, 'clientHeight', { value: 200, configurable: true });
    el.scrollTop = 280;
    expect(isNearBottom(el, 50)).toBe(true);
  });

  it('returns false when scrolled up', () => {
    const el = document.createElement('div');
    Object.defineProperty(el, 'scrollHeight', { value: 500, configurable: true });
    Object.defineProperty(el, 'clientHeight', { value: 200, configurable: true });
    el.scrollTop = 0;
    expect(isNearBottom(el, 50)).toBe(false);
  });
});

describe('scrollPaneToBottom', () => {
  it('scrolls to the bottom when near the bottom', () => {
    const el = document.createElement('div');
    Object.defineProperty(el, 'scrollHeight', { value: 500, configurable: true });
    Object.defineProperty(el, 'clientHeight', { value: 200, configurable: true });
    el.scrollTop = 280;
    scrollPaneToBottom(el, 50);
    expect(el.scrollTop).toBe(500);
  });

  it('does not scroll when scrolled up', () => {
    const el = document.createElement('div');
    Object.defineProperty(el, 'scrollHeight', { value: 500, configurable: true });
    Object.defineProperty(el, 'clientHeight', { value: 200, configurable: true });
    el.scrollTop = 0;
    scrollPaneToBottom(el, 50);
    expect(el.scrollTop).toBe(0);
  });
});

// ── Expansion follow (bug: expanding the bottom tool card must scroll) ──

describe('followExpansion', () => {
  it('drives a time-bounded rAF follow loop that outlives the transition', () => {
    const pane = document.createElement('div');
    const rafCalls: FrameRequestCallback[] = [];
    vi.spyOn(window, 'requestAnimationFrame').mockImplementation((cb: FrameRequestCallback) => {
      rafCalls.push(cb);
      return rafCalls.length;
    });
    // Wall clock advances 50ms per callback invocation — the loop must
    // terminate on its TIME deadline, not a frame count (a frame counter
    // starved the follow on 120Hz+ displays).
    let clock = 0;
    const nowSpy = vi.spyOn(performance, 'now').mockImplementation(() => clock);
    followExpansion(pane, 300);
    // Drive every queued callback until none are left (bounded by safety).
    let guard = 0;
    while (rafCalls.length > 0 && guard++ < 200) {
      clock += 50;
      rafCalls.shift()!(0);
    }
    expect(guard).toBeLessThan(200); // terminated on the deadline
    // 300ms deadline / 50ms per frame ≈ 6 frames, ×2 callbacks per frame.
    expect(rafCalls.length).toBeLessThanOrEqual(16);
    nowSpy.mockRestore();
  });

  it('is a no-op for an element outside a .chat-pane', () => {
    const el = document.createElement('div');
    expect(() => followExpansionFrom(el)).not.toThrow();
  });

  it('derives the follow window from the expanded detail transition', () => {
    // The wiring detail: followExpansionFrom reads the .tool-detail's
    // computed transition duration. jsdom reports none → the minimum
    // window applies and the loop still starts (rAF scheduled).
    const pane = document.createElement('div');
    pane.className = 'chat-pane';
    document.body.appendChild(pane);
    const rafSpy = vi.spyOn(window, 'requestAnimationFrame').mockReturnValue(1);
    const card = createToolCard({ id: 'c1', name: 'bash', args: '{}', status: 'done' });
    pane.appendChild(card);
    (card.querySelector('.tool-header') as HTMLElement).click();
    expect(rafSpy).toHaveBeenCalled();
    rafSpy.mockRestore();
    document.body.innerHTML = '';
  });
});

describe('tool card expansion follow wiring', () => {
  function inPane(el: HTMLElement): HTMLElement {
    const pane = document.createElement('div');
    pane.className = 'chat-pane';
    pane.appendChild(el);
    document.body.appendChild(pane);
    return pane;
  }

  it('schedules a follow when expanding', () => {
    const rafSpy = vi.spyOn(window, 'requestAnimationFrame').mockReturnValue(1);
    const card = createToolCard({ id: 'c1', name: 'bash', args: '{}', status: 'done' });
    inPane(card);
    const header = card.querySelector('.tool-header') as HTMLElement;
    header.click();
    expect(card.classList.contains('expanded')).toBe(true);
    expect(rafSpy).toHaveBeenCalled();
    rafSpy.mockRestore();
    document.body.innerHTML = '';
  });

  it('does not schedule a follow when collapsing', () => {
    const rafSpy = vi.spyOn(window, 'requestAnimationFrame').mockReturnValue(1);
    const card = createToolCard({ id: 'c1', name: 'bash', args: '{}', status: 'done' });
    inPane(card);
    const header = card.querySelector('.tool-header') as HTMLElement;
    header.click(); // expand (schedules — clear the spy)
    rafSpy.mockClear();
    header.click(); // collapse
    expect(card.classList.contains('expanded')).toBe(false);
    expect(rafSpy).not.toHaveBeenCalled();
    rafSpy.mockRestore();
    document.body.innerHTML = '';
  });

  it('follows a user-opened reasoning details block', () => {
    const rafSpy = vi.spyOn(window, 'requestAnimationFrame').mockReturnValue(1);
    const { el } = createReasoningBlock({ html: 'thinking' });
    inPane(el);
    // jsdom does not synthesize the platform `toggle` event on programmatic
    // open (browsers do) — dispatch it to exercise our listener wiring.
    el.open = true;
    el.dispatchEvent(new Event('toggle'));
    expect(rafSpy).toHaveBeenCalled();
    rafSpy.mockRestore();
    document.body.innerHTML = '';
  });
});
