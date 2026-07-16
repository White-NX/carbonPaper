// CarbonPaper Browser Extension - Background Service Worker
// Captures viewport screenshots on demand from the main app via Native Messaging.
// The main app's capture loop detects focused browser windows and sends capture
// requests through NMH, which forwards them here.

importScripts('image_transport.js');

const {
  fitPngPayload,
} = globalThis.CarbonPaperImageTransport;

const NM_HOST_NAME = 'com.carbonpaper.nmh';
const MAX_RETRY = 30;

let nmPort = null;
// Conservative default: do not connect until persisted state is loaded.
let isEnabled = false;
let settingsLoaded = false;
let isConnected = false;
let reconnectTimer = null;
let lastCaptureHash = null;

function ensureNativeConnection(reason = 'unknown') {
  if (!settingsLoaded || !isEnabled || isConnected) return;
  console.log('[CarbonPaper] Ensuring NMH connection, reason:', reason);
  connectNative();
}

// Detect browser process name (legacy compat shim).
// The NMH now injects the real browser exe name detected from its own
// process tree — this UA-sniffed value (which reports every Chromium fork
// as chrome.exe) is only used as a fallback by older NMH binaries that may
// survive an app update until the browser restarts.
function getBrowserName() {
  const ua = navigator.userAgent;
  if (ua.includes('Edg/')) return 'msedge.exe';
  if (ua.includes('Chrome/')) return 'chrome.exe';
  return 'browser-extension';
}

// Native Messaging Connection Management

function scheduleReconnect(delayMs = 5000) {
  if (reconnectTimer !== null) return; // already scheduled
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    ensureNativeConnection('scheduleReconnect');
  }, delayMs);
}

function connectNative() {
  if (!settingsLoaded || !isEnabled) {
    return;
  }

  // Cancel any pending scheduled reconnection — we're connecting now
  if (reconnectTimer !== null) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }

  if (nmPort) {
    try { nmPort.disconnect(); } catch (e) { /* ignore */ }
  }

  try {
    nmPort = chrome.runtime.connectNative(NM_HOST_NAME);
    isConnected = true;

    nmPort.onMessage.addListener((message) => {
      if (message.type === 'capture_request') {
        // Main app is requesting a capture
        captureCurrentTab();
      } else if (message.type === 'nmh_ready') {
        // Startup diagnostic from NMH
        console.log('[CarbonPaper] NMH ready — browser:', message.browser_exe,
          '(pid', message.browser_pid + ')',
          'cmd_pipe:', message.cmd_pipe, 'data_pipe:', message.data_pipe);
      } else if (message.type === 'nmh_registered') {
        // NMH successfully registered its capture session with the main app
        console.log('[CarbonPaper] NMH session registered — browser:',
          message.browser_exe, '(pid', message.browser_pid + ')');
      } else if (message.type === 'nmh_registration_failed') {
        // Capture requests won't reach this browser until this is resolved;
        // screenshot relay (data path) still works.
        console.error('[CarbonPaper] NMH registration failed:', message.error);
      } else if (message.status === 'error') {
        // Suppress expected cold-start errors — the main app hasn't started
        // yet or has restarted with a new auth token.
        const ignorable = message.error && (
          message.error.includes('Authentication failed') ||
          message.error.includes('CarbonPaper not running') ||
          message.error.includes('Cannot connect to CarbonPaper')
        );
        if (ignorable) {
          // Main app not ready yet — reconnect NMH so it re-authenticates
          // once CarbonPaper starts up.
          console.warn('[CarbonPaper] NMH cold-start error, will retry:', message.error);
          if (isEnabled) {
            scheduleReconnect();
          }
        } else {
          console.error('[CarbonPaper] NMH error:', message.error);
        }
      }
    });

    nmPort.onDisconnect.addListener(() => {
      isConnected = false;
      nmPort = null;
      const lastError = chrome.runtime.lastError;
      console.warn('[CarbonPaper] NMH disconnected:', lastError?.message || 'unknown');

      // Reconnect after delay
      if (isEnabled) {
        scheduleReconnect();
      }
    });

    console.log('[CarbonPaper] Connected to NMH');
  } catch (e) {
    isConnected = false;
    console.error('[CarbonPaper] Failed to connect to NMH:', e);

    // Retry after delay
    if (isEnabled && isConnected === false) {
      scheduleReconnect();
    }

  }
}

// Screenshot Capture Logic

