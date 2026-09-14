/**
 * artifacts.test.ts — the round-artifacts fold (F-11): the declarative
 * extraction table, dedupe semantics, round boundaries (user message
 * resets; history rebuild from the last user message), and the
 * reconnect clear.
 */
import { describe, it, expect, beforeEach } from 'vitest';
import { resetFluxForTest, useFlux } from '../../core/state';
import {
  recordToolStart,
  resetRound,
  rebuildRoundFromHistory,
  clearAllRounds,
  jumpToToolCall,
} from '../../services/artifacts';
import type { HistoryMessage, RoundArtifact } from '../../core/types';

function artifactsOf(chatId: string): RoundArtifact[] {
  return useFlux.getState().roundArtifacts[chatId] ?? [];
}

describe('round artifacts', () => {
  beforeEach(() => {
    resetFluxForTest();
  });

  it('file tools extract the path argument with their change kind', () => {
    recordToolStart('c1', 't1', 'write_file', JSON.stringify({ path: '/w/a.txt', content: 'x' }));
    recordToolStart('c1', 't2', 'edit_file', JSON.stringify({ path: '/w/b.txt', old: 'a', new: 'b' }));
    recordToolStart('c1', 't3', 'replace_lines', JSON.stringify({ path: '/w/c.txt' }));
    const list = artifactsOf('c1');
    expect(list).toEqual([
      { kind: 'file', callId: 't1', target: '/w/a.txt', change: 'write' },
      { kind: 'file', callId: 't2', target: '/w/b.txt', change: 'edit' },
      { kind: 'file', callId: 't3', target: '/w/c.txt', change: 'edit' },
    ]);
  });

  it('repeated touches of the same file keep ONE entry at the latest call', () => {
    recordToolStart('c1', 't1', 'edit_file', JSON.stringify({ path: '/w/a.txt' }));
    recordToolStart('c1', 't2', 'write_file', JSON.stringify({ path: '/w/a.txt' }));
    const list = artifactsOf('c1');
    expect(list).toHaveLength(1);
    expect(list[0].callId).toBe('t2');
    expect(list[0].change).toBe('write');
  });

  it('bash records the command as a shell label; long commands truncate', () => {
    recordToolStart('c1', 't1', 'bash', JSON.stringify({ command: 'cargo test -p flux-server' }));
    recordToolStart('c1', 't2', 'bash', JSON.stringify({ command: 'x'.repeat(200) }));
    const list = artifactsOf('c1');
    expect(list[0]).toMatchObject({ kind: 'tool', source: 'shell', target: 'cargo test -p flux-server' });
    expect(list[1].target.length).toBe(121);
    expect(list[1].target.endsWith('…')).toBe(true);
  });

  it('read-only and plumbing built-ins are noise — never listed', () => {
    for (const name of [
      'read_file',
      'list_directory',
      'glob',
      'grep',
      'state_get',
      'state_set',
      'buf_read',
      'question',
      'skill_list',
      'skill_read',
    ]) {
      recordToolStart('c1', 't', name, JSON.stringify({ path: '/w/x' }));
    }
    expect(artifactsOf('c1')).toEqual([]);
  });

  it('an unknown tool name is an MCP invocation; a file-arg-bearing one still lists only its name', () => {
    recordToolStart('c1', 't1', 'some_mcp_server_tool', JSON.stringify({ path: '/w/x' }));
    expect(artifactsOf('c1')).toEqual([
      { kind: 'tool', callId: 't1', target: 'some_mcp_server_tool', source: 'mcp' },
    ]);
  });

  it('malformed JSON args degrade to no entry — never a crash', () => {
    recordToolStart('c1', 't1', 'write_file', '{"path": truncated');
    recordToolStart('c1', 't2', 'write_file', JSON.stringify({ content: 'no path key' }));
    expect(artifactsOf('c1')).toEqual([]);
  });

  it('a user message resets the round; chats are isolated', () => {
    recordToolStart('c1', 't1', 'write_file', JSON.stringify({ path: '/w/a.txt' }));
    recordToolStart('c2', 't2', 'write_file', JSON.stringify({ path: '/w/b.txt' }));
    resetRound('c1');
    expect(artifactsOf('c1')).toEqual([]);
    expect(artifactsOf('c2')).toHaveLength(1);
    // Resetting an empty chat is a no-op (no store churn).
    const snap = useFlux.getState().roundArtifacts;
    resetRound('c1');
    expect(useFlux.getState().roundArtifacts).toBe(snap);
  });

  it('history rebuild starts at the LAST user message and replaces', () => {
    const messages: HistoryMessage[] = [
      { role: 'user', content: 'first' },
      {
        role: 'assistant',
        content: '',
        tool_calls: [{ id: 'old', name: 'write_file', arguments: JSON.stringify({ path: '/w/old.txt' }) }],
      },
      { role: 'tool', content: 'ok', tool_call_id: 'old' },
      { role: 'user', content: 'second' },
      {
        role: 'assistant',
        content: '',
        tool_calls: [{ id: 'new', name: 'edit_file', arguments: JSON.stringify({ path: '/w/new.txt' }) }],
      },
    ];
    // A stale live list is replaced, not merged.
    recordToolStart('c1', 'live', 'bash', JSON.stringify({ command: 'echo stale' }));
    rebuildRoundFromHistory('c1', messages);
    expect(artifactsOf('c1')).toEqual([
      { kind: 'file', callId: 'new', target: '/w/new.txt', change: 'edit' },
    ]);
    // No user message at all → no round.
    rebuildRoundFromHistory('c1', [{ role: 'assistant', content: 'hi' }]);
    expect(artifactsOf('c1')).toEqual([]);
  });

  it('reconnect clears every chat; jump with no pane is a no-op', () => {
    recordToolStart('c1', 't1', 'write_file', JSON.stringify({ path: '/w/a.txt' }));
    clearAllRounds();
    expect(useFlux.getState().roundArtifacts).toEqual({});
    expect(() => jumpToToolCall('c1', 't1')).not.toThrow();
  });
});
