/**
 * main.tsx — browser entry point.
 *
 * Derives the WS endpoint from the page origin (same-origin `/ws` on the
 * port that served this page — the server hosts UI + WS on one listener)
 * and mounts the chat UI. No log transport: console only (the web page has
 * DevTools).
 */
import { mountChat } from './mount';
import { getLogLevel, setLogLevel, log, type LogLevel } from './logger';
// The stylesheet (Tailwind + tokens + stream layer) — Vite emits it as
// assets/bundle.css for the server's static contract.
import './styles/app.css';

const params = new URLSearchParams(location.search);

function init(): void {
  const startLevel = (params.get('log') as LogLevel | null) ?? 'info';
  setLogLevel(startLevel);
  log.info('web bundle loaded (origin: ' + location.host + ', log level: ' + getLogLevel() + ')');

  // Same origin: the server serves this page and the Connect surface from
  // ONE listener, so the transport's base url is exactly the page origin.
  mountChat({
    root: document.getElementById('app')!,
    logLevel: startLevel,
  });
}

init();
