/**
 * QuestionCard.tsx — the agent's `question` tool as an inline conversation
 * card.
 *
 * Rendered INTO the active chat pane by the dialogs impl (the answer context
 * stays visible in the conversation flow). Options render as buttons; the
 * free-form input accepts any answer. The card finishes exactly once.
 *
 * Styling rides Tailwind utilities (this is a React component, not the
 * imperative streaming DOM); the pane's `.chat-pane > * + *` rule owns
 * the outer margin.
 *
 * Provides: QuestionCard
 */
import { useEffect, useRef, useState } from 'react';
import { Button, TextField } from '../ui';
import type { ChatQuestion } from '../../services/dialogs';

export function QuestionCard(props: {
  chatId: string;
  question: ChatQuestion;
  onAnswer: (answer: string) => void;
  onDismiss: () => void;
}): React.ReactElement {
  const { question } = props;
  const inputRef = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const [draft, setDraft] = useState('');

  const send = (answer: string) => {
    if (busy || !answer.trim()) return;
    setBusy(true);
    props.onAnswer(answer.trim());
  };

  // The input is the primary affordance when there are no options; with
  // options, focus the first one (Enter answers without reaching for the mouse).
  useEffect(() => {
    if (question.options && question.options.length > 0) return;
    inputRef.current?.focus();
  }, [question.options]);

  return (
    <div
      className="flex flex-col gap-2.5 rounded-lg border border-border bg-elev p-4 shadow-[var(--fx-shadow-pop)]"
      role="form"
      aria-label="The assistant is asking a question"
    >
      <div className="flex items-start gap-2">
        <span
          aria-hidden="true"
          className="grid size-5 shrink-0 place-items-center rounded-md bg-accent-dim text-2xs font-semibold text-accent"
        >
          ?
        </span>
        <span className="min-w-0 flex-1 text-sm leading-snug text-fg break-words">
          {question.text}
        </span>
      </div>
      {question.options && question.options.length > 0 && (
        <div className="flex flex-wrap gap-2">
          {question.options.map((opt) => (
            <Button key={opt} variant="secondary" disabled={busy} onClick={() => send(opt)}>
              {opt}
            </Button>
          ))}
        </div>
      )}
      <div className="flex gap-2">
        {/* The one control language: TextField owns the input chrome. */}
        <TextField
          ref={inputRef}
          type="text"
          className="min-w-0 flex-1 disabled:opacity-45"
          placeholder="Other… type your answer"
          value={draft}
          disabled={busy}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.nativeEvent.isComposing && draft.trim()) {
              e.preventDefault();
              send(draft);
            }
          }}
        />
        <Button variant="primary" disabled={busy || !draft.trim()} onClick={() => send(draft)}>
          Answer
        </Button>
      </div>
    </div>
  );
}
