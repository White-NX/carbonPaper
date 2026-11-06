import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const contentSource = readFileSync('browser-extension/content.js', 'utf8');
const backgroundSource = readFileSync('browser-extension/background.js', 'utf8');

describe('visible browser video activity', () => {
  let request;
  let video;
  let clock;
  let position;
  let paused;
  let ended;
  let ready;

  beforeEach(() => {
    document.body.innerHTML = '<video></video>';
    document.title = 'Film';
    vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('visible');
    video = document.querySelector('video');
    video.getBoundingClientRect = () => ({ top: 0, left: 0, right: 200, bottom: 200, width: 200, height: 200 });
    clock = 0;
    position = 0;
    paused = false;
    ended = false;
    ready = 4;
    Object.defineProperties(video, {
      currentTime: { get: () => position }, paused: { get: () => paused },
      ended: { get: () => ended }, readyState: { get: () => ready },
    });
    let listener;
    vm.runInNewContext(contentSource, {
      chrome: { runtime: { onMessage: { addListener: (callback) => { listener = callback; } } } },
      document, window, getComputedStyle: window.getComputedStyle,
      performance: { now: () => clock }, URL,
    });
    request = () => {
      let response;
      listener({ type: 'getMediaState' }, {}, (value) => { response = value; });
      return response;
    };
  });

  afterEach(() => { document.body.replaceChildren(); vi.restoreAllMocks(); });

  const advance = (seconds = 5) => { clock += seconds * 1000; position += seconds; };

  it('counts an advancing visible film even when muted and without input events', () => {
    video.muted = true;
    expect(request().playing).toBe(false);
    advance();
    expect(request().playing).toBe(true);
    expect(request().pageTitle).toBe('Film');
  });

  it('stops playback activity for paused, stalled, ended and buffering video', () => {
    request(); advance(); expect(request().playing).toBe(true);
    clock += 5000;
    expect(request().playing).toBe(false);
    paused = true; advance(); expect(request().playing).toBe(false);
    paused = false; ended = true; advance(); expect(request().playing).toBe(false);
    ended = false; ready = 2; advance(); expect(request().playing).toBe(false);
  });

  it('excludes background tabs and hidden or offscreen players', () => {
    request(); advance(); expect(request().playing).toBe(true);
    vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden');
    advance(); expect(request().playing).toBe(false);
    vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('visible');
    video.style.opacity = '0'; advance(); expect(request().playing).toBe(false);
    video.style.opacity = '1';
    video.getBoundingClientRect = () => ({ top: -200, bottom: -1, left: 0, right: 200 });
    advance(); expect(request().playing).toBe(false);
  });

  it('requires fresh progress after a long gap in media observations', () => {
    request(); advance(); expect(request().playing).toBe(true);
    advance(30); expect(request().playing).toBe(false);
    advance(); expect(request().playing).toBe(true);
  });
});

describe('browser media report ownership', () => {
  let context;
  let chrome;
  let port;
  const event = () => ({ addListener: vi.fn() });

  beforeEach(() => {
    port = { postMessage: vi.fn(), disconnect: vi.fn(), onMessage: event(), onDisconnect: event() };
    chrome = {
      tabs: { onActivated: event(), onUpdated: event(), query: vi.fn(async () => [{ id: 7, title: 'Film', incognito: false }]),
        sendMessage: vi.fn(async () => ({ playing: true, hasVideo: true, pageTitle: 'Film' })) },
      windows: { onFocusChanged: event(), getLastFocused: vi.fn(async () => ({ id: 1, focused: true })) },
      runtime: { onStartup: event(), onInstalled: event(), onMessage: event(), connectNative: vi.fn(() => port) },
      storage: { local: { get: (_keys, callback) => callback({ enabled: false }), set: vi.fn() } },
    };
    context = vm.createContext({
      chrome, console, testPort: port, importScripts: vi.fn(),
      CarbonPaperImageTransport: { fitPngPayload: vi.fn() },
      setInterval: vi.fn(), setTimeout: vi.fn(), clearTimeout: vi.fn(), navigator: { userAgent: 'Chrome' },
    });
    vm.runInContext(backgroundSource, context);
    vm.runInContext('isEnabled = true; isConnected = true; nmPort = testPort;', context);
  });

  const report = () => vm.runInContext('reportMediaState()', context);

  it('reports only the active tab of the focused browser window', async () => {
    await report();
    expect(chrome.tabs.query).toHaveBeenCalledWith({ active: true, windowId: 1 });
    expect(chrome.tabs.sendMessage).toHaveBeenCalledWith(7, { type: 'getMediaState' });
    expect(port.postMessage).toHaveBeenCalledWith(expect.objectContaining({
      type: 'media_state', focused: true, playing: true, page_title: 'Film',
    }));
  });

  it('excludes private pages and unfocused browser windows', async () => {
    chrome.tabs.query.mockResolvedValue([{ id: 7, title: 'Private film', incognito: true }]);
    await report();
    expect(port.postMessage).toHaveBeenLastCalledWith(expect.objectContaining({ private: true, known: true, playing: false, page_title: '' }));
    expect(chrome.tabs.sendMessage).not.toHaveBeenCalled();
    chrome.tabs.query.mockResolvedValue([{ id: 7, title: 'Film' }]);
    chrome.windows.getLastFocused.mockResolvedValue({ id: 1, focused: false });
    await report();
    expect(port.postMessage).toHaveBeenLastCalledWith(expect.objectContaining({ focused: false, playing: false }));
  });

  it('discards a film response when another tab becomes active', async () => {
    let finish;
    chrome.tabs.sendMessage.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    const pending = report();
    await vi.waitFor(() => expect(chrome.tabs.sendMessage).toHaveBeenCalled());
    chrome.tabs.query.mockResolvedValue([{ id: 8, title: 'Notes' }]);
    finish({ playing: true, pageTitle: 'Film' });
    await pending;
    expect(port.postMessage).not.toHaveBeenCalled();
  });

  it('does not let an older playing response overwrite a newer pause report', async () => {
    let finish;
    chrome.tabs.sendMessage.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }))
      .mockResolvedValue({ playing: false, hasVideo: true, pageTitle: 'Film' });
    const pending = report();
    await vi.waitFor(() => expect(chrome.tabs.sendMessage).toHaveBeenCalled());
    await report();
    finish({ playing: true, pageTitle: 'Film' });
    await pending;
    expect(port.postMessage).toHaveBeenCalledTimes(1);
    expect(port.postMessage).toHaveBeenCalledWith(expect.objectContaining({ playing: false }));
  });
});
