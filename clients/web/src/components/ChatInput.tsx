/**
 * ChatInput.tsx — Message composer.
 *
 * Auto-growing textarea with Enter-to-send (IME-safe), Shift+Enter newline,
 * a near-limit char counter, and a unified send/stop button.
 *
 * Provides: ChatInput
 */
import { useEffect, useRef, useState } from 'react';
import { cn } from '../lib/cn';
import { useIsMobile } from '../hooks/useIsMobile';
import { takePendingDraft } from '../services/forkDraft';
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

  // A fork's redo-turn draft: the fork's composer opens with the fork
  // point's content prefilled (the transcript copy stops before it —
  // services/forkDraft.ts). Consumed ONCE, on the fork's first mount —
  // editing the text is the fork's whole point.
  useEffect(() => {
    const draft = takePendingDraft(chatId);
    const el = inputRef.current;
    if (!draft || !el) return;
    el.value = draft;
    grow(el);
    setLen(el.value.length);
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
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
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
    <div id="input-area" className="flex flex-col">
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
