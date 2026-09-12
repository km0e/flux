/**
 * models.test.ts — the saved-model registry client over Connect: the
 * service delegates onto core/grpc's typed calls; the broadcast handler
 * remains the authoritative list writer; the ContextMeter's lookup order
 * (saved params > probed catalog) is pinned.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { useFlux, resetFluxForTest } from '../../core/state';
import {
  fetchModels,
  saveModel,
  removeModel,
  syncModels,
  handleModelsFrame,
  effectiveContextLength,
} from '../../services/models';
import * as grpc from '../../core/grpc';

vi.mock('../../core/grpc', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../core/grpc')>()),
  grpcFetchModels: vi.fn(async () => undefined),
  grpcSaveModel: vi.fn(async () => ({ error: undefined })),
  grpcRemoveModel: vi.fn(async () => undefined),
  grpcSyncModels: vi.fn(async () => ({ updated: 0, error: undefined })),
}));

describe('models service', () => {
  beforeEach(() => {
    resetFluxForTest();
    vi.clearAllMocks();
  });

  it('fetchModels delegates (the fresh list lands via the stream broadcast)', () => {
    fetchModels();
    expect(vi.mocked(grpc.grpcFetchModels)).toHaveBeenCalled();
  });

  it('saveModel delegates row + params; enriched flag passthrough', async () => {
    vi.mocked(grpc.grpcSaveModel).mockResolvedValueOnce({ enriched: true, error: undefined });
    await expect(saveModel('main', 'gpt-4o', { temperature: 0.7 })).resolves.toEqual({
      enriched: true,
      error: undefined,
    });
    expect(vi.mocked(grpc.grpcSaveModel)).toHaveBeenCalledWith('main', 'gpt-4o', {
      temperature: 0.7,
    });
  });

  it('a rejected save resolves the inline error', async () => {
    vi.mocked(grpc.grpcSaveModel).mockResolvedValueOnce({
      error: 'temperature must be within 0..=2',
    });
    const r = await saveModel('main', 'gpt-4o', {});
    expect(r.error).toContain('0..=2');
  });

  it('removeModel delegates the inline error; unknown row included', async () => {
    vi.mocked(grpc.grpcRemoveModel).mockResolvedValueOnce('unknown model: main/gpt-4o');
    await expect(removeModel('main', 'gpt-4o')).resolves.toBe('unknown model: main/gpt-4o');
  });

  it('syncModels carries the scope (both / provider / one row)', async () => {
    await syncModels();
    expect(vi.mocked(grpc.grpcSyncModels)).toHaveBeenLastCalledWith(undefined, undefined);
    await syncModels('main');
    expect(vi.mocked(grpc.grpcSyncModels)).toHaveBeenLastCalledWith('main', undefined);
    await syncModels('main', 'gpt-4o');
    expect(vi.mocked(grpc.grpcSyncModels)).toHaveBeenLastCalledWith('main', 'gpt-4o');
  });

  it('the models broadcast REPLACES savedModels (authoritative list, not a merge)', () => {
    handleModelsFrame([
      { provider: 'main', model: 'gpt-4o', params: { temperature: 0.7 }, meta: {} },
      { provider: 'main', model: 'o1', params: {}, meta: { reasoning: true } },
    ]);
    expect(useFlux.getState().savedModels).toHaveLength(2);
    handleModelsFrame([]);
    expect(useFlux.getState().savedModels).toHaveLength(0);
  });

  it('effectiveContextLength prefers the saved row over the probed catalog', () => {
    useFlux.setState({
      savedModels: [
        { provider: 'main', model: 'gpt-4o', params: { context_length: 65536 }, meta: {} },
      ],
      providerModels: { main: [{ id: 'gpt-4o', context_length: 128000 }] },
    });
    // Saved params win — the user's deployment truth.
    expect(effectiveContextLength('main', 'gpt-4o')).toBe(65536);
    // Unsaved model: the probed catalog answers.
    expect(effectiveContextLength('main', 'o3')).toBeUndefined();
    useFlux.setState({
      providerModels: { main: [{ id: 'o3', context_length: 200000 }] },
    });
    expect(effectiveContextLength('main', 'o3')).toBe(200000);
    // Neither source: undefined (the meter hides — never a guess).
    expect(effectiveContextLength('main', 'nope')).toBeUndefined();
  });
});
