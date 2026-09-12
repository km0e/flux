/**
 * right-dock.test.tsx — the tabbed right dock: multi-file tabs, the
 * pinned terminal tab (open button when no session), dock-level close.
 */
import { describe, it, expect, beforeEach } from 'vitest';
import { render, fireEvent, screen } from '@testing-library/react';
import { RightDock } from '../../components/RightDock';
import { useFlux, resetFluxForTest, type FileTabState } from '../../core/state';
import { preloadHighlighter } from '../../lib/highlight';

function tab(id: string, overrides: Partial<FileTabState> = {}): FileTabState {
  return {
    id,
    path: id,
    name: id.split('/').pop() ?? id,
    content: `content of ${id}`,
    truncated: false,
    loading: false,
    rawView: false,
    ...overrides,
  };
}

function seed(openFiles: FileTabState[], activeDockTab: string | null) {
  useFlux.setState({ dockOpen: true, openFiles, activeDockTab });
}

describe('RightDock', () => {
  beforeEach(() => {
    resetFluxForTest();
  });

  it('renders null when the dock is closed', () => {
    const { container } = render(<RightDock />);
    expect(container.querySelector('#right-dock')).toBeNull();
  });

  it('the dock is a flex sibling (pushes the conversation), not a fixed overlay', () => {
    seed([tab('/x/a.ts')], '/x/a.ts');
    const { container } = render(<RightDock />);
    const dock = container.querySelector('#right-dock') as HTMLElement;
    expect(dock.className).toContain('relative');
    expect(dock.className).toContain('shrink-0');
    expect(dock.className).not.toContain('fixed');
  });

  it('shows the active tab content; the strip owns the name and the floating actions', () => {
    seed([tab('/x/big.ts', { truncated: true })], '/x/big.ts');
    render(<RightDock />);
    expect(screen.getAllByText('big.ts').length).toBe(1); // strip only — no duplicate content header
    expect(screen.getByText('truncated')).toBeTruthy(); // floating badge
    expect(screen.getByRole('button', { name: 'Copy' })).toBeTruthy();
    expect(screen.getByText('content of /x/big.ts')).toBeTruthy();
  });

  it('a read failure shows a neutral placeholder — the error rides the toast stack', () => {
    seed(
      [tab('/x/a.ts', { content: '', error: 'no reply from server (timeout)' })],
      '/x/a.ts',
    );
    render(<RightDock />);
    expect(screen.getByText('(file could not be read)')).toBeTruthy();
    expect(screen.queryByText(/no reply/)).toBeNull();
  });

  it('supports multiple file tabs; closing one keeps the others', () => {
    seed([tab('/x/a.ts'), tab('/x/b.ts')], '/x/b.ts');
    render(<RightDock />);
    // Chrome-free content: every file name renders exactly once (strip).
    expect(screen.getAllByText('a.ts').length).toBe(1);
    expect(screen.getAllByText('b.ts').length).toBe(1);
    expect(screen.getByText('content of /x/b.ts')).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: 'Close b.ts' }));
    expect(useFlux.getState().openFiles.map((t) => t.id)).toEqual(['/x/a.ts']);
    // The active tab fell back to the remaining file tab.
    expect(useFlux.getState().activeDockTab).toBe('/x/a.ts');
    expect(screen.getByText('content of /x/a.ts')).toBeTruthy();
  });

  it('closing the LAST file tab falls back to a terminal tab (if any)', () => {
    useFlux.setState({ activeChatId: 'c1' });
    const meta = { id: 'term-1', chatId: 'c1', seq: 1 };
    useFlux.setState({ terminalTabs: [meta] });
    seed([tab('/x/a.ts')], '/x/a.ts');
    render(<RightDock />);
    fireEvent.click(screen.getByRole('button', { name: 'Close a.ts' }));
    expect(useFlux.getState().openFiles).toEqual([]);
    expect(useFlux.getState().activeDockTab).toBe('term-1');
  });

  it('the "+" button adds a terminal tab and activates it (no chat → disabled)', () => {
    seed([], null);
    const { rerender } = render(<RightDock />);
    // The strip's "+" — scoped: the empty state carries a same-named button
    // (which only exists WITH a chat).
    const stripPlus = () =>
      screen.getAllByRole('button', { name: 'New terminal' }).find((b) => b.closest('.overflow-x-auto'))!;
    expect((stripPlus() as HTMLButtonElement).disabled).toBe(true);

    useFlux.setState({ activeChatId: 'c1' });
    rerender(<RightDock />);
    fireEvent.click(stripPlus());
    expect(useFlux.getState().terminalTabs).toHaveLength(1);
    expect(useFlux.getState().activeDockTab).toBe(useFlux.getState().terminalTabs[0].id);
    expect(screen.getByText('Terminal 1')).toBeTruthy();
  });

  it('the Raw toggle floats in the strip and drives the tab render', async () => {
    // Pre-warm the highlighter chunk INSIDE the environment: renderMarkdown
    // on a .md tab starts its dynamic import, and a chain resolving after
    // vitest tears the environment down shows up as an unhandled error.
    await preloadHighlighter();
    seed([tab('/x/a.md', { name: 'a.md', content: '# hi' })], '/x/a.md');
    render(<RightDock />);
    fireEvent.click(screen.getByRole('button', { name: 'Raw' }));
    expect(useFlux.getState().openFiles[0].rawView).toBe(true);
    expect(screen.getByRole('button', { name: 'Rendered' })).toBeTruthy();
  });

  it('the dock close keeps the open tabs; reopening restores the view', () => {
    seed([tab('/x/a.ts')], '/x/a.ts');
    render(<RightDock />);
    fireEvent.click(screen.getByRole('button', { name: 'Close dock' }));
    expect(useFlux.getState().dockOpen).toBe(false);
    expect(useFlux.getState().openFiles).toHaveLength(1);
    // Reopen: same tabs, same active tab.
    useFlux.getState().setActiveDockTab('/x/a.ts');
    expect(useFlux.getState().dockOpen).toBe(true);
    expect(useFlux.getState().activeDockTab).toBe('/x/a.ts');
  });
});
