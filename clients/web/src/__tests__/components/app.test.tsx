/**
 * app.test.tsx — root shell: layout presence, Ctrl/Cmd+B sidebar toggle,
 * Escape cancels the streaming round (and retires interrupt-send
 * bookkeeping).
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, fireEvent, createEvent } from '@testing-library/react';
import { App } from '../../components/App';
import { useFlux, resetFluxForTest } from '../../core/state';
import { resetBridgeForTest, setBridge } from '../../core/bridge';

// matchMedia stub — the drawer tests need the MOBILE regime; everything
// else gets desktop. The flag is read live (useIsMobile's initializer and
// effect, isMobileViewport's escape probe).
let mobileMatches = false;
const matchMediaStub = vi.fn().mockImplementation((q: string) => ({
  matches: mobileMatches && q === '(max-width: 767.5px)',
  media: q,
  onchange: null,
  addEventListener: vi.fn(),
  removeEventListener: vi.fn(),
  addListener: vi.fn(),
  removeListener: vi.fn(),
  dispatchEvent: vi.fn(),
}));

describe('App', () => {
  beforeEach(() => {
    vi.stubGlobal('matchMedia', matchMediaStub);
    resetFluxForTest();
    resetBridgeForTest();
    document.body.innerHTML = '<div id="host"></div>';
    localStorage.clear();
  });

  afterEach(() => {
    mobileMatches = false;
    vi.unstubAllGlobals();
  });

  it('the mobile drawer inerts the covered content; the top bar stays reachable', () => {
    mobileMatches = true;
    useFlux.setState({ dockOpen: true });
    render(<App />);
    // Covered surfaces are out of the tab order / a11y tree / pointer.
    expect(document.getElementById('main')?.hasAttribute('inert')).toBe(true);
    expect(document.getElementById('right-dock')?.hasAttribute('inert')).toBe(true);
    // The toggle that closes the drawer must stay reachable.
    expect(document.getElementById('top-bar')?.hasAttribute('inert')).toBe(false);
    // Closing the drawer restores everything.
    fireEvent.click(document.getElementById('sidebar-toggle')!);
    expect(useFlux.getState().sidebarOpen).toBe(false);
    expect(document.getElementById('main')?.hasAttribute('inert')).toBe(false);
    expect(document.getElementById('right-dock')?.hasAttribute('inert')).toBe(false);
  });

  it('desktop never inerts the content (side-by-side layout, no overlay)', () => {
    mobileMatches = false;
    useFlux.setState({ dockOpen: true });
    render(<App />);
    expect(document.getElementById('main')?.hasAttribute('inert')).toBe(false);
    expect(document.getElementById('right-dock')?.hasAttribute('inert')).toBe(false);
  });

  it('Escape closes the mobile drawer first and does NOT cancel the round', () => {
    mobileMatches = true;
    const send = vi.fn();
    setBridge({ send });
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'R', createdAt: 1, active: false, workdir: '', provider: '', model: '' },
      ],
      activeChatId: 'c1',
      streaming: { c1: true },
    });
    render(<App />);
    fireEvent.keyDown(window, { key: 'Escape' });
    // The drawer is the topmost surface — it closes; the round survives.
    expect(useFlux.getState().sidebarOpen).toBe(false);
    expect(send).not.toHaveBeenCalled();
    // Drawer closed — Escape cancels as usual.
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(send).toHaveBeenCalledWith({ type: 'cancel', chat_id: 'c1' });
  });

  it('renders the shell: top bar, sidebar layer, messages wrap comes from panes', () => {
    const { container } = render(<App />);
    expect(container.querySelector('#top-bar')).toBeTruthy();
    expect(container.querySelector('#sidebar-layer')).toBeTruthy();
    expect(container.querySelector('#sidebar')).toBeTruthy();
  });

  it('Ctrl+B toggles the sidebar', () => {
    render(<App />);
    expect(useFlux.getState().sidebarOpen).toBe(true);
    fireEvent.keyDown(window, { key: 'b', ctrlKey: true });
    expect(useFlux.getState().sidebarOpen).toBe(false);
    fireEvent.keyDown(window, { key: 'b', metaKey: true });
    expect(useFlux.getState().sidebarOpen).toBe(true);
  });

  it('Escape cancels the streaming round', () => {
    const send = vi.fn();
    setBridge({ send });
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'R', createdAt: 1, active: false, workdir: '', provider: '', model: '' },
      ],
      activeChatId: 'c1',
      streaming: { c1: true },
    });
    render(<App />);
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(send).toHaveBeenCalledWith({ type: 'cancel', chat_id: 'c1' });
  });

  it('Escape while NOT streaming sends nothing', () => {
    const send = vi.fn();
    setBridge({ send });
    render(<App />);
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(send).not.toHaveBeenCalled();
  });

  it('Escape does NOT cancel while a Radix dialog/menu is open (B2)', () => {
    const send = vi.fn();
    setBridge({ send });
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'R', createdAt: 1, active: false, workdir: '', provider: '', model: '' },
      ],
      activeChatId: 'c1',
      streaming: { c1: true },
    });
    render(<App />);

    // An open dialog owns Escape (its layer dismisses) — the window-level
    // cancel must not ALSO fire, or closing Settings kills the round.
    const dialog = document.createElement('div');
    dialog.setAttribute('role', 'dialog');
    dialog.setAttribute('data-state', 'open');
    document.body.appendChild(dialog);
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(send).not.toHaveBeenCalled();

    // Same for an open dropdown menu (role=menu; Radix unmounts when
    // closed, so presence = open).
    dialog.remove();
    const menu = document.createElement('div');
    menu.setAttribute('role', 'menu');
    document.body.appendChild(menu);
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(send).not.toHaveBeenCalled();

    // Once the surface is gone, Escape cancels again.
    menu.remove();
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(send).toHaveBeenCalledWith({ type: 'cancel', chat_id: 'c1' });
  });

  it('Escape ignores an already-consumed (defaultPrevented) event (B2)', () => {
    const send = vi.fn();
    setBridge({ send });
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'R', createdAt: 1, active: false, workdir: '', provider: '', model: '' },
      ],
      activeChatId: 'c1',
      streaming: { c1: true },
    });
    render(<App />);
    const ev = createEvent.keyDown(window, { key: 'Escape' });
    Object.defineProperty(ev, 'defaultPrevented', { value: true });
    fireEvent(window, ev);
    expect(send).not.toHaveBeenCalled();
  });

  it('the sidebar resizer resizes by keyboard, clamped and persisted (B3)', () => {
    render(<App />);
    const resizer = document.getElementById('sidebar-resizer')!;
    expect(resizer.getAttribute('tabindex')).toBe('0');
    expect(resizer.getAttribute('aria-valuenow')).toBe('240');

    // The handle is the sidebar's RIGHT border: ArrowRight widens.
    fireEvent.keyDown(resizer, { key: 'ArrowRight' });
    expect(useFlux.getState().sidebarWidth).toBe(264);
    expect(document.documentElement.style.getPropertyValue('--fx-sidebar-w')).toBe('264px');
    expect(localStorage.getItem('flux.sidebar.width')).toBe('264');

    fireEvent.keyDown(resizer, { key: 'ArrowLeft' });
    expect(useFlux.getState().sidebarWidth).toBe(240);

    // Clamped: at the ceiling ArrowRight is a no-op; at the floor ArrowLeft too.
    useFlux.setState({ sidebarWidth: 360 });
    fireEvent.keyDown(resizer, { key: 'ArrowRight' });
    expect(useFlux.getState().sidebarWidth).toBe(360);
    useFlux.setState({ sidebarWidth: 160 });
    fireEvent.keyDown(resizer, { key: 'ArrowLeft' });
    expect(useFlux.getState().sidebarWidth).toBe(160);

    // Other keys pass through.
    fireEvent.keyDown(resizer, { key: 'ArrowUp' });
    expect(useFlux.getState().sidebarWidth).toBe(160);
  });

  it('a cancelled pointer gesture commits and detaches (B5)', () => {
    render(<App />);
    const resizer = document.getElementById('sidebar-resizer')!;

    fireEvent.pointerDown(resizer);
    fireEvent.pointerMove(window, { clientX: 300 });
    fireEvent.pointerCancel(window);
    // Commit-on-cancel: the var already showed 300 — the store must agree.
    expect(useFlux.getState().sidebarWidth).toBe(300);
    expect(localStorage.getItem('flux.sidebar.width')).toBe('300');
    expect(document.body.classList.contains('resizing-sidebar')).toBe(false);
    // The dead gesture is detached — later moves change nothing.
    fireEvent.pointerMove(window, { clientX: 350 });
    expect(document.documentElement.style.getPropertyValue('--fx-sidebar-w')).toBe('300px');
  });

  it('the sidebar resizer drags to a new width (clamped) and persists on release', () => {
    render(<App />);
    const resizer = document.getElementById('sidebar-resizer');
    expect(resizer).toBeTruthy();

    fireEvent.pointerDown(resizer!);
    expect(document.body.classList.contains('resizing-sidebar')).toBe(true);

    // During the drag the width rides the CSS var (zero React renders per
    // pointermove) — clamped both ways.
    fireEvent.pointerMove(window, { clientX: 400 });
    expect(document.documentElement.style.getPropertyValue('--fx-sidebar-w')).toBe('360px');
    fireEvent.pointerMove(window, { clientX: 100 });
    expect(document.documentElement.style.getPropertyValue('--fx-sidebar-w')).toBe('160px');
    // A legal position, then release — committed to the store, persisted,
    // drag state cleared. (pointerup carries the final coordinates — same
    // as real pointers.)
    fireEvent.pointerMove(window, { clientX: 250 });
    fireEvent.pointerUp(window, { clientX: 250 });
    expect(useFlux.getState().sidebarWidth).toBe(250);
    expect(document.documentElement.style.getPropertyValue('--fx-sidebar-w')).toBe('250px');
    expect(localStorage.getItem('flux.sidebar.width')).toBe('250');
    expect(document.body.classList.contains('resizing-sidebar')).toBe(false);
  });

  it('the sidebar width is authoritative via --fx-sidebar-w (content cannot reflow it)', () => {
    useFlux.setState({ sidebarWidth: 300 });
    render(<App />);
    // The App publishes the width as a CSS var — #sidebar consumes it.
    expect(document.documentElement.style.getPropertyValue('--fx-sidebar-w')).toBe('300px');
  });

  it('the dock width rides --fx-preview-w the same way (mirrors the sidebar pattern)', () => {
    useFlux.setState({ previewWidth: 640, dockOpen: true });
    render(<App />);
    expect(document.documentElement.style.getPropertyValue('--fx-preview-w')).toBe('640px');
    // No inline width — the CSS var is the only authoring surface (the
    // drag writes it directly; a React render per pointermove is the jank
    // this pattern exists to kill).
    const dock = document.getElementById('right-dock') as HTMLElement;
    expect(dock.style.width).toBe('');
  });

  it('the dock resizer drags via the CSS var and persists on release', () => {
    useFlux.setState({ dockOpen: true, previewWidth: 480 });
    render(<App />);
    const resizer = document.getElementById('dock-resizer');
    expect(resizer).toBeTruthy();

    fireEvent.pointerDown(resizer!);
    expect(document.body.classList.contains('resizing-preview')).toBe(true);

    // During the drag the var moves directly (zero React renders per
    // pointermove); the store stays untouched until release.
    fireEvent.pointerMove(window, { clientX: 200 });
    // Clamped to both ceilings: innerWidth-280 (=1000) and PREVIEW_MAX (1080).
    expect(document.documentElement.style.getPropertyValue('--fx-preview-w')).toBe(
      `${1280 - 280}px`,
    );
    expect(useFlux.getState().previewWidth).toBe(480); // not yet committed
    // Release commits the store + persists + clears the drag state.
    fireEvent.pointerMove(window, { clientX: 600 });
    fireEvent.pointerUp(window, { clientX: 600 });
    expect(useFlux.getState().previewWidth).toBe(1280 - 600);
    expect(localStorage.getItem('flux.preview.width')).toBe(String(1280 - 600));
    expect(document.body.classList.contains('resizing-preview')).toBe(false);
  });

  it('the file preview dock renders when a preview is open', () => {
    useFlux.setState({
      dockOpen: true,
      openFiles: [{ id: '/x/a.ts', path: '/x/a.ts', name: 'a.ts', content: 'let x = 1', truncated: false, loading: false, rawView: false }],
      activeDockTab: '/x/a.ts',
    });
    const { container } = render(<App />);
    expect(container.querySelector('#right-dock')).toBeTruthy();
    expect(container.textContent).toContain('let x = 1');
  });
});
