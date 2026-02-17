import { check } from '@tauri-apps/plugin-updater';

/**
 * Check for available updates from GitHub Releases.
 * @returns {{ available: boolean, version?: string, body?: string, update?: object }}
 */
export async function checkForUpdate() {
  const update = await check();
  if (update) {
    return {
      available: true,
      version: update.version,
      body: update.body,
      update,
    };
  }
  return { available: false };
}

/**
 * Download and install the update.
 * @param {object} update - The update object returned from checkForUpdate().update
 * @param {function} [onProgress] - Progress callback ({ downloaded, contentLength })
 */
export async function downloadAndInstallUpdate(update, onProgress) {
  let downloaded = 0;
  let contentLength = 0;

  await update.downloadAndInstall((event) => {
    switch (event.event) {
      case 'Started':
        contentLength = event.data.contentLength || 0;
        break;
      case 'Progress':
        downloaded += event.data.chunkLength;
        if (onProgress) {
          onProgress({ downloaded, contentLength });
        }
        break;
      case 'Finished':
        if (onProgress) {
          onProgress({ downloaded: contentLength, contentLength });
        }
        break;
    }
  });
}
