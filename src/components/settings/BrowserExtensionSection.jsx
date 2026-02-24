import React, { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { Globe } from 'lucide-react';

export default function BrowserExtensionSection() {
  const { t } = useTranslation();
  const [status, setStatus] = useState({ chrome: false, edge: false });
  const [enhance, setEnhance] = useState({ chrome: false, edge: false });
  const [installing, setInstalling] = useState(null); // 'chrome' | 'edge' | null
  const [message, setMessage] = useState('');
  const [messageType, setMessageType] = useState(''); // 'success' | 'error'

  const checkStatus = async () => {
    try {
      const result = await invoke('get_nm_host_status');
      setStatus(result);
    } catch (e) {
      console.error('Failed to check NM host status:', e);
    }
  };

  const loadEnhanceConfig = async () => {
    try {
      const result = await invoke('get_extension_enhancement_config');
      setEnhance(result);
    } catch (e) {
      console.error('Failed to load extension enhancement config:', e);
    }
  };

  useEffect(() => {
    checkStatus();
    loadEnhanceConfig();
  }, []);

  const handleInstall = async (browser) => {
    setInstalling(browser);
    setMessage('');
    try {
      await invoke('install_browser_extension', { browser });
      setMessage(t('settings.extension.success'));
      setMessageType('success');
      await checkStatus();
    } catch (e) {
      setMessage(t('settings.extension.error', { error: e?.message || String(e) }));
      setMessageType('error');
    } finally {
      setInstalling(null);
    }
  };

  const handleEnhanceToggle = async (browser, enabled) => {
    try {
      await invoke('set_extension_enhancement', { browser, enabled });
      setEnhance(prev => ({ ...prev, [browser]: enabled }));
    } catch (e) {
      console.error('Failed to set extension enhancement:', e);
    }
  };

  const renderBrowser = (browser, label) => (
    <div className="px-4 py-3 rounded-lg bg-ide-panel border border-ide-border space-y-2">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <Globe className="w-5 h-5 text-ide-text-secondary" />
          <div>
            <div className="text-sm font-medium">{label}</div>
            <div className={`text-xs ${status[browser] ? 'text-ide-info-success' : 'text-ide-text-secondary'}`}>
              {status[browser] ? t('settings.extension.status.registered') : t('settings.extension.status.not_registered')}
            </div>
          </div>
        </div>
        <button
          onClick={() => handleInstall(browser)}
          disabled={installing !== null}
          className={`px-3 py-1.5 rounded text-xs font-medium transition-colors ${status[browser]
              ? 'bg-green-500/20 text-ide-info-success border border-green-500/30'
              : 'bg-ide-accent text-white hover:bg-ide-accent/80'
            } disabled:opacity-50`}
        >
          {installing === browser
            ? t(`settings.extension.${browser}.registering`)
            : status[browser]
              ? t(`settings.extension.${browser}.registered`)
              : t(`settings.extension.${browser}.label`)}
        </button>
      </div>

      {/* Enhancement toggle */}
      {status[browser] && (
        <div className="flex items-center justify-between pt-1 border-t border-ide-border">
          <span className="text-xs text-ide-text-secondary">
            {t(`settings.extension.enhance.${browser}`)}
          </span>
          <button
            onClick={() => handleEnhanceToggle(browser, !enhance[browser])}
            className={`relative w-10 h-5 rounded-full transition-colors shrink-0 ${enhance[browser] ? 'bg-ide-accent' : 'bg-ide-border'
              }`}
            title={t(`settings.extension.enhance.${browser}`)}
          >
            <div
              className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${
                enhance[browser] ? 'translate-x-5' : 'translate-x-0.5'
              }`}
            />
          </button>
        </div>
      )}
    </div>
  );

  return (
    <div className="space-y-4">
      <div className="space-y-1">
        <h2 className="text-xl font-semibold">{t('settings.extension.title')} <span className="px-1 py-0.5 bg-amber-500/20 text-amber-400 text-[10px] rounded">alpha</span></h2>
        <p className="text-xs text-ide-muted">{t('settings.extension.description')}</p>
      </div>

      <div className="space-y-3">
        {renderBrowser('chrome', 'Chrome')}
        {renderBrowser('edge', 'Edge')}
      </div>

      {(enhance.chrome || enhance.edge) && (
        <div className="text-xs text-ide-text-secondary bg-ide-panel/50 px-3 py-2 rounded border border-ide-border/50">
          {t('settings.extension.enhance.description')}
        </div>
      )}

      {message && (
        <div className={`text-xs px-3 py-2 rounded ${messageType === 'success' ? 'bg-green-500/10 text-ide-info-success' : 'bg-red-500/10 text-ide-error'
          }`}>
          {message}
        </div>
      )}

      {/* Tutorial */}
      <div className="bg-ide-bg rounded-lg border border-ide-border p-3">
        <h4 className="text-xs font-semibold text-ide-text mb-2">{t('settings.extension.tutorial.title')}</h4>
        <ol className="text-xs text-ide-text-secondary space-y-1.5 list-decimal list-inside">
          <li>{t('settings.extension.tutorial.step1')}</li>
          <li>{t('settings.extension.tutorial.step2')}</li>
          <li>{t('settings.extension.tutorial.step3')}</li>
          <li>{t('settings.extension.tutorial.step4')}</li>
        </ol>
      </div>
    </div>
  );
}
