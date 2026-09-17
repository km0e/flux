import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { renderHistoryMessages, historyPageStart, historyFingerprint } from '../../services/history';
import { useFlux } from '../../core/state';
import { _resetPanesForTest, getPane, clearPaneMessages } from '../../services/panes';
import { markPaneStale } from '../../services/stream-handler';
import type { HistoryMessage } from '../../core/types';

describe('renderHistoryMessages', () => {
  let wrap: HTMLDivElement;

  beforeEach(() => {
    wrap = document.createElement('div');
    wrap.id = 'messages-wrap';
    document.body.appendChild(wrap);
    useFlux.setState({ activeChatId: 'test-chat' });
    useFlux.setState({ streaming: {} });
  });

  afterEach(() => {
    _resetPanesForTest();
    document.body.removeChild(wrap);
  });

  it('renders user message into pane', async () => {
    const msgs: HistoryMessage[] = [{ role: 'user', content: 'hello', tool_calls: [] }];
    await renderHistoryMessages('test-chat', msgs);
    const pane = getPane('test-chat');
    expect(pane.querySelector('.message.user')?.textContent).toContain('hello');
  });

  it('skips the rebuild while the chat is streaming (live-round safety net)', async () => {
    const pane = getPane('test-chat');
    const live = document.createElement('div');
    live.className = 'live-marker';
    pane.appendChild(live);
    useFlux.getState().setStreaming('test-chat', true);

    await renderHistoryMessages('test-chat', [{ role: 'user', content: 'snapshot', tool_calls: [] }]);

    // The in-flight live DOM is protected — the snapshot must not tear it down.
    expect(pane.querySelector('.live-marker')).toBeTruthy();
    expect(pane.textContent).not.toContain('snapshot');
  });

  it('re-renders a STALE pane despite the streaming flag (departed mid-round)', async () => {
    const pane = getPane('test-chat');
    const stale = document.createElement('div');
    stale.className = 'stale-marker';
    pane.appendChild(stale);
    useFlux.getState().setStreaming('test-chat', true);
    markPaneStale('test-chat');

    await renderHistoryMessages('test-chat', [{ role: 'user', content: 'snapshot', tool_calls: [] }]);

    // The flag went stale across the unsubscribe window (stream_end was
    // delivered only to subscribers): the snapshot is the resync — it must
    // replace the outdated DOM even though streaming still reads true.
    expect(pane.querySelector('.stale-marker')).toBeNull();
    expect(pane.textContent).toContain('snapshot');
  });

  it('renders assistant message with markdown', async () => {
    const msgs: HistoryMessage[] = [
      { role: 'assistant', content: '**bold** reply', tool_calls: [] },
    ];
    await renderHistoryMessages('test-chat', msgs);
    const pane = getPane('test-chat');
    expect(pane.querySelector('.message.assistant')).toBeTruthy();
    expect(pane.querySelector('.message.assistant strong')?.textContent).toBe('bold');
  });

  it('renders reasoning block when reasoning_content present', async () => {
    const msgs: HistoryMessage[] = [
      {
        role: 'assistant',
        content: 'answer',
        reasoning_content: 'thinking...',
        tool_calls: [],
      },
    ];
    await renderHistoryMessages('test-chat', msgs);
    const pane = getPane('test-chat');
    const details = pane.querySelector('details.thinking');
    expect(details).toBeTruthy();
    // Reasoning blocks in history should be collapsed
    expect((details as HTMLDetailsElement).open).toBe(false);
  });

  it('renders assistant message whose tool_calls field is absent on the wire', async () => {
    // The Rust server skips serializing an empty tool_calls vec, so
    // text-only assistant messages arrive without the field at all.
    const wire = { role: 'assistant', content: 'plain answer' } as unknown as HistoryMessage;
    await renderHistoryMessages('test-chat', [wire]);
    const pane = getPane('test-chat');
    expect(pane.querySelector('.message.assistant')?.textContent).toContain('plain answer');
  });

  it('renders tool call cards for assistant messages', async () => {
    const msgs: HistoryMessage[] = [
      {
        role: 'assistant',
        content: 'let me check',
        tool_calls: [{ id: 'tc1', name: 'read_file', arguments: '{"path":"/f"}' }],
      },
    ];
    await renderHistoryMessages('test-chat', msgs);
    const pane = getPane('test-chat');
    const toolCard = pane.querySelector('.message.tool') as HTMLElement;
    expect(toolCard).toBeTruthy();
    expect(toolCard.dataset.toolCallId).toBe('tc1');
  });

  it('hides empty state when messages are present', async () => {
    const msgs: HistoryMessage[] = [{ role: 'user', content: 'hi', tool_calls: [] }];
    await renderHistoryMessages('test-chat', msgs);
    const empty = document.getElementById('empty-test-chat');
    expect(empty?.style.display).toBe('none');
  });

  it('merges tool results into the matching call card (live-path parity)', async () => {
    // Live path: tool_start builds the card, tool_result fills it (one card).
    // History rendering must match — merge by tool_call_id instead of adding
    // an extra anonymous orphan result card.
    const msgs: HistoryMessage[] = [
      {
        role: 'assistant',
        content: '',
        tool_calls: [{ id: 'tc1', name: 'read_file', arguments: '{"path":"/f"}' }],
      },
      { role: 'tool', tool_call_id: 'tc1', content: 'file contents here' },
    ];
    await renderHistoryMessages('test-chat', msgs);
    const pane = getPane('test-chat');
    const cards = pane.querySelectorAll('.message.tool');
    expect(cards.length).toBe(1); // call + result merge into ONE card
    const card = cards[0] as HTMLElement;
    expect(card.dataset.toolCallId).toBe('tc1');
    expect(card.querySelector('.tool-status')?.textContent).toContain('completed');
    // Lazy materialization (semantic change, pinned): the collapsed card
    // carries NO result <pre>; expanding builds it with the merged result.
    expect(card.querySelector('.tool-result-container pre')).toBeNull();
    (card.querySelector('.tool-header') as HTMLElement).click();
    expect(card.querySelector('.tool-result-container pre')?.textContent).toBe(
      'file contents here',
    );
  });

  it('falls back to a standalone result card when no call card matches', async () => {
    const msgs: HistoryMessage[] = [
      { role: 'tool', tool_call_id: 'tc-orphan', content: 'orphan result' },
    ];
    await renderHistoryMessages('test-chat', msgs);
    const pane = getPane('test-chat');
    const card = pane.querySelector('.message.tool') as HTMLElement;
    expect(card).toBeTruthy();
    expect(card.textContent).toContain('result');
  });

  it('scrolls to the latest message after rendering a long history', async () => {
    const msgs: HistoryMessage[] = Array.from({ length: 40 }, (_, i) => ({
      role: 'user' as const,
      content: `msg ${i}`,
    }));
    await renderHistoryMessages('test-chat', msgs);
    const pane = getPane('test-chat');
    // Opening a chat means seeing the latest — parking at the top leaves the newest reply out of view.
    expect(pane.scrollTop).toBe(pane.scrollHeight);
  });

  it('does not tear down the pane while the chat is streaming', async () => {
    // Render history into the pane first (not streaming).
    await renderHistoryMessages('test-chat', [{ role: 'user', content: 'first', tool_calls: [] }]);
    const pane = getPane('test-chat');
    expect(pane.textContent).toContain('first');

    // Now the chat starts streaming; a second history render (duplicate
    // chat_open / refresh race) must NOT clear the pane — the live stream
    // is rendering newer content and a rebuild would drop in-flight frames.
    useFlux.setState({ streaming: { 'test-chat': true } });
    await renderHistoryMessages('test-chat', [{ role: 'user', content: 'second', tool_calls: [] }]);
    expect(pane.textContent).toContain('first');
    expect(pane.textContent).not.toContain('second');
  });

  it('skips the rebuild when the re-delivered snapshot is unchanged (switch-back fast path)', async () => {
    // The claim re-delivers the full snapshot on EVERY switch back; when it
    // is identical to what the pane already shows (the A↔B toggle with the
    // pane parked at the bottom), the tear-down + rebuild + msgIn replay is
    // the switch flash — the pane must survive untouched.
    const msgs: HistoryMessage[] = [{ role: 'user', content: 'hello', tool_calls: [] }];
    await renderHistoryMessages('test-chat', msgs);
    const pane = getPane('test-chat');
    // Sentinel: a rebuild removes every child, so its survival proves the skip.
    const sentinel = document.createElement('div');
    sentinel.className = 'sentinel';
    pane.appendChild(sentinel);

    // A fresh copy of the same snapshot — structurally what the wire redelivers.
    await renderHistoryMessages('test-chat', msgs.map((m) => ({ ...m })));

    expect(pane.querySelector('.sentinel')).toBeTruthy();
    expect(pane.querySelector('.message.user')?.textContent).toContain('hello');
  });

  it('re-renders when the snapshot changed while away (messages appended elsewhere)', async () => {
    const msgs: HistoryMessage[] = [{ role: 'user', content: 'first', tool_calls: [] }];
    await renderHistoryMessages('test-chat', msgs);
    const pane = getPane('test-chat');

    // Another window streamed while this one was unsubscribed: the count
    // changed, the fingerprint misses, the full path must run.
    const grown: HistoryMessage[] = [
      ...msgs,
      { role: 'assistant', content: 'second', tool_calls: [] },
    ];
    await renderHistoryMessages('test-chat', grown);

    expect(pane.textContent).toContain('second');
  });

  it('never skips onto a wiped pane (cursor survives clearPaneMessages)', async () => {
    const msgs: HistoryMessage[] = [{ role: 'user', content: 'hello', tool_calls: [] }];
    await renderHistoryMessages('test-chat', msgs);
    // switchLease's stale-wipe path clears the DOM but the pane's cursor
    // (and its fingerprint) survive in the WeakMap.
    clearPaneMessages('test-chat');
    const pane = getPane('test-chat');
    expect(pane.querySelector('.message')).toBeNull();

    // The identical snapshot returns — the DOM-presence gate must force the
    // full render; cursor-only matching would skip onto an empty pane and
    // leave the chat blank.
    await renderHistoryMessages('test-chat', msgs.map((m) => ({ ...m })));
    expect(pane.querySelector('.message.user')?.textContent).toContain('hello');
  });
});

