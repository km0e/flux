/**
 * logger.ts — leveled logging with a pluggable transport sink.
 *
 * Levels: debug < info < warn < error (default info). Messages below the
 * threshold are dropped BEFORE reaching the sink — zero sink cost. The
 * level is set at mount time and on runtime setting changes.
 *
 * The sink is injectable (an embedding layer may forward lines to its own
 * transport; the default sink writes to console). Shared UI code never
 * touches the host transport directly.
 *
 * Provides: log, setLogLevel, getLogLevel, setLogSink, LogLevel, isValidLogLevel
 */

export type LogLevel = 'debug' | 'info' | 'warn' | 'error';

const LEVEL_ORDER: Record<LogLevel, number> = { debug: 0, info: 1, warn: 2, error: 3 };

let threshold: LogLevel = 'info';

export type LogSink = (level: LogLevel, text: string) => void;

let sink: LogSink | null = null;

/** Install the transport sink. `null` restores console-only output. */
export function setLogSink(s: LogSink | null): void {
  sink = s;
}

/** Whether a string is a recognized log level. */
export function isValidLogLevel(level: string): level is LogLevel {
  return level in LEVEL_ORDER;
}

/** Set the minimum level forwarded to the host (and console).
 * An unrecognized level is ignored — a typo'd setting must not silently
 * disable filtering and flood the host bridge. */
export function setLogLevel(level: LogLevel): void {
  if (isValidLogLevel(level)) threshold = level;
}

export function getLogLevel(): LogLevel {
  return threshold;
}

function postLog(level: LogLevel, args: unknown[]): void {
  if (LEVEL_ORDER[level] < LEVEL_ORDER[threshold]) return;
  const text = args.map((a) => (typeof a === 'string' ? a : JSON.stringify(a))).join(' ');
  sink?.(level, text);
  // Also write to the local console for in-place debugging
  const fn = level === 'error' ? console.error : console.log;
  fn('[flux]', ...args);
}

export const log = {
  debug: (...args: unknown[]) => postLog('debug', args),
  info: (...args: unknown[]) => postLog('info', args),
  warn: (...args: unknown[]) => postLog('warn', args),
  error: (...args: unknown[]) => postLog('error', args),
};
