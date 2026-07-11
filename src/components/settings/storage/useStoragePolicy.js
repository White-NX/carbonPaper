import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../../lib/auth_api';

export function useStoragePolicy({ t }) {
  const [storageLimit, setStorageLimit] = useState(() => {
    return localStorage.getItem('snapshotStorageLimit') || 'unlimited';
  });
  const [retentionPeriod, setRetentionPeriod] = useState(() => {
    return localStorage.getItem('snapshotRetentionPeriod') || 'permanent';
  });

  useEffect(() => {
    localStorage.setItem('snapshotStorageLimit', storageLimit);
    (async () => {
      try {
        await withAuth(() => invoke('storage_set_policy', { policy: { storage_limit: storageLimit, retention_period: retentionPeriod } }));
      } catch {
        // Backend may be unavailable in dev; localStorage remains the fallback.
      }
    })();
  }, [storageLimit]);

  useEffect(() => {
    localStorage.setItem('snapshotRetentionPeriod', retentionPeriod);
    (async () => {
      try {
        await withAuth(() => invoke('storage_set_policy', { policy: { storage_limit: storageLimit, retention_period: retentionPeriod } }));
      } catch {
        // Ignore backend sync failures here; the settings remain locally persisted.
      }
    })();
  }, [retentionPeriod]);

  useEffect(() => {
    (async () => {
      try {
        const resp = await withAuth(() => invoke('storage_get_policy'));
        if (resp && typeof resp === 'object') {
          if (resp.storage_limit) setStorageLimit(String(resp.storage_limit));
          if (resp.retention_period) setRetentionPeriod(String(resp.retention_period));
        }
      } catch {
        // Keep localStorage values when the backend is unavailable.
      }
    })();
  }, []);

  const storageLimitOptions = [
    { value: '10', label: t('settings.storageManagement.storageLimit.options.10') },
    { value: '20', label: t('settings.storageManagement.storageLimit.options.20') },
    { value: '50', label: t('settings.storageManagement.storageLimit.options.50') },
    { value: '120', label: t('settings.storageManagement.storageLimit.options.120') },
    { value: 'unlimited', label: t('settings.storageManagement.storageLimit.options.unlimited') },
  ];

  const retentionOptions = [
    { value: '1month', label: t('settings.storageManagement.retention.options.1month') },
    { value: '6months', label: t('settings.storageManagement.retention.options.6months') },
    { value: '1year', label: t('settings.storageManagement.retention.options.1year') },
    { value: '2years', label: t('settings.storageManagement.retention.options.2years') },
    { value: 'permanent', label: t('settings.storageManagement.retention.options.permanent') },
  ];

  return {
    storageLimit,
    setStorageLimit,
    retentionPeriod,
    setRetentionPeriod,
    storageLimitOptions,
    retentionOptions,
  };
}
