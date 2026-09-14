/**
 * mcp.test.ts — the MCP launch-list client over Connect: delegation plus
 * the store write the `mcp_servers` broadcast owns.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { resetFluxForTest } from '../../core/state';
import { fetchMcpServers, addMcpServer, removeMcpServer } from '../../services/mcp';
import type { McpToolRegistration } from '../../core/types';
import * as grpc from '../../core/grpc';

vi.mock('../../core/grpc', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../core/grpc')>()),
  grpcFetchMcpServers: vi.fn(async () => undefined),
  grpcAddMcpServer: vi.fn(async () => ({ results: [] as McpToolRegistration[] })),
  grpcRemoveMcpServer: vi.fn(async () => undefined),
}));

describe('mcp service', () => {
  beforeEach(() => {
    resetFluxForTest();
    vi.clearAllMocks();
  });

  it('fetchMcpServers delegates', () => {
    fetchMcpServers();
    expect(vi.mocked(grpc.grpcFetchMcpServers)).toHaveBeenCalled();
  });

  it('addMcpServer delegates with the launch triple (the inline error + per-tool results ride the promise)', async () => {
    await expect(
      addMcpServer({ id: 'fs', command: 'npx', args: ['-y', '@mcp/fs'], env: { TOKEN: 'v' } }),
    ).resolves.toEqual({ results: [] });
    expect(vi.mocked(grpc.grpcAddMcpServer)).toHaveBeenCalledWith({
      id: 'fs',
      command: 'npx',
      args: ['-y', '@mcp/fs'],
      env: { TOKEN: 'v' },
    });
    vi.mocked(grpc.grpcAddMcpServer).mockResolvedValueOnce({
      error: 'failed to spawn',
      results: [],
    });
    await expect(addMcpServer({ id: 'x', command: 'x', args: [], env: {} })).resolves.toEqual({
      error: 'failed to spawn',
      results: [],
    });
  });

  it('removeMcpServer delegates the inline error; unknown id included', async () => {
    vi.mocked(grpc.grpcRemoveMcpServer).mockResolvedValueOnce('unknown MCP server id: gone');
    await expect(removeMcpServer('gone')).resolves.toBe('unknown MCP server id: gone');
  });
});
