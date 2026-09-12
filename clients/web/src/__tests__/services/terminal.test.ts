/**
 * terminal.test.ts — the terminal service's socket-frame behavior.
 *
 * Drives a FakeWebSocket through the real connect() path (createTerminal
 * → spawnSession → connect) and pins the `exited` contract: a CLEAN shell
 * exit (code 0) auto-closes the tab — the server has already torn down
 * the PTY, the tab is dead UI — while a FAILED shell (non-zero) stays
 * with its exit-code status for diagnosis.
 *
 * Depends: services/terminal.ts, core/state.ts
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useFlux } from '../../core/state';
import {
  createTerminal,
  killTerminal,
  terminalSession,
  type TermSession,
} from '../../services/terminal';

/** Minimal WebSocket double: records frames, lets the test dispatch
 * server frames and control readyState. */
class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  static OPEN = 1;
  static CONNECTING = 0;
  url: string;
  binaryType = 'blob';
  readyState = 0;
  sent: (string | Uint8Array)[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((ev: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
  }
  send(data: string | Uint8Array): void {
    this.sent.push(data);
  }
  close(): void {
    this.readyState = 3;
    this.onclose?.();
  }
  /** Test helper: pretend the server sent a text control frame. */
  emit(frame: Record<string, unknown>): void {
    this.onmessage?.({ data: JSON.stringify(frame) });
  }
}

function lastSocket(): FakeWebSocket {
  return FakeWebSocket.instances.at(-1)!;
}

async function makeTerminal(): Promise<{ tabId: string; session: TermSession }> {
  const tabId = (await createTerminal('chat-1'))!;
  const session = terminalSession(tabId)!;
  await session.ready; // connect() runs last in the ready chain
  return { tabId, session };
}

describe('terminal service — exited frame handling', () => {
  beforeEach(() => {
    FakeWebSocket.instances = [];
    vi.stubGlobal('WebSocket', FakeWebSocket as unknown as typeof WebSocket);
    sessionStorage.setItem('flux.session.id', 'session-token');
    useFlux.setState({ activeChatId: 'chat-1' });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    sessionStorage.clear();
    useFlux.setState({ terminalTabs: [], activeChatId: undefined });
  });

  it('clean exit (code 0) auto-closes the tab and drops the session', async () => {
    const { tabId, session } = await makeTerminal();
    lastSocket().readyState = FakeWebSocket.OPEN;
    lastSocket().emit({ type: 'hello', term: 'term-1', attached: false });

    lastSocket().emit({ type: 'exited', code: 0 });

    // The tab meta is gone from the store; the session map dropped it;
    // the saved reattach id was cleaned so a refresh cannot restore it.
    expect(useFlux.getState().terminalTabs.some((t) => t.id === tabId)).toBe(false);
    expect(terminalSession(tabId)).toBeUndefined();
    expect(session.killed).toBe(true);
    expect(sessionStorage.getItem('flux.terms.chat-1')).toBe('[]');
  });

  it('failed shell (non-zero) keeps the tab with its exit code', async () => {
    const { tabId } = await makeTerminal();
    lastSocket().readyState = FakeWebSocket.OPEN;
    lastSocket().emit({ type: 'hello', term: 'term-2', attached: false });

    lastSocket().emit({ type: 'exited', code: 137 });

    const s = terminalSession(tabId);
    expect(s).toBeDefined();
    expect(s!.status).toBe('exited');
    expect(s!.exitedCode).toBe(137);
    expect(useFlux.getState().terminalTabs.some((t) => t.id === tabId)).toBe(true);
  });

  it('explicit kill still tears down (shared teardown path)', async () => {
    const { tabId, session } = await makeTerminal();
    lastSocket().readyState = FakeWebSocket.OPEN;
    lastSocket().emit({ type: 'hello', term: 'term-3', attached: false });

    killTerminal(tabId);

    // The user kill notifies the server before the local teardown.
    expect(lastSocket().sent).toContain(JSON.stringify({ type: 'close' }));
    expect(terminalSession(tabId)).toBeUndefined();
    expect(session.killed).toBe(true);
    expect(useFlux.getState().terminalTabs.some((t) => t.id === tabId)).toBe(false);
  });
});
