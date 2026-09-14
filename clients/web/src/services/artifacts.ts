/**
 * artifacts.ts — the current round's artifacts (F-11).
 *
 * One chat's round (since its last user message) touches files and runs
 * tools; this module folds the stream's `tool_start` events into a small
 * per-chat list the dock's Round tab renders — files one click from
 * preview, invocations one click from their card in the stream. The
 * extraction is DECLARATIVE (a per-tool-name table), never a guess: a
 * tool whose shape is unknown degrades to a name/label line, and a bad
 * JSON argument degrades to no entry — the list is a convenience, never
 * a truth source (the transcript is).
 *
 * Round boundaries: a user message resets the list (appendUserMessage /
 * appendInterjectedMessage); a history snapshot rebuilds it from the
 * last user message onward (renderHistoryMessages) — idempotent, a
 * re-open re-renders the same tail.
 *
 * Provides: recordToolStart, resetRound, rebuildRoundFromHistory,
 *           clearAllRounds, jumpToToolCall
 * Depends: core/state.ts, core/types.ts, services/panes.ts, lib/dom.ts
 */
import { useFlux } from '../core/state';
import type { HistoryMessage, RoundArtifact } from '../core/types';
import { getPaneIfExists } from './panes';
import { escapeCssSelector } from '../lib/dom';

/** File-touching tools and how they touch: `write` creates/overwrites,
 * `edit` modifies in place (edit_file / replace_lines). */
const FILE_TOOLS: Record<string, 'write' | 'edit'> = {
  write_file: 'write',
  edit_file: 'edit',
  replace_lines: 'edit',
};

/** Built-ins that are read-only or state plumbing — listing them is
 * noise (the artifacts list is about what the round PRODUCED). */
const IGNORED_TOOLS = new Set([
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
]);

/** Shell commands are labels here — truncate to keep one row one row. */
const LABEL_MAX = 120;

function stringArg(argsJson: string, key: string): string | undefined {
  try {
    const v = JSON.parse(argsJson)?.[key];
    return typeof v === 'string' && v.trim() !== '' ? v : undefined;
  } catch {
    // Malformed model output — no entry, no crash.
    return undefined;
  }
}

/** Fold one tool_start into the chat's round list. Dedupe: a file
 * touched repeatedly keeps ONE entry (the call id updates to the latest
 * touch — the jump lands on the most recent edit); the same shell
 * command / MCP tool likewise. */
export function recordToolStart(chatId: string, callId: string, name: string, args: string): void {
  let artifact: RoundArtifact | null = null;
  const change = FILE_TOOLS[name];
  if (change) {
    const path = stringArg(args, 'path');
    if (path) artifact = { kind: 'file', callId, target: path, change };
  } else if (name === 'bash') {
    const command = stringArg(args, 'command');
    if (command) {
      const label = command.length > LABEL_MAX ? command.slice(0, LABEL_MAX) + '…' : command;
      artifact = { kind: 'tool', callId, target: label, source: 'shell' };
    }
  } else if (!IGNORED_TOOLS.has(name)) {
    // Anything else is an MCP tool (the built-in surface is the table
    // above) — the invocation name is the entry.
    artifact = { kind: 'tool', callId, target: name, source: 'mcp' };
  }
  if (!artifact) return;

  const key = (a: RoundArtifact) => (a.kind === 'file' ? `f:${a.target}` : `t:${a.target}`);
  const all = useFlux.getState().roundArtifacts;
  const list = all[chatId] ?? [];
  const idx = list.findIndex((a) => key(a) === key(artifact!));
  const next =
    idx >= 0 ? list.map((a, i) => (i === idx ? artifact! : a)) : [...list, artifact];
  useFlux.setState({ roundArtifacts: { ...all, [chatId]: next } });
}

/** A new user message starts a round — the previous round's list retires. */
export function resetRound(chatId: string): void {
  const all = useFlux.getState().roundArtifacts;
  if (!all[chatId]) return;
  const next = { ...all };
  delete next[chatId];
  useFlux.setState({ roundArtifacts: next });
}

/** Rebuild from a history snapshot: the LAST user message starts the
 * current round — its tool_calls tail folds through the same recorder
 * (dedupe included). Replaces wholesale, so a re-open is idempotent. */
export function rebuildRoundFromHistory(chatId: string, messages: HistoryMessage[]): void {
  let lastUser = -1;
  messages.forEach((m, i) => {
    if (m.role === 'user') lastUser = i;
  });
  resetRound(chatId);
  if (lastUser < 0) return;
  for (const msg of messages.slice(lastUser + 1)) {
    for (const tc of msg.tool_calls ?? []) {
      recordToolStart(chatId, tc.id, tc.name, tc.arguments);
    }
  }
}

/** Reconnect reset: the server-side loops died — the round lists restart
 * with the re-open (each claim's history rebuild repopulates). */
export function clearAllRounds(): void {
  useFlux.setState({ roundArtifacts: {} });
}

/** Scroll the artifact's tool card into view and pulse it once — the
 * "one click from retrospection" half of the contract (files jump to a
 * preview tab instead). */
export function jumpToToolCall(chatId: string, callId: string): void {
  const el = getPaneIfExists(chatId)?.querySelector(
    `[data-tool-call-id="${escapeCssSelector(callId)}"]`,
  ) as HTMLElement | null;
  if (!el) return;
  el.scrollIntoView({ behavior: 'smooth', block: 'center' });
  el.classList.remove('artifact-flash');
  // Force a reflow so a repeat click restarts the animation.
  void el.offsetWidth;
  el.classList.add('artifact-flash');
}
