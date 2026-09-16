import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Trash2 } from 'lucide-react';
import { ConfirmDialog } from '../ConfirmDialog';
import { SettingsButton, SettingsSelect } from './SettingsControls';
import { SettingsGroup, SettingsSection, SettingsStatus } from './SettingsPrimitives';
import { useSettingsActivity } from './SettingsActivityContext';

export default function QuickDeleteSection({ onDelete, isDeleting, deleteMessage, deleteMessageType = 'neutral' }) {
  const { t } = useTranslation();
  const [range, setRange] = useState('');
  const [pending, setPending] = useState(null);
  const [confirming, setConfirming] = useState(false);
  useSettingsActivity('quick-delete', { busy: confirming || isDeleting });
  const options = [
    { value: '', label: t('settings.captureFilters.quickDelete.chooseRange'), disabled: true },
    ...[{ value: '5min', minutes: 5 }, { value: '30min', minutes: 30 }, { value: '1hour', minutes: 60 }, { value: 'today', minutes: 'today' }]
      .map((item) => ({ ...item, label: t(`settings.captureFilters.quickDelete.options.${item.value}`) })),
  ];
  const confirm = async () => {
    if (!pending || confirming) return;
    setConfirming(true);
    try { await onDelete(pending.minutes); }
    finally { setConfirming(false); setPending(null); }
  };
  return (
    <SettingsSection id="clear-records" title={t('settings.captureFilters.quickDelete.title')}>
      <SettingsGroup className="space-y-3">
        <p className="text-xs text-ide-muted">{t('settings.captureFilters.quickDelete.description')}</p>
        <div className="flex flex-wrap gap-3">
          <SettingsSelect label={t('settings.captureFilters.quickDelete.range')} value={range} options={options} onChange={setRange} disabled={isDeleting || Boolean(pending)} />
          <SettingsButton variant="danger" icon={Trash2} disabled={!range || isDeleting || Boolean(pending)}
            onClick={() => setPending(options.find((option) => option.value === range))}>{t('settings.captureFilters.quickDelete.action')}</SettingsButton>
        </div>
        {deleteMessage && <SettingsStatus tone={deleteMessageType}>{deleteMessage}</SettingsStatus>}
      </SettingsGroup>
      <ConfirmDialog isOpen={Boolean(pending)} onCancel={() => { if (!confirming) setPending(null); }} onConfirm={confirm}
        title={t('settings.captureFilters.quickDelete.confirmTitle')}
        message={t('settings.captureFilters.quickDelete.confirmRange', { range: pending?.label || '' })}
        confirmLabel={t('settings.captureFilters.quickDelete.yes')} cancelLabel={t('settings.captureFilters.quickDelete.no')}
        confirmVariant="danger" loading={confirming} loadingLabel={t('common.processing')} />
    </SettingsSection>
  );
}
