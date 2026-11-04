import { useState } from 'react';
import { checkForUpdate, downloadAndInstallUpdate, openUpdateDownloadPage } from '../../../lib/update_api';
import { formatError } from '../../../lib/errors';

export function useUpdateCheck() {
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [upToDate, setUpToDate] = useState(false);
  const [updateInfo, setUpdateInfo] = useState(null);
  const [updateError, setUpdateError] = useState('');
  const [downloading, setDownloading] = useState(false);
  const [downloadProgress, setDownloadProgress] = useState({ downloaded: 0, contentLength: 0 });

  const handleCheckUpdate = async () => {
    setCheckingUpdate(true);
    setUpToDate(false);
    setUpdateInfo(null);
    setUpdateError('');
    try {
      const result = await checkForUpdate();
      if (result.available) {
        setUpdateInfo({ version: result.version, body: result.body });
      } else {
        setUpToDate(true);
      }
    } catch (err) {
      setUpdateError(formatError(err));
    } finally {
      setCheckingUpdate(false);
    }
  };

  const handleDownloadUpdate = async () => {
    if (!updateInfo) return;
    setDownloading(true);
    setDownloadProgress({ phase: 'downloading', downloaded: 0, contentLength: 0 });
    try {
      await downloadAndInstallUpdate((progress) => {
        setDownloadProgress(progress);
      });
    } catch (err) {
      setUpdateError(formatError(err));
    } finally {
      setDownloading(false);
    }
  };

  return {
    checkingUpdate,
    upToDate,
    updateInfo,
    updateError,
    downloading,
    downloadProgress,
    handleCheckUpdate,
    handleDownloadUpdate,
    handleOpenDownloads: openUpdateDownloadPage,
  };
}
