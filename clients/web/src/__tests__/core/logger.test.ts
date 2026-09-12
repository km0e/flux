import { describe, it, expect, beforeEach, afterEach, vi, type Mock } from 'vitest';
import {
  log,
  setLogLevel,
  getLogLevel,
  isValidLogLevel,
  setLogSink,
  type LogLevel,
} from '../../logger';

describe('logger', () => {
  let sinkSpy: Mock<(level: LogLevel, text: string) => void>;

  beforeEach(() => {
    sinkSpy = vi.fn<(level: LogLevel, text: string) => void>();
    setLogSink(sinkSpy);
    setLogLevel('info'); // reset to default
  });

  afterEach(() => {
    setLogLevel('info');
    setLogSink(null);
  });

  it('defaults to info level', () => {
    expect(getLogLevel()).toBe('info');
  });

  it('filters out debug messages at info level (no sink call)', () => {
    log.debug('hidden detail');
    expect(sinkSpy).not.toHaveBeenCalled();
  });

  it('passes info messages at info level with level field in payload', () => {
    log.info('hello');
    expect(sinkSpy).toHaveBeenCalledTimes(1);
    expect(sinkSpy).toHaveBeenCalledWith('info', 'hello');
  });

  it('passes warn and error at info level', () => {
    log.warn('careful');
    log.error('broken');
    expect(sinkSpy).toHaveBeenCalledTimes(2);
    expect(sinkSpy.mock.calls[0]).toEqual(['warn', 'careful']);
    expect(sinkSpy.mock.calls[1]).toEqual(['error', 'broken']);
  });

  it('passes debug messages at debug level', () => {
    setLogLevel('debug');
    log.debug('detail');
    expect(sinkSpy).toHaveBeenCalledWith('debug', 'detail');
  });

  it('filters out info/warn/debug at error level, passes error', () => {
    setLogLevel('error');
    log.info('i');
    log.warn('w');
    log.debug('d');
    expect(sinkSpy).not.toHaveBeenCalled();
    log.error('e');
    expect(sinkSpy).toHaveBeenCalledTimes(1);
  });

  it('serializes non-string args into the text field', () => {
    log.info('count', 42);
    expect(sinkSpy).toHaveBeenCalledWith('info', 'count 42');
  });

  it('setLogLevel/getLogLevel round-trips all levels', () => {
    const levels: LogLevel[] = ['debug', 'info', 'warn', 'error'];
    for (const l of levels) {
      setLogLevel(l);
      expect(getLogLevel()).toBe(l);
    }
  });

  it('rejects an invalid level and keeps the current threshold', () => {
    setLogLevel('warn');
    setLogLevel('verbose' as never);
    expect(getLogLevel()).toBe('warn');
  });

  it('isValidLogLevel distinguishes known levels from typos', () => {
    expect(isValidLogLevel('debug')).toBe(true);
    expect(isValidLogLevel('verbose')).toBe(false);
    expect(isValidLogLevel('')).toBe(false);
  });
});
