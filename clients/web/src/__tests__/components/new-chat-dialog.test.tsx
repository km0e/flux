/**
 * new-chat-dialog.test.tsx — the picker's offline behavior: a listing that
 * cannot reach the server stays INLINE (no context-free toast) and retries
 * on its own once the connection is back.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, act } from '@testing-library/react';
import { NewChatDialog } from '../../components/dialogs/NewChatDialog';
import { useFlux, resetFluxForTest } from '../../core/state';
import { listDir } from '../../services/fs';
import { DISCONNECTED_ERROR } from '../../core/grpc';
import type { FsListing } from '../../core/types';

vi.mock('../../services/fs', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../services/fs')>()),
  listDir: vi.fn(),
}));

// The ProviderPicker's registry fetch is a different surface (the tests
// seed the store instead) — stub it so no real fetch escapes.
vi.mock('../../services/providers', () => ({ fetchProviders: vi.fn() }));
vi.mock('../../services/models', () => ({ fetchModels: vi.fn() }));

const listingFor = (path: string): FsListing => ({
  type: 'fs_listing',
  requested: path,
  path: path || '/home',
  parent: '/',
  entries: [{ name: 'proj', kind: 'dir' }],
});

/** The listDir calls among the mock's invocations. */
function listCalls(): unknown[] {
  return vi.mocked(listDir).mock.calls.map((c) => c[0]);
}
describe('NewChatDialog offline behavior', () => {
  beforeEach(() => {
    resetFluxForTest();
    vi.mocked(listDir).mockReset();
    document.body.innerHTML = '';
  });

  it('opened while offline: inline outage note, NO toast', async () => {
    // Store left disconnected by resetFluxForTest; the RPC layer resolves
    // with the outage marker.
    vi.mocked(listDir).mockResolvedValue({
      type: 'fs_listing',
      requested: '',
      error: DISCONNECTED_ERROR,
      entries: [],
    });
    render(<NewChatDialog onCreate={() => {}} onCancel={() => {}} />);
    await waitFor(() => {
      expect(screen.getByText(/Server disconnected — the listing retries/)).toBeTruthy();
    });
    // No context-free error toast fired: the top bar owns outage
    // communication.
    expect(useFlux.getState().toasts).toHaveLength(0);
  });

  it('retries the listing automatically when the connection comes back', async () => {
    vi.mocked(listDir).mockResolvedValue({
      type: 'fs_listing',
      requested: '',
      error: DISCONNECTED_ERROR,
      entries: [],
    });
    render(<NewChatDialog onCreate={() => {}} onCancel={() => {}} />);
    await waitFor(() => {
      expect(screen.getByText(/Server disconnected — the listing retries/)).toBeTruthy();
    });
    // Connection restored — the self-heal re-lists without user action.
    vi.mocked(listDir).mockResolvedValue(listingFor('/home'));
    act(() => {
      useFlux.setState({ connectionStatus: 'connected' });
    });
    await waitFor(() => {
      expect(screen.getByText('proj')).toBeTruthy();
    });
    expect(useFlux.getState().toasts).toHaveLength(0);
  });
});

describe('NewChatDialog inheritance', () => {
  beforeEach(() => {
    resetFluxForTest();
    vi.mocked(listDir).mockReset();
    document.body.innerHTML = '';
  });

  it('seeds the provider pin, model, and workdir from the last opened chat', async () => {
    vi.mocked(listDir).mockImplementation(async (path?: string) => listingFor(path ?? ''));
    useFlux.setState({
      connectionStatus: 'connected',
      providers: [{ id: 'alpha', url: 'https://a/v1' }],
      // A saved row for the seeded pin keeps the Model label suffix-free
      // (the "import in Providers" pointer is no-saved-rows-only).
      savedModels: [{ provider: 'alpha', model: 'm-a', params: {}, meta: {} }],
      chats: [
        {
          id: 'c1',
          name: 'Prev',
          createdAt: 1,
          active: false,
          workdir: '/tmp/proj',
          provider: 'alpha',
          model: 'm-a',
        },
      ],
      activeChatId: 'c1',
    });
    render(<NewChatDialog onCreate={() => {}} onCancel={() => {}} />);
    // The FIRST listing targets the seed chat's workdir (not the server home).
    await waitFor(() => {
      expect(listCalls()[0]).toBe('/tmp/proj');
    });
    // The provider pin + model are seeded too (everything but the name).
    // (^Provider — the Model label may carry an import pointer whose text
    // also contains "Providers".)
    expect((screen.getByLabelText(/^Provider/) as HTMLSelectElement).value).toBe('alpha');
    expect((screen.getByLabelText(/^Model/) as HTMLInputElement).value).toBe('m-a');
  });

  it('a seeded pin whose provider vanished resets to the prompt', () => {
    vi.mocked(listDir).mockResolvedValue(listingFor(''));
    useFlux.setState({
      providers: [{ id: 'other', url: 'https://o/v1' }],
      chats: [
        {
          id: 'c1',
          name: 'Prev',
          createdAt: 1,
          active: true,
          workdir: '/tmp/proj',
          provider: 'gone',
          model: 'm-x',
        },
      ],
      activeChatId: 'c1',
    });
    render(<NewChatDialog onCreate={() => {}} onCancel={() => {}} />);
    // The select cannot hold a value its options don't carry — the pin
    // clears so Create stays gated on an explicit pick.
    expect((screen.getByLabelText(/^Provider/) as HTMLSelectElement).value).toBe('');
    expect((screen.getByLabelText(/^Model/) as HTMLInputElement).value).toBe('');
  });
});
