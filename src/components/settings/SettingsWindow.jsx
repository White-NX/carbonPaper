import React, { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Maximize2, Minus, Settings, X } from 'lucide-react';
import { withAuth, initAuthListeners } from '../../lib/auth_api';
import { notifySettingsChanged } from '../../lib/settings_api';
import { useAppTheme } from '../../hooks/useAppTheme';
import { usePowerSavingState } from '../../hooks/useAppWindowState';
import { useAuthSession } from '../../hooks/useAuthSession';
import { useTauriEventListener } from '../../hooks/useTauriEventListener';
import { DialogVisibilityContext } from '../../hooks/useDialogFocus';
import AuthMask from '../AuthMask';
import { ConfirmDialog } from '../ConfirmDialog';
import SettingsContent from './SettingsContent';
import { SettingsActivityProvider, useSettingsActivity, useSettingsWindowActivity } from './SettingsActivityContext';
import { SettingsErrorBanner } from './SettingsPrimitives';

function SettingsWindowFrame({ auth }) {
  const { t } = useTranslation();
  const { dirty, busy } = useSettingsWindowActivity();
  const [closeIntent, setCloseIntent] = useState(null);
  const [closeMessage, setCloseMessage] = useState('');
  const [preferenceError, setPreferenceError] = useState('');
  const [saving, setSaving] = useState(false);
  const [autoStartMonitor, setAutoStartMonitor] = useState(null);
  const [visited, setVisited] = useState(false);
  const appWindow = getCurrentWindow();
  const { powerSavingMode, powerSavingSuppressed, setPowerSavingMode } = usePowerSavingState();
  useSettingsActivity('window-preference', { busy: saving });

  useEffect(() => {
    if (auth.isAuthenticated) setVisited(true);
  }, [auth.isAuthenticated]);
  useEffect(() => {
    appWindow.setTitle(t('settings.window.title')).catch(console.warn);
  }, [t]);
  useEffect(() => {
    invoke('settings_set_busy', { busy }).catch(console.warn);
  }, [busy]);
  useEffect(() => {
    invoke('get_monitor_autostart').then((value) => setAutoStartMonitor(Boolean(value)))
      .catch(() => setPreferenceError(t('settings.feedback.readFailed')));
  }, []);

  const close = useCallback(async (lightweight) => {
    try {
      await invoke('settings_set_busy', { busy: false });
      await invoke('close_settings_window', { lightweight });
    } catch (error) { setCloseMessage(String(error)); }
  }, []);
  const requestClose = useCallback((lightweight = false) => {
    if (busy) { setCloseMessage(t('settings.window.operationInProgress')); return; }
    if (dirty) { setCloseIntent({ lightweight }); return; }
    close(lightweight);
  }, [busy, dirty, close, t]);
  useTauriEventListener('settings-close-requested', ({ payload }) => requestClose(payload === true));

  const changeAutoStart = async (enabled) => {
    if (saving) return;
    setSaving(true);
    setPreferenceError('');
    try {
      await withAuth(() => invoke('set_monitor_autostart', { enabled }), { autoPrompt: true });
      setAutoStartMonitor(enabled);
      localStorage.setItem('autoStartMonitor', String(enabled));
      await notifySettingsChanged(['autostart']);
    } catch (error) { setPreferenceError(t('settings.feedback.saveFailed', { error: String(error) })); }
    finally { setSaving(false); }
  };

  return (
    <div className="flex h-screen flex-col overflow-hidden bg-ide-bg text-ide-text">
      <header className="flex h-11 shrink-0 select-none items-center gap-2 border-b border-ide-border bg-ide-panel pl-4">
        <div className="flex min-w-0 flex-1 items-center gap-2 self-stretch" data-tauri-drag-region
          onDoubleClick={() => appWindow.toggleMaximize().catch(console.warn)}
          onMouseDown={(event) => { if (event.button === 0 && event.detail === 1) appWindow.startDragging().catch(console.warn); }}>
          <Settings className="h-4 w-4 text-ide-accent" aria-hidden="true" />
          <span className="text-sm font-medium">{t('settings.window.title')}</span>
        </div>
        <div className="flex self-stretch">
          <button type="button" className="px-3 text-ide-muted hover:bg-ide-hover" aria-label={t('settings.window.minimize')} onClick={() => appWindow.minimize()}><Minus className="h-4 w-4" /></button>
          <button type="button" className="px-3 text-ide-muted hover:bg-ide-hover" aria-label={t('settings.window.maximize')} onClick={() => appWindow.toggleMaximize()}><Maximize2 className="h-3.5 w-3.5" /></button>
          <button type="button" className="px-4 text-ide-muted hover:bg-red-600 hover:text-white" aria-label={t('common.close')} onClick={() => requestClose()}><X className="h-4 w-4" /></button>
        </div>
      </header>
      <div className="relative min-h-0 flex-1">
        {(visited || auth.isAuthenticated) && (
          <div className={auth.isAuthenticated ? 'h-full' : 'hidden'} hidden={!auth.isAuthenticated} aria-hidden={!auth.isAuthenticated}>
            <SettingsContent isOpen={auth.isAuthenticated} autoStartMonitor={autoStartMonitor}
              onAutoStartMonitorChange={changeAutoStart} preferenceError={preferenceError}
              onRecordsDeleted={() => notifySettingsChanged(['records'])}
              sessionTimeout={auth.sessionTimeout} onSessionTimeoutChange={auth.setSessionTimeout}
              isSessionValid={auth.isAuthenticated} onLockSession={auth.handleLockSession}
              powerSavingSuppressed={powerSavingSuppressed} powerSavingMode={powerSavingMode}
              onPowerSavingModeChange={setPowerSavingMode} />
          </div>
        )}
        {!auth.isAuthenticated && <div className="absolute inset-0 bg-ide-bg"><AuthMask isVisible onAuthSuccess={auth.handleAuthSuccess} authError={auth.authError} setAuthError={auth.setAuthError} /></div>}
      </div>
      {closeMessage && <div className="shrink-0 border-t border-ide-border p-3"><SettingsErrorBanner>{closeMessage}</SettingsErrorBanner></div>}
      <ConfirmDialog allowWhenLocked isOpen={Boolean(closeIntent)} title={t('settings.window.unsavedTitle')}
        message={t('settings.window.unsavedDescription')} confirmLabel={t('settings.window.discardAndClose')}
        cancelLabel={t('settings.window.keepEditing')} confirmVariant="danger"
        onConfirm={() => { const intent = closeIntent; setCloseIntent(null); close(intent.lightweight); }}
        onCancel={() => setCloseIntent(null)} />
    </div>
  );
}

export default function SettingsWindow() {
  useAppTheme();
  useEffect(() => initAuthListeners(), []);
  const auth = useAuthSession();
  return <SettingsActivityProvider enabled={auth.isAuthenticated}><DialogVisibilityContext.Provider value={auth.isAuthenticated}><SettingsWindowFrame auth={auth} /></DialogVisibilityContext.Provider></SettingsActivityProvider>;
}
