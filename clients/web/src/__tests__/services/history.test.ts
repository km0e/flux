import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { renderHistoryMessages } from '../../services/history';
import { useFlux } from '../../core/state';
import { _resetPanesForTest, getPane } from '../../services/panes';
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
});

describe('manual rebase entry (§2.1: archive-context-up-to-here)', () => {
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

  it('lease: user bubbles carry the rebase affordance keyed by row id', async () => {
    const pane = await renderWith();
    const buttons = pane.querySelectorAll<HTMLButtonElement>('.msg-rebase');
    expect(buttons.length).toBe(2);
    expect(buttons[0]!.dataset.baseMessageId).toBe('41');
    expect(buttons[1]!.dataset.baseMessageId).toBe('43');
  });

  it('readonly (lease elsewhere) hides the entry — a viewer cannot mutate', async () => {
    const pane = await renderWith(true);
    expect(pane.querySelectorAll('.msg-rebase').length).toBe(0);
  });

  it('messages without a row id never render the entry', async () => {
    const msgs: HistoryMessage[] = [{ role: 'user', content: 'assembled, no id' }];
    await renderHistoryMessages('test-chat', msgs);
    expect(getPane('test-chat').querySelectorAll('.msg-rebase').length).toBe(0);
  });

  it('click → confirm → bridge.send rebase with the clicked row id as base', async () => {
    const { setDialogImpls } = await import('../../services/dialogs');
    const { setBridge, resetBridgeForTest } = await import('../../core/bridge');
    const sent: unknown[] = [];
    resetBridgeForTest();
    setBridge({ send: (msg) => sent.push(msg) });
    setDialogImpls({ confirmRebase: async () => true });
    const pane = await renderWith();
    const btn = pane.querySelectorAll<HTMLButtonElement>('.msg-rebase')[0]!;
    btn.click();
    await new Promise((r) => setTimeout(r, 0));
    expect(sent).toContainEqual({
      type: 'rebase',
      chat_id: 'test-chat',
      base_message_id: 41,
    });
  });

  it('click → dismiss sends nothing', async () => {
    const { setDialogImpls, resetDialogsForTest } = await import('../../services/dialogs');
    const { setBridge, resetBridgeForTest } = await import('../../core/bridge');
    let sent = 0;
    resetBridgeForTest();
    setBridge({ send: () => sent++ });
    setDialogImpls({ confirmRebase: async () => false });
    const pane = await renderWith();
    pane.querySelectorAll<HTMLButtonElement>('.msg-rebase')[0]!.click();
    await new Promise((r) => setTimeout(r, 0));
    expect(sent).toBe(0);
    resetDialogsForTest();
  });
});
