/**
 * mcp.ts — MCP-server management client.
 *
 * The launch list lives in the server database (the server has no config
 * file); the MCP dialog lists, adds and removes entries over the
 * McpService RPCs. Mutations are persist-first and applied live: the
 * server spawns the child, registers its tools, and rebuilds the running
 * engines (an apply failure rides the response's inline error while the
 * row stays). The fresh list arrives via the `mcp_servers` stream
 * broadcast (the store handler stays authoritative).
 *
 * Provides: fetchMcpServers, addMcpServer, removeMcpServer
 * Depends: core/grpc.ts
 */
import { grpcAddMcpServer, grpcFetchMcpServers, grpcRemoveMcpServer } from '../core/grpc';
import type { McpKind, McpToolRegistration } from '../core/types';
import { log } from '../logger';

export function fetchMcpServers(): void {
  // Fire-and-forget (see providers.ts) — logged, never unhandled.
  grpcFetchMcpServers().catch((e) => log.warn('mcp fetch failed', e));
}

/** Register an MCP server (persisted to the server database — the ack
 * implies durability). Resolves with the inline error plus the per-tool
 * registration outcomes (a skipped tool carries its reason), or an empty
 * set on failure. The fresh list arrives via the `mcp_servers` broadcast.
 * Empty args/env are omitted on the wire. */
export function addMcpServer(input: {
  id: string;
  kind: McpKind;
  command: string;
  args: string[];
  env: Record<string, string>;
  url: string;
  headers: Record<string, string>;
}): Promise<{ error?: string; results: McpToolRegistration[] }> {
  return grpcAddMcpServer(input);
}

/** Remove a registered MCP server. Resolves with the inline error, or
 * undefined on success. */
export function removeMcpServer(id: string): Promise<string | undefined> {
  return grpcRemoveMcpServer(id);
}

