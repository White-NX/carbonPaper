import React, { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ShieldCheck } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { withAuth } from '../lib/auth_api';
import { formatError } from '../lib/errors';
import { OverlayShell, ProgressBlock } from './overlay';
import { Button } from './ui/Button';
import { Banner } from './ui/Banner';

export default function HmacMigrationDialog() {
  const { t } = useTranslation();
  const [isOpen, setIsOpen] = useState(false);
  const [progress, setProgress] = useState({ processed: 0, total: 0 });
  const [error, setError] = useState(null);

  useEffect(() => {
    let unlistenProgress = null;
    let unlistenComplete = null;
    let isMounted = true;

    const checkAndRun = async () => {
      let didOpen = false;
      try {
        const status = await invoke('storage_check_hmac_migration_status');

        if (!isMounted || !status.needs_migration) return;

        setIsOpen(true);
        didOpen = true;

        const up = await listen('hmac-migration-progress', (event) => {
          if (isMounted) setProgress(event.payload);
        });
        if (!isMounted) {
          up();
          return;
        }
        unlistenProgress = up;

        const uc = await listen('hmac-migration-complete', () => {
          if (isMounted) setIsOpen(false);
        });
        if (!isMounted) {
          uc();
          return;
        }
        unlistenComplete = uc;

        if (!status.is_running) {
          try {
            await withAuth(() => invoke('storage_run_hmac_migration'), { autoPrompt: true });
            if (isMounted) setIsOpen(false);
          } catch (err) {
            if (!formatError(err).includes('ALREADY_RUNNING')) {
              throw err;
            }
          }
        }
      } catch (err) {
        console.error('[HMAC_MIGRATE] Error:', err);
        if (didOpen && isMounted) setError(formatError(err));
      }
    };

    checkAndRun();

    return () => {
      isMounted = false;
      if (unlistenProgress) unlistenProgress();
      if (unlistenComplete) unlistenComplete();
    };
  }, []);

  const close = () => setIsOpen(false);

  return (
    <OverlayShell
      open={isOpen}
      onDismiss={close}
      size="lg"
      icon={ShieldCheck}
      title={t('settings.storageManagement.migration.dialog_title')}
      footer={<Button size="md" onClick={close}>{t('common.close')}</Button>}
    >
      {error ? (
        <Banner tone="error" title={t('settings.storageManagement.migration.error_default')}>{error}</Banner>
      ) : (
        <>
          <ProgressBlock
            label={t('settings.storageManagement.migration.upgrading')}
            current={progress.processed}
            total={progress.total}
          />
          <Banner tone="info">{t('settings.storageManagement.migration.background_tip')}</Banner>
        </>
      )}
    </OverlayShell>
  );
}
