import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import { describe, expect, it, vi } from 'vitest';

function worker() {
  const events = {};
  const event = (name) => ({ addListener: (callback) => { events[name] = callback; } });
  const postMessage = vi.fn();
  const captureVisibleTab = vi.fn();
  const state = { window: { id: 1, focused: true, incognito: false }, tab: { id: 3, active: true, url: 'https://example.com/', title: 'Example' } };
  const context = {
    console: { log: vi.fn(), warn: vi.fn(), error: vi.fn() }, navigator: { userAgent: 'Chrome' },
    setTimeout: vi.fn(), clearTimeout: vi.fn(), setInterval: vi.fn(), clearInterval: vi.fn(),
    importScripts: vi.fn(), CarbonPaperImageTransport: { fitPngPayload: vi.fn() },
    chrome: {
      runtime: { connectNative: () => ({ postMessage, disconnect: vi.fn(), onMessage: event('native'), onDisconnect: event('disconnect') }), onStartup: event('startup'), onInstalled: event('installed'), onMessage: event('popup') },
      storage: { local: { get: (_keys, callback) => callback({ enabled: true }), set: vi.fn() } },
      windows: { getLastFocused: async () => state.window, WINDOW_ID_NONE: -1, onFocusChanged: event('focus') },
      tabs: { query: async () => [state.tab], captureVisibleTab, onActivated: event('activation'), onUpdated: event('update') },
    },
  };
  vm.createContext(context);
  vm.runInContext(fs.readFileSync(path.resolve('browser-extension/background.js'), 'utf8'), context);
  events.native({ type: 'nmh_ready', usage_metadata: true });
  return { context, events, state, postMessage, captureVisibleTab };
}

describe('lightweight browser usage metadata', () => {
  it('reports focused tab metadata without taking a screenshot and clears it for private or unfocused windows', async () => {
    const { context, state, postMessage, captureVisibleTab } = worker();
    await context.reportUsageMetadata();
    expect(postMessage).toHaveBeenLastCalledWith({ type: 'usage_metadata', focused: true, private: false, title: 'Example', url: 'https://example.com/' });
    expect(captureVisibleTab).not.toHaveBeenCalled();
    state.window.incognito = true;
    await context.reportUsageMetadata();
    expect(postMessage).toHaveBeenLastCalledWith({ type: 'usage_metadata', focused: true, private: true });
    state.window.focused = false;
    await context.reportUsageMetadata();
    expect(postMessage).toHaveBeenLastCalledWith({ type: 'usage_metadata', focused: false });
  });

  it('does not retain a URL when the active tab changes to an internal page', async () => {
    const { context, state, postMessage } = worker();
    await context.reportUsageMetadata();
    state.tab.url = 'chrome://settings';
    await context.reportUsageMetadata();
    expect(postMessage).toHaveBeenLastCalledWith({ type: 'usage_metadata', focused: false });
  });
});
