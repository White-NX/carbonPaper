import React, { StrictMode } from 'react';
import { act, cleanup, render } from '@testing-library/react';
import { readFileSync } from 'node:fs';
import { runInThisContext } from 'node:vm';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import StartupScreenHandoff from './StartupScreenHandoff';
import { OverlayShell } from './overlay';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));

const bootstrap = readFileSync('public/startup.js', 'utf8');
const html = readFileSync('index.html', 'utf8');
const labels = { tagline: 'A screen memory', minimize: 'Minimize', hideToTray: 'Hide to tray' };
let frames;
let frameId;
let invoke;

async function paint() {
  await act(async () => {
    const pending = frames;
    frames = new Map();
    for (const callback of pending.values()) callback(0);
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  localStorage.clear();
  window.history.replaceState({}, '', '/');
  const markup = new DOMParser().parseFromString(html, 'text/html');
  document.body.replaceChildren(markup.getElementById('root'), markup.getElementById('startup-screen'));
  frames = new Map();
  frameId = 0;
  vi.stubGlobal('requestAnimationFrame', (callback) => {
    frames.set(++frameId, callback);
    return frameId;
  });
  vi.stubGlobal('cancelAnimationFrame', (id) => frames.delete(id));
  window.matchMedia = vi.fn(() => ({ matches: false }));
  invoke = vi.fn().mockResolvedValue(labels);
  window.__TAURI__ = { core: { invoke }, window: { getCurrentWindow: () => ({ minimize: vi.fn().mockResolvedValue(undefined) }) } };
  document.querySelector('[data-startup-asset]').decode = vi.fn().mockResolvedValue(undefined);
});

afterEach(() => {
  cleanup();
  delete window.__CARBONPAPER_STARTUP__;
  delete window.__TAURI__;
  delete document.documentElement.dataset.startup;
  document.documentElement.classList.remove('dark');
  document.body.replaceChildren();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe('static startup screen', () => {
  it('presents branding before React and waits for the mark to decode', async () => {
    let decode;
    document.querySelector('[data-startup-asset]').decode = () => new Promise((resolve) => { decode = resolve; });
    localStorage.setItem('theme', 'dark');
    localStorage.setItem('language', 'en');
    window.history.replaceState({}, '', '/?startup=3');
    runInThisContext(bootstrap);

    expect(document.documentElement.dataset.startup).toBe('loading');
    expect(document.documentElement).toHaveClass('dark');
    expect(document.getElementById('root')).toHaveAttribute('inert');
    expect(invoke).not.toHaveBeenCalled();
    await act(async () => { decode(); });
    await paint();
    expect(invoke).not.toHaveBeenCalled();
    await paint();
    expect(invoke).toHaveBeenCalledExactlyOnceWith('main_window_ready', { generation: 3, language: 'en' });
    expect(document.querySelector('[data-startup-tagline]')).toHaveTextContent(labels.tagline);
    expect(document.getElementById('startup-screen')).toBeInTheDocument();
  });

  it('hands focus to the startup dialog before fading, including a StrictMode remount', async () => {
    runInThisContext(bootstrap);
    const appRoot = document.getElementById('root');
    const view = (ready) => (
      <StrictMode>
        <StartupScreenHandoff ready={ready} />
        <OverlayShell open={ready} title="Unlock"><button type="button">Unlock</button></OverlayShell>
      </StrictMode>
    );
    const mounted = render(view(false), { container: appRoot });
    expect(appRoot).toHaveAttribute('inert');
    expect(document.getElementById('startup-screen')).toBeInTheDocument();

    mounted.rerender(view(true));
    expect(appRoot).not.toHaveAttribute('inert');
    expect(appRoot).not.toHaveAttribute('aria-hidden');
    expect(document.activeElement).toHaveTextContent('Unlock');
    expect(document.getElementById('startup-screen')).toHaveAttribute('inert');
    await paint();
    await paint();
    await act(async () => { await vi.advanceTimersByTimeAsync(180); });
    expect(document.getElementById('startup-screen')).not.toBeInTheDocument();
    expect(document.documentElement.dataset.startup).toBeUndefined();
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it('can reveal a hidden WebView even when animation frames are throttled', async () => {
    runInThisContext(bootstrap);
    await act(async () => { await vi.advanceTimersByTimeAsync(100); });
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(document.getElementById('startup-screen')).toBeInTheDocument();
  });

  it.each(['settings', 'snapshot-preview'])('skips branding in the %s window', async (windowType) => {
    window.history.replaceState({}, '', `/?window=${windowType}`);
    runInThisContext(bootstrap);
    await act(async () => { await vi.advanceTimersByTimeAsync(200); });
    expect(document.getElementById('startup-screen')).not.toBeInTheDocument();
    expect(document.getElementById('root')).not.toHaveAttribute('inert');
    expect(invoke).not.toHaveBeenCalled();
  });
});
