import React, { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { Copy, Globe, Loader2, RefreshCw } from 'lucide-react';
import { withAuth } from '../../lib/auth_api';
import { SettingsButton, SettingsSwitch } from './SettingsControls';
import { SettingsDisclosure, SettingsDivider, SettingsGroup, SettingsRow, SettingsSection, SettingsStatus } from './SettingsPrimitives';
import { useSettingsActive, useSettingsActivity } from './SettingsActivityContext';

export default function BrowserExtensionSection() {
  const { t } = useTranslation();
  const active = useSettingsActive();
  const [status, setStatus] = useState(null);
  const [enhanceEnabled, setEnhanceEnabled] = useState(null);
  const [sessions, setSessions] = useState(null);
  const [installing, setInstalling] = useState(null);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState(null);
  const [guideOpen, setGuideOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  const [readError, setReadError] = useState(false);
  const mounted = useRef(false);
  const operation = useRef(false);
  const request = useRef(0);
  const setupBrowser = useRef(null);
  useSettingsActivity('browser-extension', { busy: Boolean(installing) || saving });
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);

  const load = useCallback(async () => {
    const id = ++request.current;
    try {
      const [host, config, connected] = await Promise.all([
        invoke('get_nm_host_status'), invoke('get_extension_enhancement_config'), invoke('get_nmh_sessions'),
      ]);
      if (!mounted.current || id !== request.current) return;
      setStatus((current) => ({ ...host, extension_path: host.extension_path || current?.extension_path }));
      setReadError(false);
      setEnhanceEnabled(Boolean(config?.enabled));
      setSessions(Array.isArray(connected) ? connected : []);
      setMessage((current) => current?.key === 'settings.extension.readFailed' ? null : current);
      if (setupBrowser.current && (connected || []).some((session) => {
        const edge = (session.browser_exe_name || session.browser_exe_path || '').toLowerCase().includes('msedge');
        return setupBrowser.current === 'edge' ? edge : !edge;
      })) {
        setupBrowser.current = null;
        setGuideOpen(false);
        setMessage(null);
      }
    } catch (error) {
      if (mounted.current && id === request.current) { setReadError(true); setMessage({ tone: 'error', key: 'settings.extension.readFailed' }); }
    }
  }, [t]);
  useEffect(() => {
    if (!active) return undefined;
    let cancelled = false;
    let timer;
    const poll = async () => { await load(); if (!cancelled) timer = setTimeout(poll, 5000); };
    poll();
    return () => { cancelled = true; clearTimeout(timer); };
  }, [active, load]);

  const install = async (browser) => {
    if (operation.current) return;
    operation.current = true;
    setInstalling(browser);
    setMessage(null);
    try {
      const result = await withAuth(() => invoke('install_browser_extension', { browser }), { autoPrompt: true });
      if (!mounted.current) return;
      setStatus((current) => ({ ...current, [browser]: true, extension_path: result?.extension_path || current?.extension_path }));
      setGuideOpen(true);
      setupBrowser.current = browser;
      setMessage({ tone: 'success', key: 'settings.extension.prepared' });
      await load();
    } catch (error) {
      if (mounted.current) setMessage({ tone: 'error', key: 'settings.extension.error', values: { error: error?.message || String(error) } });
    } finally { operation.current = false; if (mounted.current) setInstalling(null); }
  };
  const toggle = async (enabled) => {
    if (operation.current) return;
    operation.current = true;
    setSaving(true);
    setMessage(null);
    try {
      await withAuth(() => invoke('set_extension_enhancement', { enabled }), { autoPrompt: true });
      if (mounted.current) setEnhanceEnabled(enabled);
    } catch (error) {
      if (mounted.current) setMessage({ tone: 'error', key: 'settings.feedback.saveFailed', values: { error: String(error) } });
    } finally { operation.current = false; if (mounted.current) setSaving(false); }
  };
  const connected = (browser) => sessions?.some((session) => {
    const name = (session.browser_exe_name || session.browser_exe_path || '').toLowerCase();
    return browser === 'edge' ? name.includes('msedge') : !name.includes('msedge');
  });

  return (
    <SettingsSection id="browser-extension" title={t('settings.extension.title')} icon={Globe}>
      <SettingsGroup>
        <SettingsRow label={t('settings.extension.enhance.global')} description={t('settings.extension.enhance.description')}
          control={<SettingsSwitch checked={enhanceEnabled === true} disabled={enhanceEnabled === null || saving || Boolean(installing)} onChange={toggle} />} />
        <SettingsDivider />
        <div className="space-y-4">
          {['chrome', 'edge'].map((browser) => (
            <SettingsRow key={browser} label={t(`settings.extension.${browser}.name`)}
              description={t(readError ? 'settings.extension.status.unavailable' : !status || sessions === null ? 'settings.extension.status.checking' : connected(browser) ? 'settings.extension.status.connected' : status[browser] ? 'settings.extension.status.configured' : 'settings.extension.status.not_configured')}
              control={<SettingsButton onClick={() => install(browser)} disabled={saving || Boolean(installing)}
                icon={installing === browser ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : undefined}>
                {t(installing === browser ? 'settings.extension.preparing' : status?.[browser] ? 'settings.extension.configureAgain' : 'settings.extension.setup')}
              </SettingsButton>} />
          ))}
        </div>
        {message && <div className="mt-4 flex items-center gap-3"><SettingsStatus tone={message.tone}>{t(message.key, message.values)}</SettingsStatus>
          {message.tone === 'error' && <SettingsButton variant="ghost" icon={RefreshCw} onClick={load}>{t('common.retry')}</SettingsButton>}</div>}
        <SettingsDivider />
        <SettingsDisclosure title={t('settings.extension.tutorial.title')} open={guideOpen} onOpenChange={setGuideOpen}>
          {status?.extension_path && <div className="space-y-2">
            <p className="text-xs font-medium">{t('settings.extension.folder')}</p>
            <div className="flex items-center gap-2 rounded-lg bg-ide-panel p-3">
              <code className="min-w-0 flex-1 break-all text-xs text-ide-muted">{status.extension_path}</code>
              <SettingsButton icon={Copy} onClick={async () => {
                try { await navigator.clipboard.writeText(status.extension_path); setCopied(true); }
                catch (error) { setMessage({ tone: 'error', key: 'settings.extension.copyFailed' }); }
              }}>{t(copied ? 'settings.extension.copied' : 'settings.extension.copyFolder')}</SettingsButton>
            </div>
          </div>}
          <ol className="list-inside list-decimal space-y-2 text-xs leading-relaxed text-ide-muted">
            {[1, 2, 3, 4].map((step) => <li key={step}>{t(`settings.extension.tutorial.step${step}`)}</li>)}
          </ol>
        </SettingsDisclosure>
        <SettingsDisclosure title={t('settings.extension.connectionDetails')}>
          {sessions?.length ? <ul className="space-y-2 text-xs text-ide-muted">{sessions.map((session) =>
            <li key={session.nmh_pid + '-' + session.cmd_pipe_name} className="break-all">{session.browser_exe_name || session.browser_exe_path} · {t('settings.extension.sessions.pid', { pid: session.browser_pid })}</li>)}</ul>
            : <p className="text-xs text-ide-muted">{t('settings.extension.sessions.empty')}</p>}
        </SettingsDisclosure>
      </SettingsGroup>
    </SettingsSection>
  );
}
