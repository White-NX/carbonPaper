import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, Globe, Check } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';

export default function ExtensionSetupWizard({ isVisible, onComplete }) {
  const { t } = useTranslation();
  const [selectedBrowsers, setSelectedBrowsers] = useState({ chrome: false, edge: false });
  const [installing, setInstalling] = useState(false);
  const [done, setDone] = useState(false);
  const [extensionPath, setExtensionPath] = useState('');
  const [error, setError] = useState('');

  const toggleBrowser = (browser) => {
    setSelectedBrowsers((prev) => ({ ...prev, [browser]: !prev[browser] }));
  };

  const handleSkip = async () => {
    try { await invoke('mark_extension_setup_done'); } catch {}
    onComplete?.();
  };

  const handleInstall = async () => {
    const browsers = Object.entries(selectedBrowsers)
      .filter(([, v]) => v)
      .map(([k]) => k);
    if (browsers.length === 0) return;

    setInstalling(true);
    setError('');
    try {
      let lastPath = '';
      for (const browser of browsers) {
        const result = await invoke('install_browser_extension', { browser });
        if (result?.extension_path) lastPath = result.extension_path;
      }
      setExtensionPath(lastPath);
      setDone(true);
    } catch (err) {
      console.error('Extension install failed:', err);
      setError(String(err?.message || err));
    } finally {
      setInstalling(false);
    }
  };

  const handleDone = async () => {
    try { await invoke('mark_extension_setup_done'); } catch {}
    onComplete?.();
  };

  if (!isVisible) return null;

  const anySelected = selectedBrowsers.chrome || selectedBrowsers.edge;

  return (
    <div className="absolute inset-0 z-50 flex flex-col items-center justify-center bg-ide-bg/80 backdrop-blur-sm text-ide-muted">
      <div className="w-full max-w-lg bg-ide-panel border border-ide-border rounded-xl p-6 shadow-2xl">
        {/* Header */}
        <div className="flex items-center gap-3 mb-1">
          <div className="w-10 h-10 rounded-lg bg-ide-bg border border-ide-border flex items-center justify-center">
            <Globe className="w-5 h-5 text-ide-accent" />
          </div>
          <div>
            <h2 className="text-lg font-semibold text-ide-text">{t('extensionSetup.title')}</h2>
            <p className="text-xs text-ide-muted">{t('extensionSetup.subtitle')}</p>
          </div>
        </div>

        <p className="text-xs text-ide-muted mt-4 mb-3">{t('extensionSetup.description')}</p>

        {done ? (
          <>
            <div className="p-3 bg-green-500/10 border border-green-500/30 rounded-lg text-sm text-green-400 space-y-1">
              <div className="flex items-center gap-2">
                <Check className="w-4 h-4" />
                <span>{t('extensionSetup.success_message')}</span>
              </div>
              {extensionPath && (
                <p className="text-xs text-green-400/70 break-all ml-6">{extensionPath}</p>
              )}
            </div>
            <div className="mt-5 flex justify-end">
              <button
                onClick={handleDone}
                className="px-4 py-1.5 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-sm font-medium transition-colors"
              >
                {t('extensionSetup.done')}
              </button>
            </div>
          </>
        ) : (
          <>
            {/* Browser selection */}
            <p className="text-xs text-ide-muted mb-2">{t('extensionSetup.select_browser')}</p>
            <div className="bg-ide-bg rounded-lg border border-ide-border overflow-hidden">
              {['chrome', 'edge'].map((browser, idx) => (
                <label
                  key={browser}
                  className={`flex items-center gap-3 px-3 py-2.5 cursor-pointer transition-colors hover:bg-ide-bg/50 ${
                    selectedBrowsers[browser] ? 'bg-ide-accent/10' : ''
                  } ${idx > 0 ? 'border-t border-ide-border' : ''}`}
                >
                  <input
                    type="checkbox"
                    checked={selectedBrowsers[browser]}
                    onChange={() => toggleBrowser(browser)}
                    className="accent-ide-accent"
                  />
                  <Globe className="w-4 h-4 text-ide-muted/70 shrink-0" />
                  <span className="text-sm text-ide-text flex-1">{t(`extensionSetup.${browser}`)}</span>
                </label>
              ))}
            </div>

            {error && (
              <div className="mt-3 text-xs px-3 py-2 rounded bg-red-500/10 text-red-400">
                {t('extensionSetup.error_message')}: {error}
              </div>
            )}

            {/* Actions */}
            <div className="mt-5 flex items-center justify-end gap-2">
              <button
                onClick={handleSkip}
                disabled={installing}
                className="px-4 py-1.5 bg-ide-bg hover:bg-ide-bg/80 text-ide-muted border border-ide-border rounded text-sm transition-colors disabled:opacity-50"
              >
                {t('extensionSetup.skip')}
              </button>
              <button
                onClick={handleInstall}
                disabled={installing || !anySelected}
                className="px-4 py-1.5 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-sm font-medium transition-colors disabled:opacity-50 flex items-center gap-1.5"
              >
                {installing && <Loader2 className="w-3.5 h-3.5 animate-spin" />}
                {installing ? t('extensionSetup.installing') : t('extensionSetup.apply')}
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
