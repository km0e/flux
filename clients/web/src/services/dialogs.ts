/**
 * dialogs.ts — dialog orchestration service (the D-26 replacement for the
 * ChatHost capability contract).
 *
 * The logic layer (handlers/new-chat/panes) calls promise-shaped functions
 * here; the UI layer (components/dialogs) registers the actual renderers at
 * mount — confirm/new-chat show real modal components, askQuestion mounts an
 * inline QuestionCard into the chat pane. Tests inject stubs via
 * setDialogImpls, mirroring the old ChatHost mock pattern.
 *
 * Provides: dialogs, setDialogImpls, resetDialogsForTest, NewChatChoice
 * Depends: core/types.ts (ChatKind)
 */

export interface ChatQuestion {
  text: string;
  options?: string[];
}

export type ChatKind = 'classic' | 'feature';

/** The new-chat dialog's resolution: a kind, the browsed workdir, and the
 * REQUIRED provider+model pin (providers carry no default model) — the
 * dialog gates Create on explicit picks. */
export interface NewChatChoice {
  kind: ChatKind;
  workdir: string;
  provider: string;
  model: string;
}

export interface DialogImpls {
  /** Destructive-action confirmation (true = confirmed). */
  confirmDelete(chatName: string): Promise<boolean>;
  /** New-chat flow: kind + workdir picked in one dialog. Null = dismissed. */
  pickNewChat(): Promise<NewChatChoice | null>;
  /** The `question` tool's user prompt — options + free-form input;
   * dismissal resolves with a neutral "no answer" string. */
  askQuestion(chatId: string, q: ChatQuestion): Promise<string>;
  /** Manual context rebase confirmation (archive up to a message).
   * `snippet` is the clicked message's text (truncated). */
  confirmRebase(snippet: string): Promise<boolean>;
}

/** Answer recorded when the user dismisses the question (Esc / close). */
export const QUESTION_DISMISSED = '(no answer: the user dismissed the question)';

/** Neutral fallbacks — flows never hang when no UI is registered (tests,
 * early mount). Mirrors the old ChatHost fallback semantics. */
const fallback: DialogImpls = {
  confirmDelete: async () => false,
  pickNewChat: async () => null,
  askQuestion: async () => QUESTION_DISMISSED,
  confirmRebase: async () => false,
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
  confirmRebase(snippet: string): Promise<boolean> {
    return impls.confirmRebase(snippet);
  },
};
