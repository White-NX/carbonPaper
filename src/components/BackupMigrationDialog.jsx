import React from 'react';
import { useTranslation } from 'react-i18next';
import { Dialog } from './Dialog';
import { Lock, FileUp, FileDown, ShieldAlert, CheckCircle2, RefreshCw } from 'lucide-react';
import { Button } from './ui/Button';
import { Banner } from './ui/Banner';
import { ProgressBlock } from './overlay/ProgressBlock';
import { useBackupMigration } from '../hooks/useBackupMigration';

export default function BackupMigrationDialog({ isOpen, onClose, mode = 'export' }) {
  const { t } = useTranslation();
  const {
    password, setPassword, confirmPassword, setConfirmPassword,
    status, error, progress, cancelState, handleAction, handleCancel, handleClose, handleRetry,
  } = useBackupMigration({ mode, t, onClose });

  const title = mode === 'export' 
    ? t('settings.backup.export_title')
    : t('settings.backup.import_title');

  return (
    <Dialog isOpen={isOpen} onClose={handleClose} title={title} disableClose={status === 'processing'}>
      <div className="p-6 space-y-6">
        {status === 'idle' && (
          <>
            <Banner icon={ShieldAlert} className="p-4 rounded-xl">
              {mode === 'export'
                ? t('settings.backup.export_hint')
                : t('settings.backup.import_hint')}
            </Banner>

            <div className="space-y-4">
              <div className="space-y-1.5">
                <label className="text-xs font-medium text-ide-muted ml-1">{t('settings.backup.password_label')}</label>
                <div className="relative">
                  <input
                    type="password"
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                    className="w-full bg-ide-panel border border-ide-border rounded-lg pl-9 pr-3 py-2 text-sm focus:outline-none focus:border-ide-accent"
                    placeholder={t('settings.backup.password_placeholder')}
                  />
                  <Lock className="absolute left-3 top-2.5 w-4 h-4 text-ide-muted" />
                </div>
              </div>

              {mode === 'export' && (
                <div className="space-y-1.5">
                  <label className="text-xs font-medium text-ide-muted ml-1">{t('settings.backup.confirm_password_label')}</label>
                  <div className="relative">
                    <input
                      type="password"
                      value={confirmPassword}
                      onChange={(e) => setConfirmPassword(e.target.value)}
                      className="w-full bg-ide-panel border border-ide-border rounded-lg pl-9 pr-3 py-2 text-sm focus:outline-none focus:border-ide-accent"
                      placeholder={t('settings.backup.confirm_password_placeholder')}
                    />
                    <Lock className="absolute left-3 top-2.5 w-4 h-4 text-ide-muted" />
                  </div>
                </div>
              )}
            </div>

            {error && (
              <Banner tone="error">{error}</Banner>
            )}

            <div className="flex justify-end gap-3 pt-2">
              <Button size="md"
                onClick={handleClose}
              >
                {t('common.cancel')}
              </Button>
              <Button variant="primary" size="md" icon={mode === 'export' ? FileDown : FileUp}
                onClick={handleAction}
              >
                {mode === 'export' ? t('settings.backup.export_btn') : t('settings.backup.import_btn')}
              </Button>
            </div>
          </>
        )}

        {status === 'processing' && (
          <div className="py-8 flex flex-col items-center justify-center space-y-4">
            <RefreshCw className="w-10 h-10 text-ide-accent animate-spin" />
            <div className="text-center w-full max-w-sm">
              <div className="text-sm font-medium">
                {cancelState === 'pending' ? t('settings.backup.cancelling')
                  : cancelState === 'finishing' ? t('settings.backup.phases.finalize')
                    : progress?.phase ? t(`settings.backup.phases.${progress.phase}`) : t('settings.backup.processing')}
              </div>
              <ProgressBlock
                label={t('settings.backup.processing_hint')}
                percent={progress?.total_files > 0 ? progress.copied_files / progress.total_files * 100 : undefined}
                className="mt-4 text-left"
              />
            </div>
            {error && <Banner tone="error">{error}</Banner>}
            <Button size="md"
              onClick={handleCancel}
              disabled={cancelState !== 'idle'}
            >
              {t('common.cancel')}
            </Button>
          </div>
        )}

        {status === 'success' && (
          <div className="py-6 flex flex-col items-center justify-center space-y-4">
            <CheckCircle2 className="w-12 h-12 text-green-500" />
            <div className="text-center">
              <div className="text-sm font-medium">{t('settings.backup.success_title')}</div>
              <p className="text-xs text-ide-muted mt-2 max-w-[280px]">
                {mode === 'export' 
                  ? t('settings.backup.export_success_hint')
                  : t('settings.backup.import_success_hint')}
              </p>
            </div>
            <Button size="md"
              onClick={handleClose}
              className="mt-4 px-6"
            >
              {t('common.close')}
            </Button>
          </div>
        )}

        {status === 'error' && (
          <div className="py-6 flex flex-col items-center justify-center space-y-4">
            <div className="w-12 h-12 rounded-full bg-red-500/10 flex items-center justify-center">
              <ShieldAlert className="w-7 h-7 text-red-500" />
            </div>
            <div className="text-center">
              <div className="text-sm font-medium text-red-500">{t('settings.backup.error_title')}</div>
              <p className="text-xs text-ide-muted mt-2 break-all max-w-[300px]">
                {error}
              </p>
            </div>
            <div className="flex gap-3 pt-2">
              <Button size="md"
                onClick={handleRetry}
              >
                {t('common.retry')}
              </Button>
              <Button size="md"
                onClick={handleClose}
              >
                {t('common.close')}
              </Button>
            </div>
          </div>
        )}
      </div>
    </Dialog>
  );
}
