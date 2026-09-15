/**
 * dialogs.ts — dialog orchestration service.
 *
 * The logic layer (handlers/new-chat/panes) calls promise-shaped functions
 * here; the UI layer (components/dialogs) registers the actual renderers at
 * mount — confirm/new-chat show real modal components, askQuestion mounts an
 * inline QuestionCard into the chat pane. Tests inject stubs via
 * setDialogImpls.
 *
 * Provides: dialogs, setDialogImpls, resetDialogsForTest, NewChatChoice
 */

export interface ChatQuestion {
  text: string;
  options?: string[];
}

/** The new-chat dialog's resolution: the browsed workdir and the REQUIRED
 * provider+model pin (providers carry no default model) — the dialog
 * gates Create on explicit picks. */
export interface NewChatChoice {
  workdir: string;
  provider: string;
  model: string;
}

export interface DialogImpls {
  /** Destructive-action confirmation (true = confirmed). */
  confirmDelete(chatName: string): Promise<boolean>;
  /** New-chat flow: workdir picked in one dialog. Null = dismissed. */
  pickNewChat(): Promise<NewChatChoice | null>;
  /** The `question` tool's user prompt — options + free-form input;
   * dismissal resolves with a neutral "no answer" string. */
  askQuestion(chatId: string, q: ChatQuestion): Promise<string>;
}

/** Answer recorded when the user dismisses the question (Esc / close). */
export const QUESTION_DISMISSED = '(no answer: the user dismissed the question)';

// ── Pending-question registry ──────────────────────────────────────────────
// The inline question card lives OUTSIDE React's tree (a DOM child of the
// chat pane), so lifecycle events that destroy the pane — a stale-pane
// resync wipe, a chat deletion — must reach the pending promise through a
// registry, or it hangs forever (the card gone, the answer never sent).
// Keyed by chat id: one question per chat at a time (the server's question
// tool is a blocking call in that chat's round).

type QuestionFinish = (answer: string) => void;
const pendingQuestions = new Map<string, QuestionFinish>();

/** Register the pending question's finisher for a chat. A still-pending
 * previous question for the same chat is dismissed first (a newer
 * question supersedes — the old card's server-side tool call is gone). */
export function registerPendingQuestion(chatId: string, finish: QuestionFinish): void {
  pendingQuestions.get(chatId)?.(QUESTION_DISMISSED);
  pendingQuestions.set(chatId, finish);
}

/** Unregister — the finisher calls this as it resolves (guarded: only its
 * own entry, so a superseding registration is never clobbered). */
export function unregisterPendingQuestion(chatId: string, finish: QuestionFinish): void {
  if (pendingQuestions.get(chatId) === finish) pendingQuestions.delete(chatId);
}

/** Resolve the chat's pending question with the dismissal answer (if one
 * is pending). Called by the pane-lifecycle paths (resync wipe, delete). */
export function dismissPendingQuestion(chatId: string): boolean {
  const finish = pendingQuestions.get(chatId);
  if (!finish) return false;
  pendingQuestions.delete(chatId);
  finish(QUESTION_DISMISSED);
  return true;
}

/** Neutral fallbacks — flows never hang when no UI is registered (tests,
 * early mount). */
const fallback: DialogImpls = {
  confirmDelete: async () => false,
  pickNewChat: async () => null,
  askQuestion: async () => QUESTION_DISMISSED,
};

let impls: DialogImpls = fallback;

/** Register dialog implementations (called once at mount). */
export function setDialogImpls(partial: Partial<DialogImpls>): void {
  impls = { ...impls, ...partial };
}

/** Restore fallbacks (test isolation). */
export function resetDialogsForTest(): void {
  impls = fallback;
}

export const dialogs = {
  confirmDelete(chatName: string): Promise<boolean> {
    return impls.confirmDelete(chatName);
  },
  pickNewChat(): Promise<NewChatChoice | null> {
    return impls.pickNewChat();
  },
  askQuestion(chatId: string, q: ChatQuestion): Promise<string> {
    return impls.askQuestion(chatId, q);
  },
};