describe('historyFingerprint', () => {
  it('separates an unchanged snapshot from an appended or edited one', () => {
    const base: HistoryMessage[] = [{ role: 'user', content: 'hello', tool_calls: [] }];
    // Structural copy (what the wire redelivers) → identical fingerprint.
    expect(historyFingerprint(base)).toBe(historyFingerprint(base.map((m) => ({ ...m }))));
    // Appended message → count changed.
    expect(historyFingerprint(base)).not.toBe(
      historyFingerprint([...base, { role: 'assistant', content: 'reply', tool_calls: [] }]),
    );
    // Same count, different tail content → length changed.
    expect(historyFingerprint(base)).not.toBe(
      historyFingerprint([{ role: 'user', content: 'hello!!', tool_calls: [] }]),
    );
  });
});

describe('manual fork entry (branch-into-a-new-conversation)', () => {
  let wrap: HTMLDivElement;

  beforeEach(() => {
    wrap = document.createElement('div');
    wrap.id = 'messages-wrap';
    document.body.appendChild(wrap);
    useFlux.setState({ activeChatId: 'test-chat' });
    useFlux.setState({ streaming: {} });
    useFlux.setState({ readonlyChats: {} });
    useFlux.setState({
      chats: [
        {
          id: 'test-chat',
          name: 'One',
          createdAt: 0,
          active: false,
          workdir: '/tmp/p',
          provider: 'p',
          model: 'm',
        },
      ],
    });
  });

  afterEach(() => {
    _resetPanesForTest();
    document.body.removeChild(wrap);
  });

  async function renderWith(readonly = false) {
    useFlux.setState({
      chats: [
        {
          id: 'test-chat',
          name: 'One',
          createdAt: 0,
          active: false,
          workdir: '/tmp/p',
          provider: 'p',
          model: 'm',
        },
      ],
    });
    if (readonly) useFlux.getState().setReadOnly('test-chat', true);
    const msgs: HistoryMessage[] = [
      { id: 41, role: 'user', content: 'first ask' },
      { id: 42, role: 'assistant', content: 'answer' },
      { id: 43, role: 'user', content: 'second ask' },
    ];
    await renderHistoryMessages('test-chat', msgs);
    return getPane('test-chat');
  }

  it('user bubbles carry the fork affordance keyed by row id', async () => {
    const pane = await renderWith();
    const buttons = pane.querySelectorAll<HTMLButtonElement>('.msg-fork');
    expect(buttons.length).toBe(2);
    expect(buttons[0]!.dataset.forkPoint).toBe('41');
    expect(buttons[1]!.dataset.forkPoint).toBe('43');
  });

  it('readonly (lease elsewhere) keeps the affordance — a viewer may fork', async () => {
    const pane = await renderWith(true);
    expect(pane.querySelectorAll('.msg-fork').length).toBe(2);
  });

  it('messages without a row id never render the entry', async () => {
    const msgs: HistoryMessage[] = [{ role: 'user', content: 'assembled, no id' }];
    await renderHistoryMessages('test-chat', msgs);
    expect(getPane('test-chat').querySelectorAll('.msg-fork').length).toBe(0);
  });

  it('click → bridge.send fork with the clicked row id as the fork point (no confirm)', async () => {
    const { setBridge, resetBridgeForTest } = await import('../../core/bridge');
    const sent: unknown[] = [];
    resetBridgeForTest();
    setBridge({ send: (msg) => sent.push(msg) });
    const pane = await renderWith();
    const btn = pane.querySelectorAll<HTMLButtonElement>('.msg-fork')[0]!;
    btn.click();
    await new Promise((r) => setTimeout(r, 0));
    expect(sent).toContainEqual({
      type: 'fork',
      chat_id: 'test-chat',
      fork_point: 41,
    });
    // The redo-turn draft rides the click: the copy excludes the fork
    // point, so its content waits here for the fork's chat_created ack
    // (which prefills the fork's composer — see forkDraft.ts).
    const { pairForkDraft, takePendingDraft } = await import('../../services/forkDraft');
    expect(pairForkDraft('some-fork', 'test-chat')).toBe(true);
    expect(takePendingDraft('some-fork')).toBe('first ask');
  });
});

