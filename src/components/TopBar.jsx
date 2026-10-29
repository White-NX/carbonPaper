import React from 'react';
import { Moon, Sun, Settings, Bell, Minus, Square, X, Copy, Loader2, Search, ExternalLink } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Button } from './ui/Button';
import { APP_VERSION } from '../lib/version';
import BrandMark from './BrandMark';

function ServiceStatusBadge({ backendStatus, powerSavingSuppressed, monitorPaused, handleStartBackend, handlePauseMonitor, handleResumeMonitor, locked }) {
  const { t } = useTranslation();
  let dotColor, label, onClick, disabled = false, showSpinner = false;
  if (backendStatus === 'online') {
    dotColor = monitorPaused ? 'bg-ide-warning' : 'bg-ide-success';
    label = t(monitorPaused ? 'topbar.service.paused' : 'topbar.service.running');
    onClick = monitorPaused ? handleResumeMonitor : handlePauseMonitor;
  } else if (backendStatus === 'waiting') {
    dotColor = 'bg-ide-warning'; label = t('topbar.service.starting'); disabled = true; showSpinner = true;
  } else if (powerSavingSuppressed) {
    dotColor = 'bg-ide-warning'; label = t('topbar.service.powerSavingOffline'); disabled = true;
  } else {
    dotColor = 'bg-ide-error'; label = t('topbar.service.offline'); onClick = handleStartBackend;
  }
  return (
    <Button variant="ghost" size="xs" onClick={onClick} disabled={disabled || locked}
      className="workspace-focus max-w-48 gap-2 px-2" title={label} aria-label={label} data-tauri-drag-region="false">
      {showSpinner ? <Loader2 className="h-3 w-3 animate-spin" aria-hidden="true" /> : <span className={`h-2 w-2 shrink-0 rounded-full ${dotColor}`} />}
      <span className="hidden min-w-0 truncate lg:block">{label}</span>
    </Button>
  );
}

export default function TopBar({
  darkMode, setDarkMode, setShowSettings, showNotifications, setShowNotifications,
  isMaximized, onMinimize, onToggleMaximize, onHideToTray, onOpenSearch, previewCount = 0, onRestorePreviews,
  backendStatus, powerSavingSuppressed = false, monitorPaused,
  handleStartBackend, handlePauseMonitor, handleResumeMonitor, isAuthenticated, interfaceLocked = false,
}) {
  const { t } = useTranslation();
  const settingsDisabled = !isAuthenticated || interfaceLocked;
  return (
    <header data-tauri-drag-region className="flex h-12 shrink-0 select-none items-center justify-between gap-3 px-3">
      <div className="pointer-events-none flex shrink-0 items-center gap-3">
        <div className="flex items-center gap-2 text-ide-text">
          <BrandMark className="h-5 w-5 text-ide-muted" />
          <span className="font-semibold tracking-tight">CarbonPaper</span>
        </div>
        <span className="hidden text-xs text-ide-muted xl:block">{APP_VERSION}</span>
      </div>
      <Button variant="secondary" icon={Search} onClick={onOpenSearch} disabled={settingsDisabled}
        aria-label={t('workspace.openSearch')} aria-keyshortcuts="Control+K" data-tauri-drag-region="false"
        className="workspace-focus h-8 min-w-0 flex-1 justify-start gap-2 px-3 text-ide-muted md:max-w-[480px]">
        <span className="min-w-0 flex-1 truncate text-left">{t('workspace.openSearch')}</span>
        <kbd className="hidden text-[10px] text-ide-muted sm:block">Ctrl K</kbd>
      </Button>
      <div className="flex shrink-0 items-center gap-0.5">
        {previewCount > 0 && <Button variant="ghost" icon={ExternalLink} disabled={settingsDisabled} onClick={onRestorePreviews}
          title={t('snapshotPreview.restoreWindows', { count: previewCount })} aria-label={t('snapshotPreview.restoreWindows', { count: previewCount })}
          className="workspace-focus h-8 gap-1 px-2"><span className="text-[10px] tabular-nums">{previewCount}</span></Button>}
        <ServiceStatusBadge backendStatus={backendStatus} powerSavingSuppressed={powerSavingSuppressed} monitorPaused={monitorPaused}
          handleStartBackend={handleStartBackend} handlePauseMonitor={handlePauseMonitor} handleResumeMonitor={handleResumeMonitor} locked={settingsDisabled} />
        <Button variant="ghost" icon={Settings} disabled={settingsDisabled} onClick={() => setShowSettings(true)}
          title={t('topbar.settings')} aria-label={t('topbar.settings')} className="workspace-focus h-8 w-8 px-0" />
        <Button variant="ghost" icon={Bell} onClick={() => setShowNotifications(!showNotifications)}
          title={t('topbar.notifications')} aria-label={t('topbar.notifications')} className="workspace-focus h-8 w-8 px-0" />
        <Button variant="ghost" icon={darkMode ? Sun : Moon} onClick={() => setDarkMode(!darkMode)}
          title={t(darkMode ? 'topbar.lightTheme' : 'topbar.darkTheme')} aria-label={t(darkMode ? 'topbar.lightTheme' : 'topbar.darkTheme')}
          className="workspace-focus h-8 w-8 px-0" />
        <div className="mx-1 h-4 w-px bg-ide-border" />
        <Button variant="ghost" icon={Minus} onClick={onMinimize} title={t('topbar.minimize')} aria-label={t('topbar.minimize')} className="workspace-focus h-8 w-8 px-0" />
        <Button variant="ghost" icon={isMaximized ? Copy : Square} onClick={onToggleMaximize}
          title={t(isMaximized ? 'topbar.restore' : 'topbar.maximize')} aria-label={t(isMaximized ? 'topbar.restore' : 'topbar.maximize')}
          className="workspace-focus h-8 w-8 px-0" />
        <Button variant="ghost" icon={X} onClick={onHideToTray} title={t('topbar.hideToTray')} aria-label={t('topbar.hideToTray')}
          className="workspace-focus h-8 w-8 px-0 hover:bg-ide-error hover:text-white" />
      </div>
    </header>
  );
}
