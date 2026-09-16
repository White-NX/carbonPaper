import React, { useCallback, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Settings, Shield, Activity, Image as ImageIcon, Database, HardDrive, Wrench, ScanLine, Sparkles, SlidersHorizontal, Info } from 'lucide-react';
import MonitorServiceSection from './MonitorServiceSection';
import GeneralOptionsSection from './GeneralOptionsSection';
import CaptureFiltersSection from './CaptureFiltersSection';
import QuickDeleteSection from './QuickDeleteSection';
import SecuritySection from './SecuritySection';
import StorageManagementSection from './StorageManagementSection';
import AboutSection from './AboutSection';
import AdvancedSection from './AdvancedSection';
import FeaturesSection from './FeaturesSection';
import LanguageSection from './LanguageSection';
import BrowserExtensionSection from './BrowserExtensionSection';
import AiEmbeddingSection from './AiEmbeddingSection';
import ProtectedProcessingCard from './advanced/ProtectedProcessingCard';
import { BackgroundSchedulerCard } from './advanced/InferenceCards';
import { SettingsButton, SettingsSwitch } from './SettingsControls';
import { SettingsGroup, SettingsRow, SettingsSection, SettingsErrorBanner, SettingsWarningBanner } from './SettingsPrimitives';
import { SettingsPageContext, useSettingsActivity } from './SettingsActivityContext';
import IndexMaintenanceSection from './IndexMaintenanceSection';
import { useSettingsDialogController } from './useSettingsDialogController';
import { useAdvancedSectionController } from './useAdvancedSectionController';
import { useTauriEventListener } from '../../hooks/useTauriEventListener';

const TABS = [
  ['general', Settings], ['capture', ScanLine], ['organize', Sparkles], ['privacy', Shield],
  ['maintenance', Wrench], ['advanced', SlidersHorizontal], ['about', Info],
];
function initialTab() {
  const value = new URLSearchParams(window.location.search).get('tab') || 'general';
  return TABS.some(([id]) => id === value) ? value : 'general';
}