describe('live fork affordance (message_persisted back-pressure)', () => {
  let wrap: HTMLDivElement;

  beforeEach(() => {
    wrap = document.createElement('div');
    wrap.id = 'messages-wrap';
    document.body.appendChild(wrap);
    useFlux.setState({ activeChatId: 'test-chat' });
    useFlux.setState({ streaming: {} });
    useFlux.setState({ readonlyChats: {} });
  });

  afterEach(() => {
    _resetPanesForTest();
    document.body.removeChild(wrap);
  });

  it("attaches the affordance to the oldest un-id'd live bubble with matching text", async () => {
    const { appendUserMessage } = await import('../../services/stream-handler');
    const { attachForkToLiveBubble } = await import('../../services/history');
    const pane = getPane('test-chat');
    appendUserMessage('live question');
    expect(pane.querySelector('.msg-fork')).toBeNull();

    expect(attachForkToLiveBubble('test-chat', 7, 'live question')).toBe(true);
    const btn = pane.querySelector('.msg-fork') as HTMLButtonElement;
    expect(btn).toBeTruthy();
    expect(btn.dataset.forkPoint).toBe('7');
  });

  it('never matches a bubble whose turn was cancelled (content differs)', async () => {
    const { appendUserMessage } = await import('../../services/stream-handler');
    const { attachForkToLiveBubble } = await import('../../services/history');
    const pane = getPane('test-chat');
    appendUserMessage('cancelled turn');
    appendUserMessage('later turn');

    // The cancelled turn never persisted — the next announcement is for the
    // LATER turn's content and must NOT land on the earlier bubble.
    expect(attachForkToLiveBubble('test-chat', 9, 'later turn')).toBe(true);
    const btns = [...pane.querySelectorAll('.msg-fork')];
    expect(btns).toHaveLength(1);
    expect(
      (btns[0]!.closest('.message.user')!.querySelector('.message-body') as HTMLElement)
        .textContent,
    ).toBe('later turn');
  });

  it('attaches on a read-only chat too (forking is non-destructive)', async () => {
    const { appendUserMessage } = await import('../../services/stream-handler');
    const { attachForkToLiveBubble } = await import('../../services/history');
    const pane = getPane('test-chat');
    useFlux.getState().setReadOnly('test-chat', true);
    appendUserMessage('watching');
    expect(attachForkToLiveBubble('test-chat', 3, 'watching')).toBe(true);
    expect(pane.querySelector('.msg-fork')).toBeTruthy();
  });
});

