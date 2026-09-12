/**
 * failsafe.test.ts — the window-level capture of unexpected failures.
 *
 * Pins: unhandled rejections and uncaught errors are logged AND surfaced
 * as exactly ONE deduplicated toast (throttled), while ordinary resource
 * errors (no error object) stay silent.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { installFailsafe, _resetFailsafeForTest } from '../../core/failsafe';
import { useFlux, resetFluxForTest } from '../../core/state';
import { log } from '../../logger';

describe('core/failsafe', () => {
  beforeEach(() => {
    _resetFailsafeForTest();
    resetFluxForTest();
    vi.spyOn(log, 'error').mockImplementation(() => {});
    vi.mocked(log.error).mockClear(); // per-test call counts (jsdom window is shared)
  });

  const reject = (reason: unknown) =>
    window.dispatchEvent(new PromiseRejectionEvent('unhandledrejection', {
      promise: Promise.resolve(),
      reason,
    } as PromiseRejectionEventInit));

  it('an unhandled rejection logs and raises one error toast', () => {
    installFailsafe();
    reject(new Error('boom'));
    const toasts = useFlux.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0].kind).toBe('error');
    expect(toasts[0].text).toContain('unexpected error');
    expect(log.error).toHaveBeenCalledWith(expect.stringContaining('boom'));
  });

  it('repeated failures stay ONE toast (throttled), even after dismissal', () => {
    installFailsafe();
    reject(new Error('a'));
    useFlux.getState().dismissToast(useFlux.getState().toasts[0].id);
    reject(new Error('b'));
    expect(useFlux.getState().toasts).toHaveLength(0);
    expect(log.error).toHaveBeenCalledTimes(2);
  });

  it('resource-loading errors (no error object) stay silent', () => {
    installFailsafe();
    const ev = new Event('error');
    window.dispatchEvent(ev);
    expect(useFlux.getState().toasts).toHaveLength(0);
    expect(log.error).not.toHaveBeenCalled();
  });
});
