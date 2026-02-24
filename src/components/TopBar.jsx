import React from 'react';
import { Moon, Sun, Settings, Bell, Terminal, Minus, Square, X, Copy, Loader2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { SearchBox } from './SearchBox';
import { APP_VERSION } from '../lib/version';

function ServiceStatusBadge({ backendStatus, monitorPaused, handleStartBackend, handlePauseMonitor, handleResumeMonitor }) {
  const { t } = useTranslation();

  let dotColor, label, onClick, disabled = false, showSpinner = false;

  if (backendStatus === 'online' && !monitorPaused) {
    dotColor = 'bg-green-500';
    label = t('topbar.service.running');
    onClick = handlePauseMonitor;
  } else if (backendStatus === 'online' && monitorPaused) {
    dotColor = 'bg-yellow-500';
    label = t('topbar.service.paused');
    onClick = handleResumeMonitor;
  } else if (backendStatus === 'waiting') {
    dotColor = 'bg-orange-500';
    label = t('topbar.service.starting');
    disabled = true;
    showSpinner = true;
    onClick = undefined;
  } else {
    dotColor = 'bg-red-500';
    label = t('topbar.service.offline');
    onClick = handleStartBackend;
  }

  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className="group flex items-center gap-1.5 p-1.5 rounded-full text-xs font-mono text-ide-muted hover:bg-ide-hover/50 transition-all disabled:opacity-60 disabled:cursor-not-allowed pointer-events-auto"
      title={label}
    >
      <span className={`w-2 h-2 rounded-full ${dotColor} shrink-0`} />
      <span className={`overflow-hidden transition-all duration-200 whitespace-nowrap ${backendStatus === 'online' ? 'max-w-0 opacity-0 group-hover:max-w-[6rem] group-hover:opacity-100' : 'max-w-[6rem] opacity-100'}`}>
        {showSpinner && <Loader2 className="w-3 h-3 animate-spin inline mr-1" />}
        {label}
      </span>
    </button>
  );
}

export default function TopBar({
  darkMode,
  setDarkMode,
  setShowSettings,
  showNotifications,
  setShowNotifications,
  isMaximized,
  onMinimize,
  onToggleMaximize,
  onHideToTray,
  onSearchSelect,
  onSearchSubmit,
  searchMode,
  onSearchModeChange,
  backendStatus,
  monitorPaused,
  handleStartBackend,
  handlePauseMonitor,
  handleResumeMonitor,
  backendOnline,
}) {
  const { t } = useTranslation();
  return (
    <header data-tauri-drag-region className="h-11 flex items-center justify-between px-4 shrink-0 select-none">
      <div className="flex items-center gap-4 pointer-events-none">
        <div className="flex items-center gap-2 text-ide-accent">
          <Terminal className="w-5 h-5" />
          <span className="font-bold tracking-tight">Carbon Paper</span>
        </div>
        <span className="px-3 py-1 rounded-full border border-ide-border text-xs font-mono text-ide-muted">
          {APP_VERSION}
        </span>
      </div>

      <SearchBox
        onSelectResult={onSearchSelect}
        onSubmit={onSearchSubmit}
        mode={searchMode}
        onModeChange={onSearchModeChange}
        backendOnline={backendOnline}
      />

      <div className="flex items-center gap-1">
        <ServiceStatusBadge
          backendStatus={backendStatus}
          monitorPaused={monitorPaused}
          handleStartBackend={handleStartBackend}
          handlePauseMonitor={handlePauseMonitor}
          handleResumeMonitor={handleResumeMonitor}
        />
        <button
          onClick={() => setShowSettings(true)}
          className="p-2 hover:bg-ide-hover rounded-md text-ide-muted hover:text-ide-text"
          title={t('topbar.settings')}
        >
          <Settings className="w-4 h-4" />
        </button>
        <button
          className="p-2 hover:bg-ide-hover rounded-md text-ide-muted hover:text-ide-text"
          onClick={() => setShowNotifications(!showNotifications)}
        >
          <Bell className="w-4 h-4" />
        </button>
        <button
          onClick={() => setDarkMode(!darkMode)}
          className="p-2 hover:bg-ide-hover rounded-md text-ide-muted hover:text-ide-text"
        >
          {darkMode ? <Sun className="w-4 h-4" /> : <Moon className="w-4 h-4" />}
        </button>

        <div className="w-px h-4 bg-ide-border mx-2"></div>

        <button onClick={onMinimize} className="p-2 hover:bg-ide-hover rounded-md text-ide-muted hover:text-ide-text">
          <Minus className="w-4 h-4" />
        </button>
        <button onClick={onToggleMaximize} className="p-2 hover:bg-ide-hover rounded-md text-ide-muted hover:text-ide-text">
          {isMaximized ? <Copy className="w-4 h-4 rotate-180" /> : <Square className="w-4 h-4" />}
        </button>
        <button onClick={onHideToTray} className="p-2 flex items-center justify-center hover:bg-red-500 hover:text-white rounded-md text-ide-muted transition-colors ml-1" title={t('topbar.hideToTray')}>
          <X className="w-5 h-5" />
        </button>
      </div>
    </header>
  );
}
