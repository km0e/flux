/**
 * impl.tsx — dialog implementations: register the real UI behind the
 * promise-shaped services/dialogs API. Called once at mount.
 *
 * Confirm/NewChat render Radix dialogs into a document-body overlay host
 * (each with its own React root); the question renders as an inline card
 * inside the ACTIVE chat pane (the answer context stays visible in the
 * conversation flow).
 *
 * Provides: registerDialogs
 * Depends: services/dialogs.ts, components/dialogs/*
 */
import { createRoot, type Root } from 'react-dom/client';
import { setDialogImpls, QUESTION_DISMISSED } from '../../services/dialogs';
import { ConfirmDialog } from './ConfirmDialog';
import { NewChatDialog } from './NewChatDialog';
import { QuestionCard } from './QuestionCard';

/** Overlay host element for modals — created lazily, reused. */
function overlay(): HTMLDivElement {
  let el = document.getElementById('fx-web-overlay') as HTMLDivElement | null;
  if (!el) {
    el = document.createElement('div');
    el.id = 'fx-web-overlay';
    document.body.appendChild(el);
  }
  return el;
}

interface OverlayHandle {
  close: () => void;
}

/** Mount a modal component into the overlay with its own React root; the
 * returned close unmounts + removes. */
function mountOverlay(node: (close: () => void) => React.ReactElement): OverlayHandle {
  const host = overlay();
  const container = document.createElement('div');
  host.appendChild(container);
  const root: Root = createRoot(container);
  const close = () => {
    // Unmount is async in React 18+; removal must wait or the dialog's
    // exit animation DOM vanishes mid-flight — acceptable for dialogs.
    root.unmount();
    container.remove();
  };
  root.render(node(close));
  return { close };
}

const DISMISSED = QUESTION_DISMISSED;

export function registerDialogs(): void {
  setDialogImpls({
    confirmDelete: (chatName: string) =>
      new Promise<boolean>((resolve) => {
        mountOverlay((close) => (
          <ConfirmDialog
            title="Delete conversation"
            message={`Delete “${chatName}”? The conversation and its history are removed permanently.`}
            confirmLabel="Delete"
            onConfirm={() => {
              close();
              resolve(true);
            }}
            onCancel={() => {
              close();
              resolve(false);
            }}
          />
        ));
      }),

    confirmRebase: (snippet: string) =>
      new Promise<boolean>((resolve) => {
        mountOverlay((close) => (
          <ConfirmDialog
            title="Archive context up to here"
            message={
              'This message and everything before it will be archived out of the model’s context ' +
              '(messages after it stay). The chat history remains viewable, but this cannot be ' +
              'undone. A round in flight finishes first.' +
              (snippet ? `\n\n“${snippet}”` : '')
            }
            confirmLabel="Archive & restart"
            onConfirm={() => {
              close();
              resolve(true);
            }}
            onCancel={() => {
              close();
              resolve(false);
            }}
          />
        ));
      }),

    pickNewChat: () =>
      new Promise((resolve) => {
        mountOverlay((close) => (
          <NewChatDialog
            onCancel={() => {
              close();
              resolve(null);
            }}
            onCreate={(choice) => {
              close();
              resolve(choice);
            }}
          />
        ));
      }),

    askQuestion: (chatId: string, q) =>
      new Promise<string>((resolve) => {
        // Inline card in the ACTIVE chat pane — the conversation context
        // (why the model is asking) stays visible while answering.
        const pane = document.querySelector('.chat-pane:not([hidden])');
        if (!pane) {
          resolve(DISMISSED);
          return;
        }
        const card = document.createElement('div');
        card.className = 'fx-question-mount';
        card.id = 'question-card';
        let answered = false;
        const finish = (answer: string) => {
          if (answered) return;
          answered = true;
          root.unmount();
          card.remove();
          resolve(answer);
        };
        pane.appendChild(card);
        const root: Root = createRoot(card);
        root.render(
          <QuestionCard
            chatId={chatId}
            question={q}
            onAnswer={(answer) => finish(answer)}
            onDismiss={() => finish(DISMISSED)}
          />,
        );
        // The shared follow machine drives the render pipeline; the card is
        // outside it, so nudge it into view directly (guarded: not everywhere
        // implements scrollIntoView — jsdom doesn't).
        card.scrollIntoView?.({ block: 'nearest' });
      }),
  });
}
