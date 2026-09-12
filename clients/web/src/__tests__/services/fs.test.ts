/**
 * fs.test.ts — the fs service is a thin delegation onto the Connect fs
 * RPCs (core/grpc.ts — its wire behavior is pinned in grpc.test.ts).
 * Here: the delegation wiring and the outage-marker predicate.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { listDir, readFile, isDisconnect, DISCONNECTED_ERROR } from '../../services/fs';
import * as grpc from '../../core/grpc';
import { resetFluxForTest } from '../../core/state';

describe('services/fs', () => {
  beforeEach(() => {
    resetFluxForTest();
    vi.restoreAllMocks();
  });

  it('listDir delegates to grpcListDir', async () => {
    const spy = vi
      .spyOn(grpc, 'grpcListDir')
      .mockResolvedValue({ type: 'fs_listing', requested: '/p', entries: [] });
    await listDir('/p');
    expect(spy).toHaveBeenCalledWith('/p');
  });

  it('readFile delegates to grpcReadFile', async () => {
    const spy = vi
      .spyOn(grpc, 'grpcReadFile')
      .mockResolvedValue({ type: 'fs_content', requested: '/p/a', content: 'x' });
    const r = await readFile('/p/a');
    expect(spy).toHaveBeenCalledWith('/p/a');
    expect(r.content).toBe('x');
  });

  it('isDisconnect recognizes the outage marker only', () => {
    expect(isDisconnect(DISCONNECTED_ERROR)).toBe(true);
    expect(isDisconnect('not a directory')).toBe(false);
    expect(isDisconnect(undefined)).toBe(false);
  });
});
