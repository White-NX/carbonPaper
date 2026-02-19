import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

/**
 * Check for available updates from GitHub Releases.
 * @returns {{ available: boolean, version?: string, body?: string }}
 */
export async function checkForUpdate() {
  const result = await invoke('updater_check');
  if (result.available) {
    return {
      available: true,
      version: result.version,
      body: result.notes,
    };
  }
  return { available: false };
}

/**
 * Download, extract, and apply the update.
 * @param {function} [onProgress] - Progress callback ({ downloaded, contentLength })
 */
export async function downloadAndInstallUpdate(onProgress) {
  // Listen for download progress events
  const unlisten = onProgress
    ? await listen('updater-download-progress', (event) => {
        onProgress({
          downloaded: event.payload.downloaded,
          contentLength: event.payload.content_length,
        });
      })
    : null;

  try {
    await invoke('updater_download');

    // Signal 100% before extract phase
    if (onProgress) {
      onProgress({ downloaded: 1, contentLength: 1 });
    }

    await invoke('updater_extract');
    await invoke('updater_apply');
  } finally {
    if (unlisten) unlisten();
  }
}
