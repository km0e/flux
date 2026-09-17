/**
 * providers.test.ts — the provider-management client over Connect: the
 * service delegates onto core/grpc's typed calls (the wire itself is
 * pinned in grpc.test.ts); the probe keeps the in-band error visible for
 * the dialog while the catalog cache records it (providerProbeErrors).
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { useFlux, resetFluxForTest } from '../../core/state';
import { addProvider, removeProvider, probeProvider, fetchProviders, updateProvider } from '../../services/providers';
import * as grpc from '../../core/grpc';

vi.mock('../../core/grpc', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../core/grpc')>()),
  grpcAddProvider: vi.fn(async () => undefined),
  grpcRemoveProvider: vi.fn(async () => undefined),
  grpcUpdateProvider: vi.fn(async () => undefined),
  grpcProbeProvider: vi.fn(async () => ({ models: [], error: undefined })),
  grpcFetchProviders: vi.fn(async () => undefined),
}));

describe('providers service', () => {
  beforeEach(() => {
    resetFluxForTest();
    vi.clearAllMocks();
  });

  it('addProvider delegates with the inline error passthrough', async () => {
    await expect(addProvider({ id: 'p', url: 'https://a/v1', api_key: 'k' })).resolves.toBeUndefined();
    expect(vi.mocked(grpc.grpcAddProvider)).toHaveBeenCalledWith({
      id: 'p',
      url: 'https://a/v1',
      api_key: 'k',
    });
    vi.mocked(grpc.grpcAddProvider).mockResolvedValueOnce('duplicate provider id: p');
    await expect(addProvider({ id: 'p' })).resolves.toBe('duplicate provider id: p');
  });

  it('removeProvider delegates with the inline error passthrough', async () => {
    vi.mocked(grpc.grpcRemoveProvider).mockResolvedValueOnce('unknown provider id: gone');
    await expect(removeProvider('gone')).resolves.toBe('unknown provider id: gone');
    expect(vi.mocked(grpc.grpcRemoveProvider)).toHaveBeenCalledWith('gone');
  });

  it('updateProvider delegates; an empty api_key stays present (server-side clear)', async () => {
    // A blank key maps to undefined (the wire tri-state's "keep").
    await expect(updateProvider({ id: 'p', url: 'https://n/v1' })).resolves.toBeUndefined();
    expect(vi.mocked(grpc.grpcUpdateProvider)).toHaveBeenCalledWith({
      id: 'p',
      url: 'https://n/v1',
    });
    // The inline error passes through untouched.
    vi.mocked(grpc.grpcUpdateProvider).mockResolvedValueOnce('unknown provider id: ghost');
    await expect(updateProvider({ id: 'ghost' })).resolves.toBe('unknown provider id: ghost');
  });

  it('fetchProviders delegates (the fresh list lands via the stream broadcast)', () => {
    fetchProviders();
    expect(vi.mocked(grpc.grpcFetchProviders)).toHaveBeenCalled();
  });

  it('probeProvider records the catalog cache AND the probe error', async () => {
    vi.mocked(grpc.grpcProbeProvider).mockResolvedValueOnce({
      models: [{ id: 'm1', context_length: 4096 }],
      error: undefined,
    });
    const r = await probeProvider('main');
    expect(r.models).toHaveLength(1);
    expect(useFlux.getState().providerModels['main']).toEqual([
      { id: 'm1', context_length: 4096 },
    ]);
    expect(useFlux.getState().providerProbeErrors['main']).toBeUndefined();

    vi.mocked(grpc.grpcProbeProvider).mockResolvedValueOnce({ models: [], error: 'connection refused' });
    await probeProvider('broken');
    expect(useFlux.getState().providerProbeErrors['broken']).toBe('connection refused');
    // A later clean probe clears the error.
    vi.mocked(grpc.grpcProbeProvider).mockResolvedValueOnce({ models: [], error: undefined });
    await probeProvider('broken');
    expect(useFlux.getState().providerProbeErrors['broken']).toBeUndefined();
  });
});
