/**
 * skills.ts — the Skills-management client.
 *
 * Skills are pure filesystem content on the server: the dialog lists the
 * global skills dir plus the active chat's workdir-local (project)
 * skills, installs into the global dir (a local directory path or a git
 * URL with an optional subpath), and removes global entries. An install
 * is IMMEDIATELY effective — flux-tools scans fresh on every skill_list
 * call, so there is no restart semantics (unlike MCP servers).
 *
 * Direct gRPC-Web calls (the SkillService RPCs); the fresh list arrives
 * via the `skills` stream broadcast (the store handler stays
 * authoritative).
 *
 * Provides: fetchSkills, addSkill, removeSkill, handleSkillsMessage
 * Depends: core/grpc.ts, core/state.ts
 */
import { grpcAddSkill, grpcFetchSkills, grpcRemoveSkill } from '../core/grpc';
import { log } from '../logger';
import { useFlux } from '../core/state';

/** Fetch the installed skills. With a chat id, the reply also carries
 * that chat's workdir-local (project) skills — read-only entries. The
 * `skills` reply lands in the store via handlers.ts. */
export function fetchSkills(chatId?: string): void {
  // Fire-and-forget (see providers.ts) — logged, never unhandled.
  grpcFetchSkills(chatId).catch((e) => log.warn('skill fetch failed', e));
}

/** Install a skill into the global skills dir. Exactly one source: a
 * local directory path OR a git URL (with an optional subpath for
 * multi-skill repositories). Resolves with the inline error, or
 * undefined on success (the fresh list arrives via the `skills`
 * broadcast — the install is immediately effective). */
export function addSkill(input: {
  path?: string;
  url?: string;
  subpath?: string;
}): Promise<string | undefined> {
  return grpcAddSkill(input);
}

/** Remove a skill from the global skills dir. Resolves with the inline
 * error, or undefined on success (the fresh list arrives via the `skills`
 * broadcast). */
export function removeSkill(name: string): Promise<string | undefined> {
  return grpcRemoveSkill(name);
}

/** Reply routing (called by handlers.ts): the installed-skills snapshot
 * (skills_list reply / post-mutation broadcast) fills the store. */
export function handleSkillsMessage(skills: import('../core/types').SkillSummary[]): void {
  useFlux.setState({ skills });
}
