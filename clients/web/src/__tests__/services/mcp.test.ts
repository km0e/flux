/**
 * mcp.test.ts — the MCP launch-list client over Connect: delegation plus
 * the store write the `mcp_servers` broadcast owns.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { resetFluxForTest } from '../../core/state';
import { fetchMcpServers, addMcpServer, removeMcpServer } from '../../services/mcp';
import * as grpc from '../../core/grpc';

vi.mock('../../core/grpc', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../core/grpc')>()),
  grpcFetchMcpServers: vi.fn(async () => undefined),
  grpcAddMcpServer: vi.fn(async () => undefined),
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

  it('addMcpServer delegates with the launch triple (the inline error rides the promise)', async () => {
    await expect(
      addMcpServer({ id: 'fs', command: 'npx', args: ['-y', '@mcp/fs'], env: { TOKEN: 'v' } }),
    ).resolves.toBeUndefined();
    expect(vi.mocked(grpc.grpcAddMcpServer)).toHaveBeenCalledWith({
      id: 'fs',
      command: 'npx',
      args: ['-y', '@mcp/fs'],
      env: { TOKEN: 'v' },
    });
    vi.mocked(grpc.grpcAddMcpServer).mockResolvedValueOnce('failed to spawn');
    await expect(addMcpServer({ id: 'x', command: 'x', args: [], env: {} })).resolves.toBe(
      'failed to spawn',
    );
  });

  it('removeMcpServer delegates the inline error; unknown id included', async () => {
    vi.mocked(grpc.grpcRemoveMcpServer).mockResolvedValueOnce('unknown MCP server id: gone');
    await expect(removeMcpServer('gone')).resolves.toBe('unknown MCP server id: gone');
  });
});
