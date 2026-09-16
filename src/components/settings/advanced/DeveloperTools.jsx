import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../../lib/auth_api';
import { SettingsButton } from '../SettingsControls';
import { SettingsDisclosure, SettingsGroup, SettingsStatus } from '../SettingsPrimitives';

const PREVIEWS = ['error', 'security', 'ocr', 'update', 'critical-update', 'extension', 'clustering', 'smart-cluster', 'app-bound', 'app-bound-repair'];

export default function DeveloperTools() {
  const { t } = useTranslation();
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  if (!import.meta.env.DEV) return null;
  return <SettingsDisclosure title={t('settings.about.developers')}>
    <SettingsGroup className="space-y-3">
      <div className="flex flex-wrap gap-2">{PREVIEWS.map((preview) => <SettingsButton key={preview} disabled={busy} onClick={async () => {
        setBusy(true); setError('');
        try { await withAuth(() => invoke('settings_debug_preview', { preview }), { autoPrompt: true }); }
        catch (cause) { setError(String(cause)); }
        finally { setBusy(false); }
      }}>{t(`settings.about.previews.${preview}`)}</SettingsButton>)}</div>
      {error && <SettingsStatus tone="error">{error}</SettingsStatus>}
    </SettingsGroup>
  </SettingsDisclosure>;
}