// ── History pagination (tail-first) ──────────────────────────────────────

describe('historyPageStart (round-atomic page alignment)', () => {
  it('returns 0 untouched (the first page never aligns backwards past the start)', async () => {
    const msgs: HistoryMessage[] = [
      { role: 'assistant', content: 'a', tool_calls: [] },
      { role: 'user', content: 'u', tool_calls: [] },
    ];
    expect(historyPageStart(msgs, 0)).toBe(0);
  });

  it('aligns a candidate that lands mid-round back to the round user message', async () => {
    // Round: user(0) → assistant(1) → tool(2) → assistant(3) → user(4).
    // A candidate at 2 (the tool result) must pull back to 0 — a page
    // starting at a tool result would orphan it from its call card.
    const msgs: HistoryMessage[] = [
      { role: 'user', content: 'q1', tool_calls: [] },
      { role: 'assistant', content: '', tool_calls: [{ id: 'c1', name: 'bash', arguments: '{}' }] },
      { role: 'tool', content: 'out', tool_call_id: 'c1' },
      { role: 'assistant', content: 'done', tool_calls: [] },
      { role: 'user', content: 'q2', tool_calls: [] },
    ];
    expect(historyPageStart(msgs, 2)).toBe(0);
    expect(historyPageStart(msgs, 3)).toBe(0);
    expect(historyPageStart(msgs, 4)).toBe(4);
  });

  it('clamps out-of-range candidates', async () => {
    const msgs: HistoryMessage[] = [{ role: 'user', content: 'q', tool_calls: [] }];
    expect(historyPageStart(msgs, 99)).toBe(0);
    expect(historyPageStart(msgs, -5)).toBe(0);
  });
});

