/**
 * provider-picker.test.tsx — the picker lists ONLY real registry entries
 * (no synthetic "server default" option — `chat_create` requires an
 * explicit pin) and the model probe follows the selection.
 */
import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { useState } from 'react';
import { ProviderPicker } from '../../components/ProviderPicker';
import { useFlux, resetFluxForTest } from '../../core/state';
import { resetBridgeForTest } from '../../core/bridge';

/** Controlled harness mirroring the dialogs' wiring (state flows back). */
function Harness() {
  const [sel, setSel] = useState({ provider: '', model: '' });
  return (
    <>
      <div data-testid="picked">{sel.provider || '(none)'}</div>
      <div data-testid="model">{sel.model}</div>
      <ProviderPicker providerId={sel.provider} model={sel.model} onChange={setSel} />
    </>
  );
}

describe('ProviderPicker', () => {
  beforeEach(() => {
    resetFluxForTest();
    resetBridgeForTest();
    document.body.innerHTML = '';
    useFlux.setState({
      // Registry entries are pure endpoints — id + effective url, no model.
      providers: [{ id: 'alpha', url: 'https://a/v1' }, { id: 'beta', url: 'https://b/v1' }],
      // The probe lands entries with (optionally) probed context lengths.
      providerModels: {
        alpha: [
          { id: 'm-a', context_length: 131072 },
          { id: 'm-alt' },
        ],
      },
    });
  });

  it('renders only real registry entries — no server-default option', () => {
    render(<Harness />);
    expect(screen.getByText('alpha')).toBeTruthy();
    expect(screen.getByText('beta')).toBeTruthy();
    expect(screen.queryByText(/server default/i)).toBeNull();
    // Nothing picked yet: the disabled prompt holds the select, and the
    // harness still holds the empty pin (Create would stay gated).
    expect(screen.getByTestId('picked').textContent).toBe('(none)');
    const select = screen.getByLabelText(/Provider/) as HTMLSelectElement;
    expect(select.value).toBe('');
    expect(select.options[0].disabled).toBe(true);
  });

  it('a selection flows back through onChange; picking resets the model', () => {
    render(<Harness />);
    fireEvent.change(screen.getByLabelText(/Provider/), { target: { value: 'beta' } });
    expect(screen.getByTestId('picked').textContent).toBe('beta');
    expect(screen.getByTestId('model').textContent).toBe('');
  });

  it('the model input is required and carries no provider-default placeholder', () => {
    render(<Harness />);
    fireEvent.change(screen.getByLabelText(/Provider/), { target: { value: 'alpha' } });
    const input = screen.getByLabelText(/Model/) as HTMLInputElement;
    expect(input.placeholder).toBe('model id');
    expect(screen.getByText(/required/)).toBeTruthy();
  });

  it('the hint shows a probed context length and stays silent without one', () => {
    render(<Harness />);
    fireEvent.change(screen.getByLabelText(/Provider/), { target: { value: 'alpha' } });
    // Empty model: nothing to match — no hint.
    expect(screen.queryByText(/ctx/)).toBeNull();
    // Typing a catalog match shows that entry's probed ctx…
    fireEvent.change(screen.getByLabelText(/Model/), { target: { value: 'm-a' } });
    expect(screen.getByText(/131k ctx/)).toBeTruthy();
    // …an unprobed model shows nothing (never a guessed number).
    fireEvent.change(screen.getByLabelText(/Model/), { target: { value: 'm-unknown' } });
    expect(screen.queryByText(/ctx/)).toBeNull();
  });

  it('lists saved rows in the datalist (star-marked)', () => {
    // The saved-model list is session-level (preloaded at attach) — the
    // picker only reads the store; no fetch belongs here.
    useFlux.setState({
      savedModels: [
        { provider: 'alpha', model: 'm-a', params: { context_length: 131072 }, meta: {} },
      ],
    });
    render(<Harness />);
    // The saved row feeds the datalist (star-marked) once its provider is picked.
    fireEvent.change(screen.getByLabelText(/Provider/), { target: { value: 'alpha' } });
    expect(screen.getByText('★ m-a')).toBeTruthy();
  });

  it('the datalist offers ONLY saved models — the probed catalog never bloats it', () => {
    useFlux.setState({
      providerModels: { alpha: [{ id: 'probe-1' }, { id: 'probe-2' }] },
      savedModels: [{ provider: 'alpha', model: 'm-a', params: {}, meta: {} }],
    });
    render(<Harness />);
    fireEvent.change(screen.getByLabelText(/Provider/), { target: { value: 'alpha' } });
    // Unsaved probed entries stay OUT of the datalist; only the saved row
    // is offered. Discovery lives in Providers, not here.
    const values = [...document.querySelectorAll('datalist option')].map((o) =>
      o.getAttribute('value'),
    );
    expect(values).toEqual(['m-a']);
  });

  it('no saved rows → empty datalist + import pointer; free text still pins', () => {
    useFlux.setState({ providerModels: { alpha: [{ id: 'probe-1' }] } });
    render(<Harness />);
    fireEvent.change(screen.getByLabelText(/Provider/), { target: { value: 'alpha' } });
    expect(screen.getByText(/import in Providers/)).toBeTruthy();
    expect(document.querySelectorAll('datalist option').length).toBe(0);
    // The registry is a convenience, never a gate — an unsaved model
    // string still pins (upstream defaults apply).
    fireEvent.change(screen.getByLabelText(/Model/), { target: { value: 'any-unsaved' } });
    expect(screen.getByTestId('model').textContent).toBe('any-unsaved');
  });
});
