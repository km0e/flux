/**
 * explorer.test.tsx — the workdir file tree: lazy per-directory loads over
 * fs_list, file activation → the read-only preview, error surfacing.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, fireEvent, screen, waitFor } from '@testing-library/react';
import { Explorer } from '../../components/Explorer';
import { useFlux, resetFluxForTest } from '../../core/state';
import { listDir, readFile, DISCONNECTED_ERROR } from '../../services/fs';
import type { FsListing, FsContent } from '../../core/types';

vi.mock('../../services/fs', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../services/fs')>()),
  listDir: vi.fn(),
  readFile: vi.fn(),
}));

/** Wire the fs calls to canned replies (the RPCs resolve as promises). */
function mockFs(listing: (path: string) => FsListing, content?: (path: string) => FsContent) {
  vi.mocked(listDir).mockImplementation(async (path?: string) => listing(path ?? ''));
  if (content) vi.mocked(readFile).mockImplementation(async (path: string) => content(path));
  else vi.mocked(readFile).mockResolvedValue({ type: 'fs_content', requested: '', error: 'no read' });
}

describe('Explorer', () => {
  beforeEach(() => {
    resetFluxForTest();
    vi.clearAllMocks();
    // Browsing requires a live connection — fs requests fail fast offline.
    useFlux.setState({
      connectionStatus: 'connected',
      chats: [
        {
          id: 'c1',
          name: 'R',
          createdAt: 1,
          active: false,
          workdir: '/proj',
        provider: '',
        model: '',
        },
      ],
      activeChatId: 'c1',
    });
  });

  it('lazy-loads the root on mount and shows the entries', async () => {
    mockFs(() => ({
      type: 'fs_listing',
      requested: '/proj',
      path: '/proj',
      parent: '/',
      entries: [
        { name: 'src', kind: 'dir' },
        { name: 'README.md', kind: 'file', size: 120 },
      ],
    }));
    render(<Explorer key="/proj" />);
    await waitFor(() => {
      expect(screen.getByTitle('/proj/src')).toBeTruthy();
      expect(screen.getByTitle('/proj/README.md')).toBeTruthy();
    });
    expect(screen.getByText('README.md')).toBeTruthy();
    expect(screen.getByText('120 B')).toBeTruthy();
  });

  it('rows are drag sources carrying the absolute path (composer drop-to-reference)', async () => {
    mockFs(() => ({
      type: 'fs_listing',
      requested: '/proj',
      path: '/proj',
      parent: '/',
      entries: [{ name: 'README.md', kind: 'file' as const, size: 120 }],
    }));
    render(<Explorer key="/proj" />);
    await waitFor(() => {
      expect(screen.getByTitle('/proj/README.md')).toBeTruthy();
    });
    const row = screen.getByTitle('/proj/README.md');
    expect(row.getAttribute('draggable')).toBe('true');
    const setData = vi.fn();
    fireEvent.dragStart(row, {
      dataTransfer: { setData, effectAllowed: 'none' },
    });
    expect(setData).toHaveBeenCalledWith('text/plain', '/proj/README.md');
  });

  it('expanding a subdirectory lists its contents', async () => {
    const listed: string[] = [];
    mockFs(
      (path) => {
        listed.push(path);
        const entries =
          path === '/proj'
            ? [{ name: 'src', kind: 'dir' as const }]
            : [{ name: 'main.rs', kind: 'file' as const, size: 10 }];
        return { type: 'fs_listing', requested: path, path, parent: '/', entries };
      },
      (path) => ({ type: 'fs_content', requested: path, content: 'fn main() {}' }),
    );
    render(<Explorer key="/proj" />);
    await waitFor(() => expect(screen.getByTitle('/proj/src')).toBeTruthy());
    fireEvent.click(screen.getByTitle('/proj/src'));
    await waitFor(() => expect(screen.getByText('main.rs')).toBeTruthy());
    expect(listed).toContain('/proj/src');
  });

  it('clicking a file opens the preview dock with the content', async () => {
    mockFs(
      () => ({
        type: 'fs_listing',
        requested: '/proj',
        path: '/proj',
        parent: '/',
        entries: [{ name: 'a.ts', kind: 'file' }],
      }),
      (path) => ({ type: 'fs_content', requested: path, content: 'const a = 1;' }),
    );
    render(<Explorer key="/proj" />);
    await waitFor(() => expect(screen.getByTitle('/proj/a.ts')).toBeTruthy());
    fireEvent.click(screen.getByTitle('/proj/a.ts'));
    await waitFor(() => {
      const fp = useFlux.getState().openFiles[0];
      expect(fp?.path).toBe('/proj/a.ts');
      expect(fp?.content).toBe('const a = 1;');
    });
  });

  it('a listing failure surfaces on the unified toast, not on the row', async () => {
    mockFs(() => ({
      type: 'fs_listing',
      requested: '/proj',
      error: 'permission denied',
      entries: [],
    }));
    render(<Explorer key="/proj" />);
    await waitFor(() => {
      const toasts = useFlux.getState().toasts;
      expect(toasts.some((t) => t.text.includes('permission denied'))).toBe(true);
    });
    // The tree area shows a neutral placeholder — never the raw error.
    expect(screen.getByText('(directory unavailable)')).toBeTruthy();
    expect(screen.queryByText('permission denied')).toBeNull();
  });

  it('manual refresh re-lists every loaded directory', async () => {
    const listed: string[] = [];
    mockFs((path) => {
      listed.push(path);
      return {
        type: 'fs_listing',
        requested: path,
        path,
        parent: '/',
        entries: [{ name: 'README.md', kind: 'file' as const, size: 3 }],
      };
    });
    render(<Explorer key="/proj" />);
    await waitFor(() => expect(screen.getByTitle('/proj/README.md')).toBeTruthy());
    expect(listed.filter((p) => p === '/proj').length).toBe(1);
    fireEvent.click(screen.getByRole('button', { name: 'Refresh files' }));
    await waitFor(() => expect(listed.filter((p) => p === '/proj').length).toBe(2));
  });

  it('auto-refresh re-lists the loaded dirs on the fixed cadence', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const listed: string[] = [];
    mockFs((path) => {
      listed.push(path);
      return {
        type: 'fs_listing',
        requested: path,
        path,
        parent: '/',
        entries: [{ name: 'README.md', kind: 'file' as const, size: 3 }],
      };
    });
    render(<Explorer key="/proj" />);
    await waitFor(() => expect(listed.filter((p) => p === '/proj').length).toBe(1));
    vi.advanceTimersByTime(15_000);
    await waitFor(() => expect(listed.filter((p) => p === '/proj').length).toBe(2));
    vi.useRealTimers();
  });

  it('without a workdir it shows a hint', () => {
    useFlux.setState({ chats: [], activeChatId: '' });
    render(<Explorer key="none" />);
    expect(screen.getByText(/Open a conversation to browse/)).toBeTruthy();
  });

  it('stays silent while offline: no fs requests, no toasts', async () => {
    useFlux.setState({ connectionStatus: 'disconnected' });
    // The RPC layer resolves with the outage marker while detached.
    vi.mocked(listDir).mockResolvedValue({
      type: 'fs_listing',
      requested: '/proj',
      error: DISCONNECTED_ERROR,
      entries: [],
    });
    render(<Explorer key="/proj" />);
    await new Promise((r) => setTimeout(r, 20));
    expect(useFlux.getState().toasts).toHaveLength(0);
    // The tree states the outage instead of a filesystem failure.
    expect(screen.getByText(/Server disconnected/)).toBeTruthy();
  });

  it('git status colors the name and renders the letter badge', async () => {
    mockFs(() => ({
      type: 'fs_listing',
      requested: '/proj',
      path: '/proj',
      parent: '/',
      entries: [
        { name: 'changed.rs', kind: 'file', git: 'modified' },
        { name: 'fresh.ts', kind: 'file', git: 'untracked' },
        { name: 'staged.txt', kind: 'file', git: 'added' },
        { name: 'hot', kind: 'dir', git: 'conflicted' },
        { name: 'clean.rs', kind: 'file' },
      ],
    }));
    render(<Explorer key="/proj" />);
    await waitFor(() => expect(screen.getByTitle('/proj/changed.rs (modified)')).toBeTruthy());

    // Badges carry the letter + the full-word aria-label (the row title adds
    // the same word in parentheses).
    const mod = screen.getByLabelText('git: modified');
    expect(mod.textContent).toBe('M');
    expect(screen.getByLabelText('git: untracked').textContent).toBe('U');
    expect(screen.getByLabelText('git: added').textContent).toBe('A');
    expect(screen.getByLabelText('git: merge conflict').textContent).toBe('C');

    // The name itself carries the status color class; clean entries stay muted.
    const changed = screen.getByTitle('/proj/changed.rs (modified)');
    expect(changed.querySelector('span.text-warn')).toBeTruthy();
    const clean = screen.getByTitle('/proj/clean.rs');
    expect(clean.querySelector('.text-warn, .text-success, .text-danger')).toBeNull();
    // Directories aggregate too (the folder glyph picks up the color).
    const hot = screen.getByTitle('/proj/hot (merge conflict)');
    expect(hot.querySelector('.text-danger')).toBeTruthy();
  });
});
