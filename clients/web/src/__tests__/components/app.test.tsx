/**
 * app.test.tsx — root shell: layout presence, Ctrl/Cmd+B sidebar toggle,
 * Escape cancels the streaming round (and retires interrupt-send
 * bookkeeping).
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, fireEvent } from '@testing-library/react';
import { App } from '../../components/App';
import { useFlux, resetFluxForTest } from '../../core/state';
import { resetBridgeForTest, setBridge } from '../../core/bridge';

describe('App', () => {
  beforeEach(() => {
    resetFluxForTest();
    resetBridgeForTest();
    document.body.innerHTML = '<div id="host"></div>';
    localStorage.clear();
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
