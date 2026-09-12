/**
 * failsafe.ts — global capture of unexpected failures.
 *
 * The React tree has its ErrorBoundary and the wire has its error frames,
 * but everything OUTSIDE those nets — a service promise nobody awaited, a
 * listener callback throwing — died silently before this existed. Capture
 * at the window level, log structurally, and surface ONE deduplicated
 * toast (the stack dedupes by text, so a repeating slip cannot spam; the
 * throttle window keeps the stack from re-firing it after each dismiss).
 *
 * Deliberately NOT per-error toasts: a programming slip can fire dozens of
 * rejections per second — the console carries the detail, the toast only
 * carries the fact that something went wrong.
 *
 * Provides: installFailsafe (idempotent)
 * Depends: core/state.ts, logger.ts
 */
import { useFlux } from './state';
import { log } from '../logger';

/** Minimum spacing between "something went wrong" toasts. */
const TOAST_THROTTLE_MS = 30_000;

let lastToastAt = 0;
let installed = false;
/** Live listener refs — the reset hook must be able to REMOVE them, or
 * repeated installs (tests) accumulate duplicate listeners. */
let rejectionHandler: ((e: PromiseRejectionEvent) => void) | null = null;
let errorHandler: ((e: ErrorEvent) => void) | null = null;

function detail(reason: unknown): string {
  if (reason instanceof Error) return reason.stack ?? reason.message;
  return String(reason);
}

function report(what: string, why: unknown): void {
  log.error(`${what}: ${detail(why)}`);
  const now = Date.now();
  if (now - lastToastAt < TOAST_THROTTLE_MS) return;
  lastToastAt = now;
  useFlux.getState().pushToast('error', 'An unexpected error occurred — see the console log');
}

/** Install the window-level capture. Idempotent (tests re-run mount). */
export function installFailsafe(): void {
  if (installed) return;
  installed = true;
  rejectionHandler = (e) => report('unhandled rejection', e.reason);
  errorHandler = (e) => {
    // Resource-loading errors (img/script) also surface here with no
    // error object — report only the ones carrying one.
    if (e.error || e.message) report('uncaught error', e.error ?? e.message);
  };
  window.addEventListener('unhandledrejection', rejectionHandler);
  window.addEventListener('error', errorHandler);
}

/** Test hook — remove the listeners + reset the throttle. */
export function _resetFailsafeForTest(): void {
  if (rejectionHandler) window.removeEventListener('unhandledrejection', rejectionHandler);
  if (errorHandler) window.removeEventListener('error', errorHandler);
  rejectionHandler = null;
  errorHandler = null;
  installed = false;
  lastToastAt = 0;
}
