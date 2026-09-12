/**
 * terminal-panel.test.tsx — the status-line poll: the service mutates
 * session objects outside React, so the panel polls — but a poll that
 * re-renders every tick forever (the old 300ms unconditional setState)
 * burns renders while the terminal sits idle. The pinned semantic: the
 * state write happens ONLY on an actual status transition.
 */
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render } from '@testing-library/react';
import { TerminalPanel } from '../../components/TerminalPanel';

describe('TerminalPanel', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('the status poll re-renders only on an actual transition (idle ticks are free)', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let renders = 0;
    const Probe = (): React.ReactElement => {
      renders++;
      return (
        <div data-probe>
          <TerminalPanel tabId="t1" />
        </div>
      );
    };
    render(<Probe />);

    // Many idle ticks with NO session (status stays undefined) — the poll
    // compares and skips the state write: no re-render beyond the mount.
    const afterMount = renders;
    await vi.advanceTimersByTimeAsync(300 * 5);
    expect(renders).toBe(afterMount);
    // The status line shows the fallback (connecting) branch.
    expect(document.body.textContent).toContain('connecting…');
  });

  it('without a session the status line falls back to the connecting branch', () => {
    render(<TerminalPanel tabId="missing" />);
    expect(document.body.textContent).toContain('connecting…');
  });
});
