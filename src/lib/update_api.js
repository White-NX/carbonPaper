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
      critical: result.critical,
    };
  }
  return { available: false };
}

/**
 * Download, extract, and apply the update.
 * @param {function} [onProgress] - Progress callback ({ downloaded, contentLength })
 */
export async function downloadAndInstallUpdate(onProgress) {
  const emitProgress = (progress) => {
    if (onProgress) onProgress(progress);
  };

  // Listen for download progress events
  const unlisten = onProgress
    ? await listen('updater-download-progress', (event) => {
        emitProgress({
          phase: 'downloading',
          downloaded: event.payload.downloaded,
          contentLength: event.payload.content_length,
        });
      })
    : null;

  try {
    emitProgress({ phase: 'downloading', downloaded: 0, contentLength: 0 });
    await invoke('updater_download');

    emitProgress({ phase: 'extracting', downloaded: 1, contentLength: 1 });
    await invoke('updater_extract');
    emitProgress({ phase: 'applying', downloaded: 1, contentLength: 1 });
    await invoke('updater_apply');
  } finally {
    if (unlisten) unlisten();
  }
}