// Window-independent content; native lifecycle belongs to SettingsWindow.
export default function SettingsContent({
  isOpen = true, autoStartMonitor, onAutoStartMonitorChange, onRecordsDeleted,
  sessionTimeout, onSessionTimeoutChange, isSessionValid, onLockSession,
  powerSavingSuppressed, powerSavingMode, onPowerSavingModeChange, preferenceError,
}) {
  const { t } = useTranslation();
  const [activeTab, setActiveTab] = useState(initialTab);
  const [visited, setVisited] = useState(() => new Set([initialTab()]));
  const [restarting, setRestarting] = useState(false);
  const navigationRef = useRef(null);
  const pendingSection = useRef(new URLSearchParams(window.location.search).get('section'));
  const c = useSettingsDialogController({ isOpen, activeTab, onRecordsDeleted, t });
  const runtimeActive = isOpen && ['privacy', 'maintenance', 'advanced'].includes(activeTab);
  const runtime = useAdvancedSectionController({ monitorStatus: c.monitorStatus, t, active: runtimeActive, diagnosticsActive: isOpen && activeTab === 'advanced' });
  useSettingsActivity('monitor-control', { busy: restarting || ['loading', 'waiting'].includes(c.monitorStatus) });
  useSettingsActivity('update', { busy: c.downloading });
  const navigate = useCallback((tab, section) => {
    const next = TABS.some(([id]) => id === tab) ? tab : 'general';
    setVisited((current) => new Set([...current, next]));
    setActiveTab(next);
    localStorage.setItem('settings.lastTab', next);
    pendingSection.current = section;
  }, []);
  useTauriEventListener('settings-navigate', ({ payload }) => {
    if (typeof payload?.tab === 'string') navigate(payload.tab, typeof payload.section === 'string' ? payload.section : undefined);
  });

  const segments = useMemo(() => [
    ['models', Activity, 'bg-indigo-500/70'], ['images', ImageIcon, 'bg-sky-500/70'],
    ['database', Database, 'bg-emerald-500/70'], ['other', HardDrive, 'bg-amber-500/70'],
  ].map(([key, icon, color]) => ({ key, icon, color, label: t(`settings.storage.${key}`), bytes: c.storage?.[key + '_bytes'] })), [c.storage, t]);

  const restart = async () => {
    setRestarting(true);
    try {
      if (await c.handleRestartMonitor()) { runtime.clearCpuChanged(); runtime.clearDmlChanged(); }
    } finally { setRestarting(false); }
  };
  const page = (tab) => {
    switch (tab) {
      case 'general': return <>
        <LanguageSection />
        <MonitorServiceSection monitorStatus={c.monitorStatus} onStart={async () => {
          if (await c.handleStartMonitor()) { runtime.clearCpuChanged(); runtime.clearDmlChanged(); }
        }} onStop={c.handleStopMonitor}
          onPause={c.handlePauseMonitor} onResume={c.handleResumeMonitor} onRestart={restart}
          autoStartMonitor={autoStartMonitor} onAutoStartMonitorChange={onAutoStartMonitorChange}
          autoLaunchEnabled={c.autoLaunchEnabled} autoLaunchLoading={c.autoLaunchLoading} autoLaunchMessage={c.autoLaunchMessage}
          onToggleAutoLaunch={c.handleToggleAutoLaunch} powerSavingSuppressed={powerSavingSuppressed} />
        <GeneralOptionsSection powerSavingMode={powerSavingMode} onTogglePowerSaving={onPowerSavingModeChange} />
      </>;
      case 'capture': return <>
        <CaptureFiltersSection filterSettings={c.filterSettings} processInput={c.processInput} titleInput={c.titleInput}
          onProcessInputChange={c.setProcessInput} onTitleInputChange={c.setTitleInput}
          onAddProcess={c.addProcessTags} onAddTitle={c.addTitleTags} onRemoveProcess={c.removeProcessTag} onRemoveTitle={c.removeTitleTag}
          onToggleProtected={c.handleToggleProtected} onSave={c.handleSaveFilters} filtersDirty={c.filtersDirty}
          pendingApply={c.pendingApply} savingFilters={c.savingFilters} saveFiltersMessage={c.saveFiltersMessage} />
        <BrowserExtensionSection />
      </>;
      case 'organize': return <FeaturesSection monitorStatus={c.monitorStatus} />;
      case 'privacy': return <>
        <SecuritySection sessionTimeout={sessionTimeout} onSessionTimeoutChange={onSessionTimeoutChange}
          isSessionValid={isSessionValid} onLockSession={onLockSession} />
        <BackgroundSchedulerCard enabled={runtime.backgroundProcessingEnabled} saving={runtime.backgroundProcessingSaving}
          status={runtime.backgroundSchedulerStatus} onChange={runtime.handleBackgroundProcessingChange} onRefresh={runtime.refreshBackgroundSchedulerStatus} />
        <ProtectedProcessingCard backgroundEnabled={runtime.backgroundProcessingEnabled} />
        <SettingsSection title={t('settings.advanced.network.title')}>
          <SettingsGroup><SettingsRow label={t('settings.advanced.network.label')} description={t('settings.advanced.network.description')}
            control={<SettingsSwitch checked={runtime.config?.network_enabled === true} disabled={!runtime.config || runtime.configSaving}
              onChange={() => runtime.handleToggle('network_enabled')} />} /></SettingsGroup>
        </SettingsSection>
        <AiEmbeddingSection />
      </>;
      case 'maintenance': return <StorageManagementSection storageSegments={segments} totalStorage={c.storage?.total_bytes || 0}
        storage={c.storage} loading={c.analysisLoading} refreshing={c.analysisRefreshing} error={c.analysisError} onRefresh={c.handleRefreshAnalysis}
        maintenance={<IndexMaintenanceSection controller={runtime} />}
        cleanup={<QuickDeleteSection onDelete={c.handleQuickDelete} isDeleting={c.isDeleting} deleteMessage={c.deleteMessage} deleteMessageType={c.deleteMessageType} />} />;
      case 'advanced': return <AdvancedSection controller={runtime} />;
      case 'about': return <AboutSection checking={c.checkingUpdate} upToDate={c.upToDate} onCheckUpdate={c.handleCheckUpdate}
        updateInfo={c.updateInfo} updateError={c.updateError} downloading={c.downloading} downloadProgress={c.downloadProgress} onDownloadUpdate={c.handleDownloadUpdate} />;
      default: return null;
    }
  };
  return (
    <div className="flex h-full min-h-0 overflow-hidden bg-ide-bg">
      <nav ref={navigationRef} aria-label={t('settings.title')} className="flex w-48 shrink-0 flex-col gap-1 overflow-y-auto border-r border-ide-border bg-ide-panel p-3">
        {TABS.map(([id, Icon], index) => <button key={id} type="button" aria-current={activeTab === id ? 'page' : undefined} aria-controls={'settings-page-' + id}
          onClick={() => navigate(id)} onKeyDown={(event) => {
            const next = event.key === 'ArrowDown' ? (index + 1) % TABS.length : event.key === 'ArrowUp' ? (index + TABS.length - 1) % TABS.length : event.key === 'Home' ? 0 : event.key === 'End' ? TABS.length - 1 : null;
            if (next !== null) { event.preventDefault(); navigate(TABS[next][0]); navigationRef.current?.querySelectorAll('button')[next]?.focus(); }
          }}
          className={'flex items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm transition-colors ' + (activeTab === id ? 'bg-ide-accent/10 font-medium text-ide-accent' : 'text-ide-muted hover:bg-ide-hover hover:text-ide-text')}>
          <Icon className="h-4 w-4 shrink-0" aria-hidden="true" /><span className="min-w-0 leading-snug">{t(`settings.tabs.${id}`)}</span>
        </button>)}
      </nav>
      <div className="flex min-w-0 flex-1 flex-col overflow-hidden">
        {TABS.filter(([id]) => visited.has(id)).map(([id]) => <div key={id} id={'settings-page-' + id} hidden={activeTab !== id}
          className={(activeTab === id ? 'block' : 'hidden') + ' settings-surface min-h-0 flex-1 overflow-y-auto p-6'}
          ref={(element) => {
            if (element && id === activeTab && pendingSection.current) {
              const target = element.querySelector('[id="' + pendingSection.current.replace(/[^a-zA-Z0-9-]/g, '') + '"]');
              if (target) { target.scrollIntoView?.({ block: 'start' }); pendingSection.current = null; }
            }
          }}>
          <SettingsPageContext.Provider value={isOpen && activeTab === id}>
            <div className="mx-auto max-w-[760px] space-y-6">
              {id !== 'maintenance' && <div><h1 className="text-xl font-semibold">{t(`settings.tabs.${id}`)}</h1><p className="mt-1 text-xs leading-relaxed text-ide-muted">{t(`settings.pages.${id}`)}</p></div>}
              {page(id)}
            </div>
          </SettingsPageContext.Provider>
        </div>)}
        {(preferenceError || c.monitorError || (runtimeActive && (runtime.configError || runtime.runtimeError))) && <div className="shrink-0 px-6 py-3"><SettingsErrorBanner>
          {preferenceError || c.monitorError || runtime.configError || runtime.runtimeError}
          {runtime.configError && !runtime.config && <SettingsButton onClick={runtime.loadConfig}>{t('common.retry')}</SettingsButton>}
        </SettingsErrorBanner></div>}
        {(runtime.cpuChanged || runtime.dmlChanged) && <div className="shrink-0 border-t border-ide-border px-6 py-3">
          <SettingsWarningBanner><div className="flex flex-wrap items-center justify-between gap-3">
            <span>{t('settings.feedback.savedPendingRestart')}</span>
            <SettingsButton disabled={restarting || c.monitorStatus !== 'running'} onClick={restart}>{t('settings.advanced.quick_restart')}</SettingsButton>
          </div></SettingsWarningBanner>
        </div>}
      </div>
    </div>
  );
}
