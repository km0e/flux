/**
 * integration-dialogs.test.tsx — the SettingsDialog's three management
 * panels over the Connect services (mocked at the service boundary: the
 * dialogs' wiring, preview semantics, and inline-error handling are the
 * subject; the RPC wire itself is pinned in grpc.test.ts / the Rust e2e).
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, fireEvent, screen, waitFor } from '@testing-library/react';
import { ProvidersPanel } from '../../components/dialogs/ProvidersPanel';
import { McpPanel } from '../../components/dialogs/McpPanel';
import { SkillsPanel } from '../../components/dialogs/SkillsPanel';
import { SettingsDialog } from '../../components/dialogs/SettingsDialog';
import { useFlux, resetFluxForTest } from '../../core/state';
import { dispatchMessage } from '../../services/dispatch';
import { registerAllHandlers } from '../../services/handlers';
import { resetDialogsForTest, setDialogImpls } from '../../services/dialogs';
import { _resetPanesForTest } from '../../services/panes';

// The whole management surface is mocked at the service boundary — the
// panels' promise semantics (inline error, success toast) are what the
// dialogs pin.
const actualProviders = vi.hoisted(() => ({ mod: null as null | typeof import('../../services/providers') }));
vi.mock('../../services/providers', async (importOriginal) => {
  const mod = await importOriginal<typeof import('../../services/providers')>();
  actualProviders.mod = mod;
  return {
    ...mod,
    fetchProviders: vi.fn(),
    probeProvider: vi.fn(async () => ({ models: [], error: undefined })),
    addProvider: vi.fn(async () => undefined),
    removeProvider: vi.fn(async () => undefined),
  };
});
vi.mock('../../services/models', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../services/models')>()),
  fetchModels: vi.fn(),
  saveModel: vi.fn(async () => ({ error: undefined })),
}));
vi.mock('../../services/mcp', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../services/mcp')>()),
  fetchMcpServers: vi.fn(),
  addMcpServer: vi.fn(async () => undefined),
  removeMcpServer: vi.fn(async () => undefined),
}));
vi.mock('../../services/skills', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../services/skills')>()),
  fetchSkills: vi.fn(),
  addSkill: vi.fn(async () => undefined),
  removeSkill: vi.fn(async () => undefined),
}));

import {
  fetchProviders,
  probeProvider,
  addProvider,
  removeProvider,
} from '../../services/providers';
import { fetchModels } from '../../services/models';
import { fetchMcpServers, addMcpServer, removeMcpServer } from '../../services/mcp';
import { fetchSkills, addSkill } from '../../services/skills';

function ctx() {
  return {
    get state() {
      return useFlux.getState();
    },
    conn: { send: vi.fn() },
    bridge: { send: vi.fn(), reconnect: vi.fn() },
  } as unknown as Parameters<typeof dispatchMessage>[1];
}

describe('integration panels', () => {
  beforeEach(() => {
    registerAllHandlers();
    resetFluxForTest();
    resetDialogsForTest();
    setDialogImpls({});
    _resetPanesForTest();
    document.body.innerHTML = '';
    vi.clearAllMocks();
    useFlux.setState({ connectionStatus: 'connected' });
  });

  it('providers: fetches + probes on open, first entry preselected, add self-selects after the broadcast', async () => {
    useFlux.setState({
      providers: [{ id: 'main', url: 'https://api.deepseek.com/v1' }],
    });
    // The probe publishes through the REAL reply handler (the pickers
    // read the cache, never ad-hoc results) — the mock mirrors that.
    vi.mocked(probeProvider).mockImplementation(async (pid) => {
      const r = { models: [{ id: 'm1', context_length: 4096 }, { id: 'm2' }], error: undefined };
      actualProviders.mod?.handleProviderModelsReply(pid, r.models, r.error);
      return r;
    });
    render(<ProvidersPanel />);
    // Mount → fetchProviders + fetchModels (the panel owns both registries'
    // fetches; the pickers read the caches only), plus the automatic
    // catalog probe of the selected entry.
    await waitFor(() => expect(fetchProviders).toHaveBeenCalled());
    expect(fetchModels).toHaveBeenCalled();
    await waitFor(() => expect(probeProvider).toHaveBeenCalledWith('main'));
    // The rail lists the entry; the first one is preselected (preview-first).
    expect(screen.getByRole('button', { name: 'Select provider main' })).toBeTruthy();
    expect(screen.getAllByText('main').length).toBeGreaterThan(0);
    expect(screen.getAllByText('https://api.deepseek.com/v1').length).toBeGreaterThan(0);
    // The probe resolves → the catalog lands in the store via the SAME
    // reply handler the broadcast path uses, and the preview renders it
    // DIRECTLY (no expand step on desktop).
    await waitFor(() => {
      expect(screen.getByText('2 models')).toBeTruthy();
    });
    const list = screen.getByRole('list', { name: 'Models of main' });
    expect(list.textContent).toContain('m1');
    expect(list.textContent).toContain('m2');
    // The api key is never in the document.
    expect(document.body.textContent).not.toContain('sk-');
    // Switching to the New row carries the creation form, gated on the id.
    fireEvent.click(screen.getByRole('button', { name: 'New provider' }));
    const addBtn = screen.getByRole('button', { name: /Add provider/ });
    expect((addBtn as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(screen.getByPlaceholderText('main'), { target: { value: 'second' } });
    fireEvent.change(screen.getByPlaceholderText('https://api.openai.com/v1'), {
      target: { value: 'https://x/v1' },
    });
    expect((addBtn as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(addBtn);
    await waitFor(() => expect(addProvider).toHaveBeenCalled());
    // The RPC resolves → success toast + form reset; the fresh list lands
    // via the broadcast and the NEW entry selects itself.
    dispatchMessage(
      { type: 'providers', providers: [{ id: 'main', url: 'https://api.deepseek.com/v1' }, { id: 'second', url: 'https://x/v1' }] },
      ctx(),
    );
    await waitFor(() => {
      expect(useFlux.getState().toasts.some((t) => t.text.includes('"second" added'))).toBe(true);
    });
    await waitFor(() => {
      // the detail now previews 'second' (title + its url), not the form
      expect(screen.getByText(/never shown back/)).toBeTruthy();
    });
    expect(screen.getAllByText('https://x/v1').length).toBeGreaterThan(0);
    // A rejected add surfaces the inline error on the form.
    vi.mocked(addProvider).mockResolvedValueOnce('duplicate provider id: main');
    fireEvent.click(screen.getByRole('button', { name: 'New provider' }));
    fireEvent.change(screen.getByPlaceholderText('main'), { target: { value: 'main' } });
    fireEvent.click(screen.getByRole('button', { name: /Add provider/ }));
    await waitFor(() => {
      expect(screen.getByText('duplicate provider id: main')).toBeTruthy();
    });
  });

  it('providers: the upstream catalog block filters by id (import discovery)', async () => {
    useFlux.setState({ providers: [{ id: 'main', url: 'https://x/v1' }] });
    vi.mocked(probeProvider).mockImplementation(async (pid) => {
      const r = { models: [{ id: 'alpha' }, { id: 'beta' }, { id: 'gamma' }], error: undefined };
      actualProviders.mod?.handleProviderModelsReply(pid, r.models, r.error);
      return r;
    });
    render(<ProvidersPanel />);
    await waitFor(() => expect(screen.getByText('3 models')).toBeTruthy());
    // The filter narrows the import candidates live; the count shows the
    // shown/total split.
    fireEvent.change(screen.getByLabelText('Filter models of main'), {
      target: { value: 'gam' },
    });
    const list = screen.getByRole('list', { name: 'Models of main' });
    expect(list.textContent).toContain('gamma');
    expect(list.textContent).not.toContain('alpha');
    // The shown/total count rides the filter row above the list.
    expect(screen.getByText('1 / 3')).toBeTruthy();
  });

  it('providers: deleting the selected entry falls selection to the next one', async () => {
    useFlux.setState({
      providers: [
        { id: 'a', url: 'https://a/v1' },
        { id: 'b', url: 'https://b/v1' },
        { id: 'c', url: 'https://c/v1' },
      ],
      providerModels: { a: [], b: [], c: [] },
    });
    render(<ProvidersPanel />);
    // First entry preselected → remove it via the preview's two-step.
    fireEvent.click(screen.getByLabelText('Remove a'));
    fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
    await waitFor(() => expect(removeProvider).toHaveBeenCalledWith('a'));
    // The broadcast lands → selection falls to the NEXT entry (b).
    dispatchMessage(
      { type: 'providers', providers: [{ id: 'b', url: 'https://b/v1' }, { id: 'c', url: 'https://c/v1' }] },
      ctx(),
    );
    await waitFor(() => {
      expect(screen.getByText(/never shown back/)).toBeTruthy();
    });
    expect(screen.getAllByText('https://b/v1').length).toBeGreaterThan(0);
    // Remove b → falls to c; remove c → the list is empty and the
    // creation form takes over.
    fireEvent.click(screen.getByLabelText('Remove b'));
    fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
    dispatchMessage({ type: 'providers', providers: [{ id: 'c', url: 'https://c/v1' }] }, ctx());
    await waitFor(() => {
      expect(screen.getAllByText('https://c/v1').length).toBeGreaterThan(0);
    });
    fireEvent.click(screen.getByLabelText('Remove c'));
    fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
    dispatchMessage({ type: 'providers', providers: [] }, ctx());
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Add provider/ })).toBeTruthy();
    });
  });

  it('mcp: preselected preview with live-apply semantics, two-step delete, env line validation', async () => {
    useFlux.setState({
      mcpServers: [
        {
          id: 'fs',
          kind: 'stdio',
          command: 'npx',
          args: ['-y', '@mcp/fs'],
          env_keys: ['TOKEN'],
          url: '',
          header_keys: [],
          state: 'running',
          tool_names: ['fs_read', 'fs_write'],
        },
      ],
    });
    render(<McpPanel />);
    expect(fetchMcpServers).toHaveBeenCalled();
    // First entry preselected → preview shows id, launch line, env KEY
    // badge (never a value), and the live-apply footnote.
    expect(screen.getAllByText('fs').length).toBeGreaterThan(0);
    expect(screen.getAllByText('npx -y @mcp/fs').length).toBeGreaterThan(0);
    expect(screen.getByText('TOKEN')).toBeTruthy();
    // The live session's registered tools render as chips (the summary's
    // tool_names — the debugging surface that replaces the server log).
    expect(screen.getByText('fs_read')).toBeTruthy();
    expect(screen.getByText('fs_write')).toBeTruthy();
    expect(screen.getByText(/Applies live/)).toBeTruthy();
    // Delete is two-step, in the preview pane.
    fireEvent.click(screen.getByLabelText('Remove fs'));
    fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
    await waitFor(() => expect(removeMcpServer).toHaveBeenCalledWith('fs'));
    // The New row carries the full explanation + notice + form; env
    // validation blocks the add locally — nothing is sent.
    fireEvent.click(screen.getByRole('button', { name: 'New server' }));
    expect(screen.getByText(/apply live/i)).toBeTruthy();
    fireEvent.change(screen.getByPlaceholderText('filesystem'), { target: { value: 'x' } });
    fireEvent.change(screen.getByPlaceholderText('npx'), { target: { value: 'npx' } });
    fireEvent.change(screen.getByLabelText('Environment'), {
      target: { value: 'NOT_AN_ASSIGNMENT' },
    });
    fireEvent.click(screen.getByRole('button', { name: /Add server/ }));
    expect(screen.getByText(/expected KEY=VALUE/)).toBeTruthy();
    expect(addMcpServer).not.toHaveBeenCalled();
  });

  it('skills: fetches with the active chat, first entry preselected (read-only for project), install form semantics', async () => {
    useFlux.setState({
      activeChatId: 'c1',
      skills: [
        { name: 'pdf', description: 'PDF toolkit', source: 'global', removable: true },
        { name: 'repo', description: 'Repo skill', source: 'project', removable: false },
      ],
    });
    render(<SkillsPanel />);
    // Fetch on show carries the active chat (project skills, read-only).
    await waitFor(() => expect(fetchSkills).toHaveBeenCalledWith('c1'));
    expect(screen.getAllByText('pdf').length).toBeGreaterThan(0);
    expect(screen.getAllByText('Repo skill').length).toBeGreaterThan(0);
    // First entry (global) preselected → removable in the preview.
    expect(screen.getByLabelText('Remove pdf')).toBeTruthy();
    // The project skill is read-only — and not even selected here.
    expect(screen.queryByLabelText('Remove repo')).toBeNull();
    // Switching to repo → read-only note, no remove control.
    fireEvent.click(screen.getByRole('button', { name: 'Select skill repo' }));
    expect(screen.getByText(/Read-only/)).toBeTruthy();
    expect(screen.queryByLabelText('Remove repo')).toBeNull();
    // The New row carries the install form, gated on the source; local vs
    // git switches the explanation line and enables the subpath field.
    fireEvent.click(screen.getByRole('button', { name: 'Install skill' }));
    const install = screen.getByRole('button', { name: 'Install' });
    expect((install as HTMLButtonElement).disabled).toBe(true);
    const subpath = screen.getByLabelText(/Subpath/);
    expect((subpath as HTMLInputElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText(/Source/), {
      target: { value: 'https://github.com/u/skills' },
    });
    expect(screen.getByText(/shallow-cloned/)).toBeTruthy();
    expect((screen.getByPlaceholderText('skills/pdf-tools') as HTMLInputElement).disabled).toBe(false);
    fireEvent.click(install);
    await waitFor(() =>
      expect(addSkill).toHaveBeenCalledWith(
        expect.objectContaining({ url: 'https://github.com/u/skills' }),
      ),
    );
    // The RPC resolves → success toast + form reset.
    await waitFor(() => {
      expect(useFlux.getState().toasts.some((t) => t.text.includes('Skill installed'))).toBe(true);
    });
    expect((screen.getByRole('button', { name: 'Install' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('settings dialog: opens on the requested section, other tabs fetch only when visited', async () => {
    const onClose = vi.fn();
    render(<SettingsDialog initialTab="skills" onClose={onClose} />);
    // One tab strip, three sections.
    expect(screen.getByRole('tab', { name: 'Providers' })).toBeTruthy();
    expect(screen.getByRole('tab', { name: 'MCP servers' })).toBeTruthy();
    expect(screen.getByRole('tab', { name: 'Skills' })).toBeTruthy();
    // initialTab=skills → the skills panel fetches; providers/mcp never do
    // (Radix unmounts inactive content — fetch-on-show is the semantics).
    expect(fetchSkills).toHaveBeenCalled();
    expect(fetchProviders).not.toHaveBeenCalled();
    expect(fetchMcpServers).not.toHaveBeenCalled();
    expect(screen.getByRole('tab', { name: 'Skills', selected: true })).toBeTruthy();
    // Switching to Providers fetches its registry and shows its rail.
    // Radix Tabs activates on mouse-down (left button, no ctrl).
    fireEvent.mouseDown(screen.getByRole('tab', { name: 'Providers' }), { button: 0 });
    await waitFor(() => {
      expect(fetchProviders).toHaveBeenCalled();
    });
    expect(screen.getByText('No providers yet.')).toBeTruthy();
    // Closing reports to the parent (which unmounts — as TopBar does).
    fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