async function captureCurrentTab(retry = 0) {
  if (!isEnabled || !isConnected || !nmPort) return;

  try {
    // Get the active tab
    const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
    if (!tab || !tab.id) return;

    // Skip chrome:// and edge:// pages
    if (tab.url && (tab.url.startsWith('chrome://') || tab.url.startsWith('edge://') ||
      tab.url.startsWith('chrome-extension://') || tab.url.startsWith('about:'))) {
      return;
    }

    // Capture the visible tab
    const dataUrl = await chrome.tabs.captureVisibleTab(null, {
      format: 'png'
    });

    if (!dataUrl) return;

    // Extract base64 data (remove data:image/png;base64, prefix). OCR must receive
    // lossless pixels; the desktop app performs its own JPEG encoding for storage.
    const base64Data = dataUrl.split(',')[1];
    if (!base64Data) return;

    // Compute a simple hash to avoid duplicate captures
    const hash = await computeHash(base64Data.substring(0, 1000)); // Hash first 1KB for speed
    if (hash === lastCaptureHash) return;

    // Get page metadata from content script
    let pageData = {
      url: tab.url || '',
      title: tab.title || '',
      favicon: tab.favIconUrl || '',
      visibleLinks: []
    };

    try {
      const response = await chrome.tabs.sendMessage(tab.id, { type: 'getPageData' });
      if (response) {
        pageData = response;
      }
    } catch (e) {
      // Content script may not be injected (e.g., on new tab page)
      // Use tab data as fallback
      if (retry < MAX_RETRY) {
        const nextRetry = retry + 1;
        setTimeout(() => captureCurrentTab(nextRetry), 500);
        return;
      }
    }

    // Convert favicon URL to base64 data URI (same format as local process icons)
    let faviconBase64 = null;
    const faviconUrl = pageData.favicon || tab.favIconUrl;
    if (faviconUrl && !faviconUrl.startsWith('data:')) {
      faviconBase64 = await fetchAsBase64(faviconUrl);
    } else if (faviconUrl && faviconUrl.startsWith('data:')) {
      faviconBase64 = faviconUrl;
    }

    const payloadTemplate = (imageData) => ({
      type: 'save_screenshot',
      image_data: imageData,
      // SHA-256 hex is always 64 ASCII bytes; use a fixed-size placeholder
      // while selecting the image scale so the measured JSON size is exact.
      image_hash: '0'.repeat(64),
      width: tab.width || 0,
      height: tab.height || 0,
      page_url: pageData.url,
      page_title: pageData.title,
      page_icon: faviconBase64,
      visible_links: pageData.visibleLinks || [],
      browser_name: getBrowserName()
    });

    const fitted = await fitPngPayload({
      dataUrl,
      buildPayload: payloadTemplate,
    });
    if (!fitted) {
      console.warn('[CarbonPaper] Screenshot is too large to send over Native Messaging');
      return;
    }

    const imageHash = await computeHash(fitted.base64);
    fitted.payload.image_hash = imageHash;
    nmPort.postMessage(fitted.payload);
    // Only suppress duplicates after a message was actually handed to NMH;
    // transport failures must remain retryable.
    lastCaptureHash = hash;

  } catch (e) {
    // captureVisibleTab can fail if window is minimized, etc.

    if (e.message?.includes('user may be dragging a tab') && retry < MAX_RETRY) {
      // This is a known Chrome bug.
      // We can retry after a short delay.
      const nextRetry = retry + 1;
      console.warn(`[CarbonPaper] Capture failed due to dragging state, retrying (${nextRetry}): `, e.message);
      setTimeout(() => captureCurrentTab(nextRetry), 500);
      return;
    }

    if (!e.message?.includes('No active web contents')) {
      console.warn('[CarbonPaper] Capture failed:', e.message);
    }
  }
}

async function computeHash(data) {
  const encoder = new TextEncoder();
  const buffer = encoder.encode(data);
  const hashBuffer = await crypto.subtle.digest('SHA-256', buffer);
  const hashArray = Array.from(new Uint8Array(hashBuffer));
  return hashArray.map(b => b.toString(16).padStart(2, '0')).join('');
}

/**
 * Fetch a URL and return its content as a base64 data URI.
 * Used to convert favicon URLs to the same format as local process icons.
 */
async function fetchAsBase64(url) {
  try {
    const response = await fetch(url);
    const blob = await response.blob();
    if (blob.size === 0) return null;
    const buffer = await blob.arrayBuffer();
    const bytes = new Uint8Array(buffer);
    let binary = '';
    for (let i = 0; i < bytes.length; i++) {
      binary += String.fromCharCode(bytes[i]);
    }
    const base64 = btoa(binary);
    const mimeType = blob.type || 'image/x-icon';
    return `data:${mimeType};base64,${base64}`;
  } catch (e) {
    return null;
  }
}

// State Persistence and Control

// Load saved state
chrome.storage.local.get(['enabled'], (result) => {
  settingsLoaded = true;
  isEnabled = result.enabled !== false; // Default to true
  if (isEnabled) {
    connectNative();
  } else {
    // Defensive cleanup if a stale worker state ever left a connection open.
    if (reconnectTimer !== null) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
    if (nmPort) {
      try { nmPort.disconnect(); } catch (e) { /* ignore */ }
      nmPort = null;
      isConnected = false;
    }
  }
});

chrome.runtime.onStartup.addListener(() => {
  ensureNativeConnection('runtime.onStartup');
});

chrome.runtime.onInstalled.addListener(() => {
  ensureNativeConnection('runtime.onInstalled');
});

chrome.tabs.onActivated.addListener(() => {
  ensureNativeConnection('tabs.onActivated');
});

chrome.windows.onFocusChanged.addListener((windowId) => {
  if (windowId !== chrome.windows.WINDOW_ID_NONE) {
    ensureNativeConnection('windows.onFocusChanged');
  }
});

// Listen for state changes from popup
chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (message.type === 'setEnabled') {
    settingsLoaded = true;
    isEnabled = message.enabled;
    chrome.storage.local.set({ enabled: isEnabled });

    if (isEnabled) {
      connectNative();
    } else {
      if (reconnectTimer !== null) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }
      if (nmPort) {
        try { nmPort.disconnect(); } catch (e) { /* ignore */ }
        nmPort = null;
        isConnected = false;
      }
    }

    sendResponse({ enabled: isEnabled });
  } else if (message.type === 'getStatus') {
    sendResponse({
      enabled: isEnabled,
      connected: isConnected
    });

    if (settingsLoaded && isEnabled && !isConnected) {
      console.warn('[CarbonPaper] Status requested but main process is not connected. Attempting to reconnect...');
      connectNative();
    }

  }
  return true;
});
