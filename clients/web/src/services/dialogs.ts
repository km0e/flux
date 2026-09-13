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