describe('tail-first history pagination', () => {
  let wrap: HTMLDivElement;

  beforeEach(() => {
    wrap = document.createElement('div');
    wrap.id = 'messages-wrap';
    document.body.appendChild(wrap);
    useFlux.setState({ activeChatId: 'test-chat' });
    useFlux.setState({ streaming: {} });
  });

  afterEach(() => {
    _resetPanesForTest();
    document.body.removeChild(wrap);
  });

  it('renders only the last page for a long transcript and offers the rest', async () => {
    const msgs: HistoryMessage[] = Array.from({ length: 150 }, (_, i) => ({
      role: i % 2 === 0 ? ('user' as const) : ('assistant' as const),
      content: `msg-${i}`,
      id: i + 1,
      tool_calls: [],
    }));
    await renderHistoryMessages('test-chat', msgs);
    const pane = getPane('test-chat');

    const rendered = pane.querySelectorAll('.message.user, .message.assistant');
    expect(rendered.length).toBe(60);
    // The tail is what renders — the LAST message must be present, the first must not.
    expect(pane.textContent).toContain('msg-149');
    expect(pane.textContent).not.toContain('msg-0');
    const btn = pane.querySelector('.history-load-earlier') as HTMLButtonElement;
    expect(btn?.textContent).toBe('Load earlier messages (90)');
  });

  it('prepends earlier pages on click, oldest first, and retires the button at the top', async () => {
    const msgs: HistoryMessage[] = Array.from({ length: 150 }, (_, i) => ({
      role: i % 2 === 0 ? ('user' as const) : ('assistant' as const),
      content: `msg-${i}`,
      id: i + 1,
      tool_calls: [],
    }));
    await renderHistoryMessages('test-chat', msgs);
    const pane = getPane('test-chat');

    const btn = pane.querySelector('.history-load-earlier') as HTMLButtonElement;
    btn.click();
    let rendered = pane.querySelectorAll('.message.user, .message.assistant');
    expect(rendered.length).toBe(120);
    expect(pane.textContent).toContain('msg-30');
    expect((pane.querySelector('.history-load-earlier') as HTMLButtonElement).textContent).toBe(
      'Load earlier messages (30)',
    );
    // Page order preserved: msg-30 sits ABOVE msg-149's page.
    const first = rendered[0]!.querySelector('.message-body')!.textContent;
    expect(first).toBe('msg-30');

    (pane.querySelector('.history-load-earlier') as HTMLButtonElement).click();
    rendered = pane.querySelectorAll('.message.user, .message.assistant');
    expect(rendered.length).toBe(150);
    expect(pane.textContent).toContain('msg-0');
    expect(pane.querySelector('.history-load-earlier')).toBeNull();
  });

  it('keeps a tool pair atomic when the page boundary lands mid-round', async () => {
    // 3 rounds; page candidate (150-60…) is irrelevant at this scale — build
    // exactly 61 messages so the naive tail slice starts mid-round: 40 user/
    // assistant pairs then a tool round. Simpler: craft directly.
    const msgs: HistoryMessage[] = [];
    for (let r = 0; r < 30; r++) {
      msgs.push({ role: 'user', content: `q-${r}`, id: r * 3 + 1, tool_calls: [] });
      msgs.push({
        role: 'assistant',
        content: '',
        id: r * 3 + 2,
        tool_calls: [{ id: `call-${r}`, name: 'bash', arguments: '{}' }],
      });
      msgs.push({ role: 'tool', content: `out-${r}`, tool_call_id: `call-${r}` });
    }
    await renderHistoryMessages('test-chat', msgs);
    const pane = getPane('test-chat');

    // Every rendered tool result merged into its call card: NO orphan
    // result cards (a standalone card carries an empty .tool-args-summary).
    const orphanResults = [...pane.querySelectorAll('.tool')].filter(
      (el) => el.querySelector('.tool-status')?.textContent?.trim() === 'result',
    );
    expect(orphanResults).toHaveLength(0);
    // The first user message in the DOM starts a page — every call card in
    // the pane found its result (60 rendered of 90 messages, but the page
    // starts at a user boundary: 20 full rounds).
    const renderedUsers = pane.querySelectorAll('.message.user').length;
    expect(renderedUsers).toBe(20);
    const btn = pane.querySelector('.history-load-earlier') as HTMLButtonElement;
    expect(btn?.textContent).toBe('Load earlier messages (30)');
  });

  it('merges tool pairs inside a CLICK-loaded earlier page (off-pane fragment)', async () => {
    // The earlier page builds into an unmounted DocumentFragment: while the
    // loop runs, a pane query cannot see the call cards created moments
    // earlier in the SAME page — the merge must consult the page's own
    // registry (the bug: every same-page pair rendered call card + orphan
    // anonymous result card). Pagination: the tool round sits at indices
    // 0–2, followed by 40 user/assistant pairs (83 total) — the tail page
    // starts at index 23, so the round only enters on a click.
    const msgs: HistoryMessage[] = [
      { role: 'user', content: 'q-early', id: 1, tool_calls: [] },
      {
        role: 'assistant',
        content: '',
        id: 2,
        tool_calls: [{ id: 'call-early', name: 'bash', arguments: '{}' }],
      },
      { role: 'tool', content: 'early output', tool_call_id: 'call-early' },
    ];
    for (let r = 0; r < 40; r++) {
      msgs.push({ role: 'user', content: `q-${r}`, id: 3 + r * 2, tool_calls: [] });
      msgs.push({ role: 'assistant', content: `a-${r}`, id: 4 + r * 2, tool_calls: [] });
    }
    await renderHistoryMessages('test-chat', msgs);
    const pane = getPane('test-chat');
    // The tail page holds no tool round — the orphan filter would be
    // trivially green before the click.
    expect(pane.querySelector('[data-tool-call-id="call-early"]')).toBeNull();
    const btn = pane.querySelector('.history-load-earlier') as HTMLButtonElement;
    expect(btn.textContent).toBe('Load earlier messages (23)');

    btn.click();

    const card = pane.querySelector('[data-tool-call-id="call-early"]') as HTMLElement;
    expect(card).toBeTruthy();
    // ONE card — the result merged in, no anonymous orphan beside it.
    const orphanResults = [...pane.querySelectorAll('.tool')].filter(
      (el) => el.querySelector('.tool-status')?.textContent?.trim() === 'result',
    );
    expect(orphanResults).toHaveLength(0);
    (card.querySelector('.tool-header') as HTMLElement).click();
    expect(card.querySelector('.tool-result-container pre')?.textContent).toBe('early output');
  });

  it('a short history renders whole with no affordance', async () => {
    const msgs: HistoryMessage[] = [
      { role: 'user', content: 'only', id: 1, tool_calls: [] },
      { role: 'assistant', content: 'answer', id: 2, tool_calls: [] },
    ];
    await renderHistoryMessages('test-chat', msgs);
    const pane = getPane('test-chat');
    expect(pane.querySelectorAll('.message').length).toBe(2);
    expect(pane.querySelector('.history-load-earlier')).toBeNull();
  });
});
