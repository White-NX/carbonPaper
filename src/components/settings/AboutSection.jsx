import React from 'react';
import { useTranslation } from 'react-i18next';
import { Github, RefreshCw } from 'lucide-react';
import { openUrl } from '@tauri-apps/plugin-opener';
import { APP_VERSION } from '../../lib/version';
import appIcon from '../../../src-tauri/icons/128x128.png';
import { SettingsButton } from './SettingsControls';
import { SettingsDivider, SettingsErrorBanner, SettingsGroup, SettingsRow, SettingsStatus } from './SettingsPrimitives';

export default function AboutSection({ checking, upToDate, onCheckUpdate, updateInfo, updateError, downloading, downloadProgress, onDownloadUpdate }) {
  const { t } = useTranslation();
  const phase = downloadProgress?.phase || 'downloading';
  const progress = downloadProgress?.contentLength > 0 ? Math.min(1, downloadProgress.downloaded / downloadProgress.contentLength) : undefined;
  return <div className="space-y-6">
    <div className="flex items-center gap-4 py-2">
      <img src={appIcon} alt="" className="h-14 w-14 shrink-0" />
      <div><h2 className="text-2xl font-semibold tracking-tight">CarbonPaper</h2><p className="mt-1 text-sm text-ide-muted">{APP_VERSION}</p></div>
    </div>
    <SettingsGroup className="space-y-3">
      <SettingsRow label={t('settings.about.updates')} description={updateInfo ? t('settings.about.available', { version: updateInfo.version }) : upToDate ? t('settings.about.latest') : APP_VERSION}
        control={<SettingsButton icon={RefreshCw} onClick={onCheckUpdate} disabled={checking || downloading}>{t(checking ? 'settings.about.checking' : 'settings.about.check')}</SettingsButton>} />
      {updateError && <SettingsErrorBanner>{updateError}</SettingsErrorBanner>}
      {downloading ? <div className="space-y-2">
        <SettingsStatus>{t(phase === 'applying' ? 'updateModal.applying' : phase === 'extracting' ? 'updateModal.extracting' : 'updateModal.downloading')}</SettingsStatus>
        <progress className="h-1.5 w-full accent-ide-accent" max={1} value={phase === 'downloading' ? progress : undefined} aria-label={t('settings.about.download')} />
      </div> : updateInfo && <SettingsButton variant="primary" onClick={onDownloadUpdate}>{t('settings.about.download')}</SettingsButton>}
    </SettingsGroup>
    <SettingsGroup>
      <div className="space-y-2 text-xs leading-relaxed text-ide-muted"><p>{t('settings.about.license')}</p><p>{t('settings.about.author')}</p></div>
      <SettingsDivider />
      <SettingsRow label={t('settings.about.repository')} description={t('settings.about.repositoryDescription')}
        control={<SettingsButton icon={Github} onClick={() => openUrl('https://github.com/White-NX/carbonPaper')}>GitHub</SettingsButton>} />
    </SettingsGroup>
  </div>;
}
