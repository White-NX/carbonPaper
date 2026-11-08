import React from 'react';
import { useTranslation } from 'react-i18next';
import { ConfirmDialog } from '../../ConfirmDialog';

export default function UnattendedWarning({ open, loading, onConfirm, onCancel }) {
  const { t } = useTranslation();
  return <ConfirmDialog isOpen={open} loading={loading} onConfirm={onConfirm} onCancel={onCancel}
    title={t('settings.security.warning.title')} message={t('settings.security.warning.message')}
    confirmLabel={t('settings.security.warning.confirm')} cancelLabel={t('common.cancel')}
    loadingLabel={t('settings.security.feedback.applying')} confirmVariant="danger" />;
}
