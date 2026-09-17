/**
 * ChatInput.tsx — Message composer.
 *
 * Auto-growing textarea with Enter-to-send (IME-safe), Shift+Enter newline,
 * a near-limit char counter, and a unified send/stop button. `@` opens
 * file-path completion (services/mention.ts — workdir-relative, lazy
 * cached); dropping an Explorer row inserts its path at the caret.
 *
 * Provides: ChatInput
 */
import { useEffect, useRef, useState } from 'react';
import { cn } from '../lib/cn';
import { useFlux } from '../core/state';
import { useIsMobile } from '../hooks/useIsMobile';
import { takePendingDraft } from '../services/forkDraft';
import { getDraft, saveDraft, clearDraft } from '../services/drafts';
import { mentionTokenAt, completeMention, relativeToWorkdir } from '../services/mention';
import { ArrowUp, Square } from 'lucide-react';

interface ChatInputProps {
  /** The composer's chat — keys the fork-draft consume (a fork's composer
   *  opens prefilled with the fork point's content; see forkDraft.ts). */
  chatId: string;
  onSend: (text: string) => void;
  onCancel: () => void;
  disabled: boolean;
  streaming: boolean;
}

/** Upper textarea height — ~40vh, capped for absurd windows. Evaluated at
 * EVENT time, not module load: a window resize or phone rotation must
 * re-clamp against the CURRENT viewport (a frozen constant would keep the
 * auto-grow honest against a stale height). The CSS clamp
 * `max-h-[min(40dvh,320px)]` on the textarea remains the authoritative
 * visual cap; this JS value keeps style.height honest so the auto-grow
 * never fights it. */
function inputMaxHeight(): number {
  return Math.min(320, Math.round(window.innerHeight * 0.4));
}

/** Re-run the auto-grow: collapse to auto, then clamp to the scrollHeight
 * (≤ the event-time viewport cap). The one place the height dance lives —
 * the fork-draft fill, the compose-event fill, and typing all ride it. */
function grow(el: HTMLTextAreaElement): void {
  el.style.height = 'auto';
  el.style.height = Math.min(el.scrollHeight, inputMaxHeight()) + 'px';
}

/** The char counter appears only near the model's inline limit — a permanent
 * counter is noise; 8000 is the output-buffer budget of the kernel. */
const CHAR_WARN_AT = 8000;

