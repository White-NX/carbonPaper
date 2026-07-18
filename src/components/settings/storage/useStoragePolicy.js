import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../../lib/auth_api';

export function useStoragePolicy({ t }) {
  const [storageLimit, setStorageLimitState] = useState(() => {
    return localStorage.getItem('snapshotStorageLimit') || 'unlimited';
  });
  const [retentionPeriod, setRetentionPeriodState] = useState(() => {
    return localStorage.getItem('snapshotRetentionPeriod') || 'permanent';
  });
  const policyRef = useRef({ storageLimit, retentionPeriod });
  const userEditedRef = useRef(false);

  useEffect(() => {
    policyRef.current = { storageLimit, retentionPeriod };
  }, [storageLimit, retentionPeriod]);

  useEffect(() => {
    (async () => {
      try {
        const resp = await withAuth(() => invoke('storage_get_policy'));
        if (userEditedRef.current || !resp || typeof resp !== 'object') return;
        // The backend policy is authoritative; a missing value means the
        // corresponding limit is disabled, so never push the cached value up.
        const backendLimit = resp.storage_limit ? String(resp.storage_limit) : 'unlimited';
        const backendRetention = resp.retention_period ? String(resp.retention_period) : 'permanent';
        setStorageLimitState(backendLimit);
        setRetentionPeriodState(backendRetention);
        localStorage.setItem('snapshotStorageLimit', backendLimit);
        localStorage.setItem('snapshotRetentionPeriod', backendRetention);
      } catch {
        // Keep localStorage values for display when the backend is unavailable.
      }
    })();
  }, []);

  const persistPolicy = async (nextLimit, nextRetention) => {
    userEditedRef.current = true;
    setStorageLimitState(nextLimit);
    setRetentionPeriodState(nextRetention);
    localStorage.setItem('snapshotStorageLimit', nextLimit);
    localStorage.setItem('snapshotRetentionPeriod', nextRetention);
    try {
      await withAuth(() => invoke('storage_set_policy', { policy: { storage_limit: nextLimit, retention_period: nextRetention } }));
    } catch {
      // Backend may be unavailable in dev; localStorage remains the fallback.
    }
  };

  const setStorageLimit = (value) => persistPolicy(value, policyRef.current.retentionPeriod);
  const setRetentionPeriod = (value) => persistPolicy(policyRef.current.storageLimit, value);

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
