/**
 * toasts.test.tsx — the unified notification surface: store semantics
 * (dedupe, cap, dismiss) and the sticky-vs-auto-dismiss rendering.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { Toasts } from '../../components/Toasts';
import { useFlux, resetFluxForTest } from '../../core/state';

describe('toast store', () => {
  beforeEach(() => {
    resetFluxForTest();
  });

  it('pushes and dismisses entries', () => {
    useFlux.getState().pushToast('error', 'boom');
    expect(useFlux.getState().toasts.map((t) => t.text)).toEqual(['boom']);
    const id = useFlux.getState().toasts[0].id;
    useFlux.getState().dismissToast(id);
    expect(useFlux.getState().toasts).toEqual([]);
  });

  it('dedupes by kind+text — a repeated failure refreshes one entry', () => {
    useFlux.getState().pushToast('error', 'same');
    const first = useFlux.getState().toasts[0];
    useFlux.getState().pushToast('error', 'same');
    const toasts = useFlux.getState().toasts;
    expect(toasts.length).toBe(1);
    expect(toasts[0].id).not.toBe(first.id); // fresh entry, same slot
  });

  it('caps the stack at 4 — oldest yields', () => {
    for (let i = 0; i < 6; i++) useFlux.getState().pushToast('info', `t${i}`);
    const toasts = useFlux.getState().toasts;
    expect(toasts.length).toBe(4);
    expect(toasts[0].text).toBe('t2');
    expect(toasts[3].text).toBe('t5');
  });
});

describe('Toasts component', () => {
  beforeEach(() => {
    resetFluxForTest();
    // shouldAdvanceTime keeps RTL's waitFor working while still allowing
    // manual time jumps for the TTL assertions.
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('errors render as alerts and stay until dismissed', async () => {
    render(<Toasts />);
    useFlux.getState().pushToast('error', 'permission denied');
    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toContain('permission denied');
    vi.advanceTimersByTime(10_000);
    expect(screen.getByRole('alert')).toBeTruthy(); // sticky
    fireEvent.click(screen.getByRole('button', { name: 'Dismiss notification' }));
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('info toasts auto-dismiss after the TTL', async () => {
    render(<Toasts />);
    useFlux.getState().pushToast('info', 'refreshed');
    expect(await screen.findByRole('status')).toBeTruthy();
    vi.advanceTimersByTime(5_000);
    await waitFor(() => expect(screen.queryByRole('status')).toBeNull());
  });
});