export function ChatInput({ chatId, onSend, onCancel, disabled, streaming }: ChatInputProps): React.ReactElement {
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const [len, setLen] = useState(0);
  // The composer's chat workdir — the mention tree and drop-path
  // relativization both anchor here.
  const workdir = useFlux((s) => s.chats.find((c) => c.id === chatId)?.workdir) ?? '';
  // @-mention completion state: the token under the caret, its items,
  // the roving index. The textarea is uncontrolled (house style); the
  // popup is composer-local — Escape is consumed HERE (stopPropagation
  // shields the app-level chain; a mention popup is narrower than the
  // search bar, which is narrower than the drawer).
  const [mention, setMention] = useState<{ start: number; token: string } | null>(null);
  const [mentionItems, setMentionItems] = useState<string[]>([]);
  const [mentionActive, setMentionActive] = useState(0);
  const mentionKeyRef = useRef('');
  const mentionSeqRef = useRef(0);
  // The keyboard-hint suffix is desktop-only: at the mobile 16px composer
  // font the full placeholder wraps to a second line inside the 40px
  // single-row textarea and clips mid-glyph.
  const isMobile = useIsMobile();

  // Autofocus on mount: the composer is the primary interaction surface, and
  // the component remounts per chat switch (key={cid} in ChatView) — so every
  // switch lands the cursor here for free.
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // Draft restore on mount (the component remounts per chat switch —
  // key={cid} in ChatView — so this lands on every switch and open).
  // Priority: a fork's redo-turn draft wins (editing that text IS the
  // fork's whole point — services/forkDraft.ts); otherwise the chat's own
  // saved composer text. A consumed fork draft also drops any saved draft
  // for the id, so a stale one cannot resurrect on a later remount.
  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    const forkDraft = takePendingDraft(chatId);
    if (forkDraft) {
      el.value = forkDraft;
      grow(el);
      setLen(el.value.length);
      clearDraft(chatId);
      return;
    }
    const saved = getDraft(chatId);
    if (!saved) return;
    el.value = saved;
    grow(el);
    setLen(el.value.length);
  }, [chatId]);

  // Draft save on unmount — captures the element (React nulls the ref
  // before passive cleanups run) and the chat id (closures keep rapid
  // A→B→A switching correct). Runs once per switch-away, never per
  // keystroke; detached nodes keep their value readable.
  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    const cid = chatId;
    return () => {
      if (el.value) saveDraft(cid, el.value);
    };
  }, [chatId]);

  // A prompt suggestion (or any compose request) fills the composer. The
  // empty state lives in imperative DOM (services/panes.ts) — this window
  // event is the bridge across the React boundary.
  useEffect(() => {
    const onCompose = (e: Event) => {
      const el = inputRef.current;
      const text = (e as CustomEvent<string>).detail;
      if (!el || !text) return;
      el.value = text;
      grow(el);
      el.focus();
      el.setSelectionRange(el.value.length, el.value.length);
      setLen(el.value.length);
    };
    window.addEventListener('flux:compose', onCompose);
    return () => window.removeEventListener('flux:compose', onCompose);
  }, []);

  const send = () => {
    if (disabled) return; // disconnected — Enter must be inert
    const el = inputRef.current;
    if (!el) return;
    const text = el.value.trim();
    if (!text) return;
    el.value = '';
    el.style.height = 'auto';
    setLen(0);
    clearDraft(chatId); // sent — the pending draft is consumed
    onSend(text);
  };

  const onClick = () => {
    if (streaming) {
      onCancel();
    } else {
      send();
    }
  };

  const onInput = () => {
    const el = inputRef.current;
    if (!el) return;
    grow(el);
    setLen(el.value.length);
    updateMention(el.value, el.selectionStart ?? el.value.length);
  };

  /** Re-evaluate the @-token under the caret. A key-ref dedupes: the
   * keyup of an intercepted ArrowDown replays this with an UNCHANGED
   * caret and must not reset the roving index (or refetch). */
  const updateMention = (text: string, caret: number) => {
    const m = mentionTokenAt(text, caret);
    const key = m ? `${m.start}:${m.token}` : '';
    if (key === mentionKeyRef.current) return;
    mentionKeyRef.current = key;
    if (!m || !workdir) {
      setMention(null);
      setMentionItems([]);
      return;
    }
    setMention(m);
    setMentionActive(0);
    const seq = ++mentionSeqRef.current;
    void completeMention(workdir, m.token).then((items) => {
      if (mentionSeqRef.current !== seq) return; // a newer token won
      setMentionItems(items);
    });
  };

  const closeMention = () => {
    mentionKeyRef.current = '';
    setMention(null);
    setMentionItems([]);
  };

  /** Replace the @-token with the chosen path. Files consume the '@'
   * (the reference is complete — trailing space); directories KEEP it —
   * '@src/' stays visible, the token re-anchors on the next segment, and
   * the completion keeps drilling. */
  const insertMention = (choice: string) => {
    const el = inputRef.current;
    if (!el || !mention) return;
    const caret = el.selectionStart ?? el.value.length;
    const isDir = choice.endsWith('/');
    const head = isDir ? mention.start + 1 : mention.start;
    const next = el.value.slice(0, head) + choice + (isDir ? '' : ' ') + el.value.slice(caret);
    el.value = next;
    const pos = head + choice.length + (isDir ? 0 : 1);
    el.setSelectionRange(pos, pos);
    grow(el);
    setLen(el.value.length);
    el.focus();
    if (isDir) updateMention(el.value, pos);
    else closeMention();
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    // Completion keystrokes outrank composer chords: while the popup is
    // open, Enter/Tab INSERT (never send), arrows rove, Escape closes
    // locally (stopPropagation shields the app-level Escape chain).
    if (mention && mentionItems.length > 0) {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        setMentionActive((a) => (a + 1) % mentionItems.length);
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        setMentionActive((a) => (a - 1 + mentionItems.length) % mentionItems.length);
        return;
      }
      if (e.key === 'Enter' || e.key === 'Tab') {
        const nat = e.nativeEvent as KeyboardEvent & { keyCode: number };
        if (e.key === 'Enter' && (nat.isComposing || nat.keyCode === 229)) return;
        e.preventDefault();
        insertMention(mentionItems[mentionActive] as string);
        return;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        closeMention();
        return;
      }
    }
    if (e.key === 'Enter' && !e.shiftKey) {
      // Enter during IME composition (CJK candidate selection) confirms the
      // candidate, it does not send — keyCode 229 is the legacy
      // composition-in-progress signal; guard both.
      const nat = e.nativeEvent as KeyboardEvent & { keyCode: number };
      if (nat.isComposing || nat.keyCode === 229)
        return;
      e.preventDefault();
      // Always send: while streaming the parent decides — cancel the
      // round and interject the message (the button keeps its Stop
      // semantics, the draft stays when only a cancel is wanted).
      send();
    }
  };

  return (
    <div id="input-area" className="relative flex flex-col">
      {mention && mentionItems.length > 0 && (
        <ul
          id="mention-popup"
          role="listbox"
          aria-label="File path completion"
          className={cn(
            'absolute bottom-full left-0 z-20 mb-1 max-h-56 w-full overflow-y-auto',
            'rounded-md border border-border bg-elev p-1 shadow-[var(--fx-shadow-pop)]',
          )}
        >
          {mentionItems.map((p, i) => (
            <li
              key={p}
              role="option"
              aria-selected={i === mentionActive}
              // mousedown, not click: it must run before the textarea
              // blurs (preventDefault keeps the focus in the composer).
              onMouseDown={(e) => {
                e.preventDefault();
                insertMention(p);
              }}
              className={cn(
                'cursor-pointer truncate rounded-sm px-2.5 py-1.5 font-mono text-sm',
                i === mentionActive ? 'bg-active text-fg' : 'text-muted hover:text-fg',
              )}
            >
              {p}
            </li>
          ))}
        </ul>
      )}
      <div
        id="input-row"
        className={cn(
          'flex items-end gap-2 rounded-md border bg-inset px-3 py-2 shadow-sm',
          'border-border transition-colors duration-fast focus-within:border-accent',
          streaming && 'streaming border-accent/45 focus-within:border-accent',
        )}
      >
        <textarea
          id="input"
          ref={inputRef}
          rows={1}
          placeholder={
            isMobile ? 'Ask Flux…' : 'Ask Flux…  (Enter ↵ send · Shift+Enter newline)'
          }
          aria-description="Enter to send, Shift+Enter for newline, Escape to cancel"
          onKeyDown={onKeyDown}
          onInput={onInput}
          onKeyUp={() => {
            const el = inputRef.current;
            if (!el) return;
            updateMention(el.value, el.selectionStart ?? el.value.length);
          }}
          onDragOver={(e) => {
            // Accept only Explorer-row drags (text/plain paths).
            if (e.dataTransfer.types.includes('text/plain')) e.preventDefault();
          }}
          onDrop={(e) => {
            const abs = e.dataTransfer.getData('text/plain');
            if (!abs) return;
            e.preventDefault();
            const rel = relativeToWorkdir(abs, workdir);
            if (!rel) return;
            const el = inputRef.current;
            if (!el) return;
            const caret = el.selectionStart ?? el.value.length;
            el.value = el.value.slice(0, caret) + rel + ' ' + el.value.slice(caret);
            const pos = caret + rel.length + 1;
            el.setSelectionRange(pos, pos);
            grow(el);
            setLen(el.value.length);
            el.focus();
          }}
          disabled={disabled}
          // `max-md:text-[16px]` is the ONE deliberate exception to the type
          // scale: iOS Safari zooms any focused input whose font is under
          // 16px, which breaks the composer on every focus.
          className="max-h-[min(40dvh,320px)] min-h-7 max-md:min-h-10 flex-1 resize-none bg-transparent leading-[1.45] text-fg placeholder:text-faint focus:outline-none disabled:cursor-not-allowed max-md:text-[16px]"
        />
        <button
          id="send"
          className={cn(
            'grid size-8 max-md:size-10 shrink-0 place-items-center rounded-sm transition-all duration-fast',
            streaming
              ? 'bg-warn text-white hover:brightness-110'
              : 'bg-accent text-accent-fg hover:bg-accent-strong',
            'disabled:cursor-not-allowed disabled:opacity-40',
          )}
          onClick={onClick}
          disabled={disabled && !streaming}
          title={streaming ? 'Stop the current round (Esc)' : 'Send (Enter)'}
          aria-label={streaming ? 'Stop' : 'Send'}
        >
          {streaming ? <Square size={13} fill="currentColor" /> : <ArrowUp size={16} />}
        </button>
      </div>
      {/* The counter appears only near the limit — below it the row stays
          out of the layout entirely (noise reduction). */}
      {len > CHAR_WARN_AT && (
        <div id="input-hints" className="flex justify-end px-1 pt-1 text-2xs">
          <span id="input-count" className="text-warn">
            {len} chars — approaching the model limit
          </span>
        </div>
      )}
    </div>
  );
}
