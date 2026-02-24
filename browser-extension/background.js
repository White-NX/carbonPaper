// CarbonPaper Browser Extension - Background Service Worker
// Captures viewport screenshots on demand from the main app via Native Messaging.
// The main app's capture loop detects focused browser windows and sends capture
// requests through NMH, which forwards them here.

const NM_HOST_NAME = 'com.carbonpaper.nmh';

let nmPort = null;
let isEnabled = true;
let isConnected = false;
let lastCaptureHash = null;

// Detect browser process name
function getBrowserName() {
  const ua = navigator.userAgent;
  if (ua.includes('Edg/')) return 'msedge.exe';
  if (ua.includes('Chrome/')) return 'chrome.exe';
  return 'browser-extension';
}

// Native Messaging Connection Management

function connectNative() {
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
        console.log('[CarbonPaper] NMH ready — browser:', message.browser_type,
                    'cmd_pipe:', message.cmd_pipe, 'data_pipe:', message.data_pipe);
      } else if (message.status === 'error') {
        // Suppress expected cold-start errors — the main app hasn't started
        // yet or has restarted with a new auth token.
        const ignorable = message.error && (
          message.error.includes('Authentication failed') ||
          message.error.includes('CarbonPaper not running') ||
          message.error.includes('Cannot connect to CarbonPaper')
        );
        if (!ignorable) {
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
        setTimeout(connectNative, 5000);
      }
    });

    console.log('[CarbonPaper] Connected to NMH');
  } catch (e) {
    isConnected = false;
    console.error('[CarbonPaper] Failed to connect to NMH:', e);
  }
}

// Screenshot Capture Logic

async function captureCurrentTab() {
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
      format: 'jpeg',
      quality: 70
    });

    if (!dataUrl) return;

    // Extract base64 data (remove data:image/jpeg;base64, prefix)
    const base64Data = dataUrl.split(',')[1];
    if (!base64Data) return;

    // Compute a simple hash to avoid duplicate captures
    const hash = await computeHash(base64Data.substring(0, 1000)); // Hash first 1KB for speed
    if (hash === lastCaptureHash) return;
    lastCaptureHash = hash;

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
    }

    // Convert favicon URL to base64 data URI (same format as local process icons)
    let faviconBase64 = null;
    const faviconUrl = pageData.favicon || tab.favIconUrl;
    if (faviconUrl && !faviconUrl.startsWith('data:')) {
      faviconBase64 = await fetchAsBase64(faviconUrl);
    } else if (faviconUrl && faviconUrl.startsWith('data:')) {
      faviconBase64 = faviconUrl;
    }

    // Compute full image hash
    const imageHash = await computeHash(base64Data);

    // Send to NMH as a single message
    nmPort.postMessage({
      type: 'save_screenshot',
      image_data: base64Data,
      image_hash: imageHash,
      width: tab.width || 0,
      height: tab.height || 0,
      page_url: pageData.url,
      page_title: pageData.title,
      page_icon: faviconBase64,
      visible_links: pageData.visibleLinks || [],
      browser_name: getBrowserName()
    });

  } catch (e) {
    // captureVisibleTab can fail if window is minimized, etc.
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
  isEnabled = result.enabled !== false; // Default to true
  if (isEnabled) {
    connectNative();
  }
});

// Listen for state changes from popup
chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (message.type === 'setEnabled') {
    isEnabled = message.enabled;
    chrome.storage.local.set({ enabled: isEnabled });

    if (isEnabled) {
      connectNative();
    } else {
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
  }
  return true;
});
