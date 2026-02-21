import React from 'react';
import { Moon, Sun, Settings, Bell, Terminal, Minus, Square, X, Copy } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { SearchBox } from './SearchBox';
import { APP_VERSION } from '../lib/version';

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
  onSearchModeChange
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
      />

      <div className="flex items-center gap-1">
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
