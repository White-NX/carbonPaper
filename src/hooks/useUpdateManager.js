import { useEffect, useState } from 'react';
import { checkForUpdate, downloadAndInstallUpdate } from '../lib/update_api';

export function useUpdateManager() {
  const [updateModalVisible, setUpdateModalVisible] = useState(false);
  const [updateInfo, setUpdateInfo] = useState(null);
  const [updateDownloading, setUpdateDownloading] = useState(false);
  const [updateDownloadProgress, setUpdateDownloadProgress] = useState(null);
  const [updateDownloadError, setUpdateDownloadError] = useState(null);

  useEffect(() => {
    const timer = setTimeout(async () => {
      try {
        const result = await checkForUpdate();
        if (result.available) {
          const dismissedVersion = localStorage.getItem('updateDismissed');
          if (result.critical || dismissedVersion !== result.version) {
            setUpdateInfo(result);
            setUpdateModalVisible(true);
          }
        }
      } catch {
        // Network failure is non-fatal at startup.
      }
    }, 5000);
    return () => clearTimeout(timer);
  }, []);

  useEffect(() => {
    const handler = (e) => {
      setUpdateInfo({
        version: '9.9.9-debug',
        body: 'This is a debug update payload.\n- It supports multiline text.\n- And lists.\n\nEnjoy testing the update modal!',
        critical: e.detail?.critical || false,
      });
      setUpdateModalVisible(true);
    };
    window.addEventListener('debug-update-modal', handler);
    return () => window.removeEventListener('debug-update-modal', handler);
  }, []);

  const handleDownloadUpdate = async () => {
    setUpdateDownloading(true);
    setUpdateDownloadError(null);
    setUpdateDownloadProgress({ phase: 'downloading', downloaded: 0, contentLength: 0 });
    try {
      await downloadAndInstallUpdate((progress) => {
        setUpdateDownloadProgress(progress);
      });
    } catch (err) {
      setUpdateDownloadError(err.message || String(err));
      setUpdateDownloading(false);
    }
  };

  const handleLater = () => {
    setUpdateModalVisible(false);
    if (updateInfo) {
      localStorage.setItem('updateDismissed', updateInfo.version);
    }
  };

  return {
    updateModalVisible,
    updateInfo,
    updateDownloading,
    updateDownloadProgress,
    updateDownloadError,
    setUpdateModalVisible,
    handleDownloadUpdate,
    handleLater,
  };
}
