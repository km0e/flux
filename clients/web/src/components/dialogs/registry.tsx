/**
 * registry.tsx — dialog implementations: register the real UI behind the
 * promise-shaped services/dialogs API. Called once at mount.
 *
 * Confirm/NewChat render Radix dialogs into a document-body overlay host
 * (each with its own React root); the question renders as an inline card
 * inside the ACTIVE chat pane (the answer context stays visible in the
 * conversation flow).
 *
 * Provides: registerDialogs
 * Depends: services/dialogs.ts, components/dialogs/{ConfirmDialog,
 *          NewChatDialog,QuestionCard}
 */
import { createRoot, type Root } from 'react-dom/client';
import {
  setDialogImpls,
  QUESTION_DISMISSED,
  registerPendingQuestion,
  unregisterPendingQuestion,
} from '../../services/dialogs';
import { getPaneIfExists, ensurePane } from '../../services/panes';
import { useFlux } from '../../core/state';
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
 * returned close unmounts + removes. The unmount is immediate BY DESIGN:
 * DialogContent defines only an entrance animation (no data-[state=closed]
 * rules), so there is no exit animation to preserve — an unmount delay
 * would only leave a dead dialog on screen for a frame window. Revisit
 * together with any future exit-animation work (control the `open` prop,
 * unmount after the transition). */
function mountOverlay(node: (close: () => void) => React.ReactElement): OverlayHandle {
  const host = overlay();
  const container = document.createElement('div');
  host.appendChild(container);
  const root: Root = createRoot(container);
  const close = () => {
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
        // Inline card in the chat's OWN pane — found by chat id, NOT by a
        // DOM-order guess: panes hide with inline `display:none` (never the
        // `hidden` attribute), so a `.chat-pane:not([hidden])` query hits
        // the FIRST-created pane — the card used to materialize inside a
        // background conversation, invisible (B10). A missing pane still
        // gets one created (the answer must be answerable when the user
        // arrives); if the pane host itself doesn't exist (pre-render),
        // fall back to a toast + the dismissal answer instead of silence.
        const pane =
          getPaneIfExists(chatId) ??
          (document.getElementById('messages-wrap') ? ensurePane(chatId) : null);
        if (!pane) {
          useFlux.getState().pushToast(
            'error',
            'The assistant asked a question, but no conversation surface exists to answer it in',
          );
          resolve(DISMISSED);
          return;
        }
        // Discoverability: a question for a BACKGROUND chat lands in that
        // chat's pane — visible on switch, but nothing calls for attention
        // until then.
        if (useFlux.getState().activeChatId !== chatId) {
          const name = useFlux.getState().chats.find((c) => c.id === chatId)?.name;
          useFlux.getState().pushToast(
            'info',
            `The assistant asked a question in “${name || chatId}” — switch to it to answer`,
          );
        }
        const card = document.createElement('div');
        card.className = 'fx-question-mount';
        card.id = `question-card-${chatId}`;
        let answered = false;
        const finish = (answer: string) => {
          if (answered) return;
          answered = true;
          unregisterPendingQuestion(chatId, finish);
          root.unmount();
          card.remove();
          resolve(answer);
        };
        // Registry first: a concurrent question for the same chat (or a
        // pane wipe) resolves the older pending promise via the registry.
        registerPendingQuestion(chatId, finish);
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
